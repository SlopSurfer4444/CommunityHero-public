import { CliError, checkpointError, localAdmissionPayloadHash } from './client.mjs';

const failures = new WeakMap();
const exact = (value, keys) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
const positive = value => Number.isSafeInteger(value) && value > 0;
const hash = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
const id = value => typeof value === 'string' && /^[A-Za-z0-9_-]{1,160}$/u.test(value);
const invalid = () => { throw new CliError('Conductor connection observation differs from its native identity', { code: 'INVALID_CONDUCTOR_CONFIG' }); };
const bindingKeys = ['id', 'workspaceId', 'accountId', 'connector', 'revision', 'providerAccountId'];
export function validateConnectionBinding(value, account) {
  if (!exact(value, bindingKeys) || !positive(value.revision)
    || !['angryspace', 'vk', 'instagram', 'youtube', 'tiktok'].includes(value.connector)
    || !bindingKeys.filter(key => key !== 'revision').every(key => typeof value[key] === 'string' && value[key].trim() && value[key].length <= 4096)
    || value.workspaceId !== 'local-pilot' || value.accountId !== account) invalid();
  return value;
}
const same = (left, right) => localAdmissionPayloadHash({ value: left }) === localAdmissionPayloadHash({ value: right });
export function validateConnectionDependency(value, config, negative = undefined) {
  if (!exact(value, ['version', 'kind', 'runId', 'leaseGeneration', 'connectionBinding', 'gateObservation', 'executeRejection'])
    || value.version !== 1 || value.kind !== 'conductor-connection-dependency'
    || value.runId !== config.runId || value.leaseGeneration !== config.leaseGeneration
    || !same(validateConnectionBinding(value.connectionBinding, config.account), config.connectionBinding)) invalid();
  const gate = value.gateObservation;
  if (gate?.kind === 'missing') { if (!exact(gate, ['kind'])) invalid(); }
  else {
    const owner = gate?.owner;
    if (!exact(gate, ['kind', 'gateEpoch', 'owner', 'connectionBinding', 'state', 'availabilityState'])
      || gate.kind !== 'present' || !positive(gate.gateEpoch)
      || !['open', 'closing', 'blocked'].includes(gate.state)
      || !['ready', 'blocked', 'recovering', 'needs_owner', 'unverified'].includes(gate.availabilityState)
      || !same(validateConnectionBinding(gate.connectionBinding, config.account), config.connectionBinding)
      || !exact(owner, ['account', 'runtimeId', 'releaseSha256', 'epoch']) || owner.account !== config.account
      || !/^[A-Za-z0-9_-]{1,80}$/u.test(owner.runtimeId || '') || !hash(owner.releaseSha256) || !positive(owner.epoch)) invalid();
  }
  const rejection = value.executeRejection;
  if (rejection !== null && (!exact(rejection, ['kind', 'requestId', 'approvalId', 'payloadHash', 'evaluationId', 'receiptSha256'])
    || rejection.kind !== 'execute-rejection' || !id(rejection.requestId) || !id(rejection.approvalId)
    || !id(rejection.evaluationId) || !hash(rejection.payloadHash) || !hash(rejection.receiptSha256))) invalid();
  if (negative === null && rejection !== null || negative && (!rejection
    || ['requestId', 'approvalId', 'payloadHash', 'evaluationId', 'receiptSha256'].some(key => rejection[key] !== negative[key]))) invalid();
  return structuredClone(value);
}

// Only this process's typed private transport may establish a before-POST
// boundary. An error code, HTTP body or persisted boolean cannot establish it.
export function connectionWait(dependency, { beforeMutationPath = null, rejectionError = null } = {}) {
  const error = new CliError('Original conductor is waiting for its connection', { code: 'CONDUCTOR_CONNECTION_DEPENDENCY' });
  failures.set(error, { dependency, beforeMutationPath, rejectionError }); return error;
}
export const connectionFailure = error => failures.get(error) || null;
export async function checkConnection(client) {
  if (typeof client.assertConnectionReady === 'function') await client.assertConnectionReady();
}

export function connectionWaitState(state, error) {
  const failure = connectionFailure(error); if (!failure) throw error;
  // Only the existing exact durable rejection parser plus the same-run native
  // dependency read may resolve lost-ACK UNKNOWN into a known no-attempt.
  if (!failure.rejectionError && (state.phase === 'unknown' || state.error?.code === 'UNKNOWN_MUTATION_OUTCOME'))
    throw new CliError('Original mutation remains unconfirmed', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const next = { ...state, connectionDependency: failure.dependency };
  if (failure.rejectionError) { next.phase = 'stopped'; next.error = checkpointError(failure.rejectionError); }
  const pending = state.pendingLocalAdmission;
  const paths = { prepare: '/api/engine/prepare', approval: '/api/approvals', execute: `/api/approvals/${encodeURIComponent(state.approvalId)}/execute` };
  if (pending && failure.beforeMutationPath === paths[pending.kind] && state.phase === `${pending.kind}-admitting`) {
    next.connectionUnpostedIntent = { ...pending, path: failure.beforeMutationPath };
    next.pendingLocalAdmission = null;
    next.phase = { prepare: 'starting', approval: 'editorial-reviewed', execute: 'approved' }[pending.kind];
  }
  return next;
}
export function verifyUnpostedIntent(state, kind, requestId, payloadHash) {
  const intent = state.connectionUnpostedIntent;
  if (intent && (intent.kind !== kind || intent.requestId !== requestId || intent.payloadHash !== payloadHash))
    throw new CliError('Original unposted admission scope changed', { code: 'INVALID_CHECKPOINT' });
}
export function hasUnconfirmedAdmission(state) {
  if (!state || typeof state !== 'object') return false;
  return state.phase === 'unknown' || state.error?.code === 'UNKNOWN_MUTATION_OUTCOME'
    || ['slices', 'readyBatches'].some(key => (state[key] || []).some(hasUnconfirmedAdmission))
    || state.child && hasUnconfirmedAdmission(state.child);
}

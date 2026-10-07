import { CliError, checkpointError, waitForPoll } from './client.mjs';

export const PREPARE_REVIEW_ONLY = 'prepare_review_only';
export const READY_FOR_OWNER_APPROVAL = 'ready_for_owner_approval';
export async function bindWorkflowGeneration(client, state, { resuming = false } = {}) {
  if (typeof client.pinWorkspaceGeneration !== 'function') return state;
  // An old intent without a generation belongs to the legacy database episode.
  // Never attach it to the next pristine episode based on a fresh server read.
  if (resuming) client.pinWorkspaceGeneration(Object.hasOwn(state, 'workflowGeneration') ? state.workflowGeneration : null);
  const workflowGeneration = await client.getWorkspaceGeneration();
  return { ...state, workflowGeneration };
}
const reads = ['getJob', 'bootstrap', 'reviewItems', 'operatorProgress'];
const transient = error => ['NETWORK_ERROR', 'READ_TIMEOUT'].includes(error?.code)
  || error?.code === 'INVALID_RESPONSE' && (error.status == null || error.status === 200 || error.status >= 500)
  || error?.code === 'HTTP_ERROR' && [500, 502, 503, 504].includes(error.status);

// One finite budget for this observation session. Only these named read methods
// are wrapped; admissions and ready callbacks always run outside the retry scope.
export function createReadObserver(client, { enabled = true, signal, pollMs = 1000,
  observation = {}, onObservation = async () => {} } = {}) {
  let remaining = enabled ? 2 : 0;
  const state = { version: 1, state: 'observing', lastObservedAt: null,
    lastDurableOutcomeAt: null, retryCount: 0, errorCode: null, httpStatus: null,
    phase: null, coverage: null, ...observation };
  const observed = Object.create(client);
  observed.observation = state;
  for (const method of reads) {
    if (typeof client[method] !== 'function') continue;
    observed[method] = async (...args) => {
      for (;;) {
        if (signal?.aborted) throw new CliError('Observation stopped', { code: 'STOPPED' });
        let value;
        try { value = await client[method](...args); }
        catch (error) {
          if (signal?.aborted) error = new CliError('Observation stopped', { code: 'STOPPED' });
          const safe = checkpointError(error);
          state.errorCode = safe.code; state.httpStatus = safe.status ?? null;
          state.phase = method; state.state = signal?.aborted ? 'stopped' : transient(error) ? 'disconnected' : 'blocked';
          if (!transient(error) || remaining === 0 || signal?.aborted) {
            if (transient(error) && !signal?.aborted) state.state = 'exhausted';
            await onObservation({ ...state }); throw error;
          }
          remaining--; state.retryCount++;
          await onObservation({ ...state });
          try { await waitForPoll(Math.min(pollMs, 1000), signal); }
          catch (stopped) { state.state = 'stopped'; state.errorCode = 'STOPPED'; await onObservation({ ...state }); throw stopped; }
          continue;
        }
        state.state = 'observing'; state.lastObservedAt = new Date().toISOString();
        state.errorCode = null; state.httpStatus = null; state.phase = method;
        await onObservation({ ...state });
        return value;
      }
    };
  }
  return observed;
}

export function workflowMode(saved, requested, { execute = false, autonomous = false } = {}) {
  const mode = requested ?? saved?.workflowMode;
  if (mode !== undefined && mode !== PREPARE_REVIEW_ONLY)
    throw new CliError('Unsupported workflow mode', { code: 'USAGE' });
  if (saved && requested !== undefined && saved.workflowMode !== requested)
    throw new CliError('Resume workflow mode differs', { code: 'INVALID_CHECKPOINT' });
  if (mode === PREPARE_REVIEW_ONLY && (execute || autonomous))
    throw new CliError('Prepare-review-only cannot grant approval or execution', { code: 'USAGE' });
  if (mode === PREPARE_REVIEW_ONLY && saved) validatePrepareOnlyState(saved);
  return mode;
}

export function validatePrepareOnlyState(state) {
  if (state.approvalId || state.approvalRequestId || state.admission || state.executeAttemptId || state.executeJobId || state.executeRequestId
    || state.pendingLocalAdmission && !['prepare', 'editorial'].includes(state.pendingLocalAdmission.kind))
    throw new CliError('Prepare-review-only checkpoint contains effect authority', { code: 'INVALID_CHECKPOINT' });
  for (const child of [...(state.slices || []), ...(state.readyBatches || []), ...(state.child ? [state.child] : [])])
    validatePrepareOnlyState(child);
}

// This is the existing native ledger projection, with absent counters preserved
// as null. It never adds observer counts to native or historical operations.
export function nativeProgress(value) {
  if (!value || value.version !== 1 || typeof value !== 'object' || Array.isArray(value))
    return { version: 1, available: false, counters: null, reason: 'native-progress-unavailable' };
  const counters = Object.fromEntries(Object.entries(value.counters || {}).map(([key, count]) => [key,
    Number.isSafeInteger(count) && count >= 0 ? count : null]));
  return { ...value, counters: value.counters == null ? null : counters };
}

export function settledEditorialProgress(value, references) {
  const key = ref => JSON.stringify([ref?.id, ref?.revision]);
  const expected = new Set(references.map(key)); const seen = new Set();
  if (value?.version !== 1 || value.contract !== 'communityhero-editorial-progress-v1'
    || value.approvalRequired !== true || value.dispatchAuthorized !== false || value.retryAllowed !== false
    || !['accepted', 'reused', 'held', 'pending', 'readyForOwnerApproval'].every(field => Array.isArray(value[field])))
    throw new CliError('Editorial progress contract differs', { code: 'INVALID_EDITORIAL_REVIEW' });
  for (const ref of [...value.accepted, ...value.held.map(row => row.reference), ...value.pending]) {
    if (!expected.has(key(ref)) || seen.has(key(ref))) throw new CliError('Editorial progress scope differs', { code: 'INVALID_EDITORIAL_REVIEW' });
    seen.add(key(ref));
  }
  if (seen.size !== expected.size || value.reused.some(ref => !value.accepted.some(row => key(row) === key(ref)))
    || new Set(value.readyForOwnerApproval.map(key)).size !== value.readyForOwnerApproval.length
    || value.readyForOwnerApproval.some(ref => !value.accepted.some(row => key(row) === key(ref))
      || ref.itemId !== references.find(row => key(row) === key(ref))?.itemId
      || !/^[a-f0-9]{64}$/u.test(ref.textSha256 || '') || !/^[a-f0-9]{64}$/u.test(ref.receiptSha256 || ''))
    || typeof value.complete !== 'boolean' || value.complete && value.pending.length
    || !Number.isSafeInteger(value.completedUnits) || !Number.isSafeInteger(value.totalUnits)
    || value.completedUnits < 0 || value.completedUnits > value.totalUnits)
    throw new CliError('Editorial progress partition differs', { code: 'INVALID_EDITORIAL_REVIEW' });
  return value;
}

export function currentReadyReferences(snapshot, proposals, accepted, prior) {
  const current = snapshot?.readyForOwnerApproval;
  if (!Array.isArray(current) || accepted.some(ref => current.filter(row => row.id === ref.id && row.revision === ref.revision
    && /^[a-f0-9]{64}$/u.test(row.receiptSha256 || '') && /^[a-f0-9]{64}$/u.test(row.textSha256 || '')
    && proposals.some(proposal => proposal.id === row.id && proposal.itemId === row.itemId)).length !== 1))
    throw new CliError('Current exact editorial-ready receipts are unavailable', { code: 'INVALID_REVIEW_COVERAGE' });
  const ready = accepted.map(ref => {
    const row = current.find(row => row.id === ref.id && row.revision === ref.revision);
    return Object.fromEntries(['id', 'revision', 'itemId', 'textSha256', 'receiptSha256'].map(field => [field, row[field]]));
  });
  if (prior && JSON.stringify(prior) !== JSON.stringify(ready))
    throw new CliError('Current editorial receipt differs from saved readiness', { code: 'STALE_OR_CONFLICT' });
  return ready;
}

export function resultExitCode(result) {
  const state = result?.checkpoint || result;
  if (state?.stopReason === 'operator-stopped' || state?.error?.code === 'STOPPED') return 130;
  const slices = state?.slices || [];
  const summary = result?.summary || result?.counts || {};
  if (['unknown', 'needs-reconciliation', 'executing', 'stopped', 'incomplete', 'blocked'].includes(result?.mode)
    && !['operation-failures', 'execution-job-failed', 'prepare-job-failed'].includes(state?.stopReason)
    || summary.unknown > 0 || summary.unresolvedSlices?.length
    || slices.some(slice => ['unknown', 'needs-reconciliation'].includes(slice.phase)
      || slice.observation && ['exhausted', 'disconnected', 'blocked'].includes(slice.observation.state))) return 4;
  if (['completed-with-failures', 'prepare-partial-failed', 'prepare-failed'].includes(result?.mode)
    || summary.failed > 0 || summary.stale > 0
    || ['operation-failures', 'execution-job-failed', 'prepare-job-failed'].includes(state?.stopReason)) return 1;
  return 0;
}

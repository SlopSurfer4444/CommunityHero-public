import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { homedir } from 'node:os';
import { CliError, UnknownMutationError, checkpointError, localAdmissionPayloadHash, settleMaterialsImport, waitForJob,
  executeAdmissionPath, executeAdmissionResult, recoverExecuteAdmission, waitForPoll } from './client.mjs';
import { EDITORIAL_PATH, editorialReferences, settleEditorialReview, settleEditorialRepair, repairExpected, verifyRepairChain } from './editorial.mjs';
import { discoverReadyProposals, drainReadyBatches, createReadyBatchCoordinator, confirmedExecutionClosure, readyBatchStopped } from './ready-batches.mjs';
import { bindWorkflowGeneration, createReadObserver, currentReadyReferences, nativeProgress, settledEditorialProgress, workflowMode as resolveWorkflowMode, PREPARE_REVIEW_ONLY, READY_FOR_OWNER_APPROVAL } from './read-observer.mjs';
import { checkConnection, connectionFailure, connectionWaitState, verifyUnpostedIntent } from './conductor-connection.mjs';

const VERSION = 1;
export function sameFlowPolicy(saved, expected) {
  const fields = ['freshEditorial', 'maxRepairRounds', 'continueHeld'];
  return saved && typeof saved === 'object' && !Array.isArray(saved) && Object.keys(saved).length === fields.length
    && Object.keys(saved).every(key => fields.includes(key)) && fields.every(key => saved[key] === expected[key]);
}
function checkAdmissionSignal(signal) {
  if (signal?.aborted) throw new CliError('Stopped before a new admission; resume the saved checkpoint', { code: 'STOPPED' });
}
export function hasPrepareScopeReservation(jobId, proof) {
  return proof?.version === 1 && proof.ownerJobId === jobId && typeof proof.keysDigest === 'string' && /^[a-f0-9]{64}$/u.test(proof.keysDigest);
}
// Only pure reads of an acknowledged preparation use this budget. Keeping
// callbacks outside it prevents replaying a ready child's paid/effect work.
function preparationReadObserver(client, options) {
  return createReadObserver(client, options);
}
export const DEFAULT_INSTRUCTION = 'Подготовь безопасные предложения для выбранных комментариев. Для каждого выбери ответ и закрытие, закрытие без ответа или явно объясни, почему нужно участие человека. Ничего не публикуй.';

export async function readCheckpoint(path) {
  let value;
  try { value = JSON.parse(await readFile(path, 'utf8')); }
  catch (error) { throw new CliError(`Cannot read checkpoint ${path}`, { code: 'INVALID_CHECKPOINT', details: { cause: error.code || error.name } }); }
  if (value?.schemaVersion !== VERSION) throw new CliError('Unsupported checkpoint version', { code: 'INVALID_CHECKPOINT' });
  return value;
}

export async function writeCheckpoint(path, value) {
  const target = resolve(path); await mkdir(dirname(target), { recursive: true });
  const tmp = `${target}.${process.pid}.${randomUUID()}.tmp`; const next = { ...value, schemaVersion: VERSION, updatedAt: new Date().toISOString() };
  await writeFile(tmp, `${JSON.stringify(next, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  await replaceCheckpointFile(tmp, target); return next;
}

// Windows readers can briefly deny replacement. Retry only the same atomic
// rename; never regenerate the checkpoint or repeat its associated admission.
export async function replaceCheckpointFile(tmp, target, {
  renameFile = rename, pause = ms => new Promise(resolve => setTimeout(resolve, ms))
} = {}) {
  for (let attempt = 0; ; attempt++) {
    try { await renameFile(tmp, target); return; }
    catch (error) {
      if (!['EPERM', 'EACCES', 'EBUSY'].includes(error?.code) || attempt >= 5) throw error;
      await pause(5 * (2 ** attempt));
    }
  }
}

function validateResume(client, state) {
  if ((state.phase === 'unknown' || state.error?.code === 'UNKNOWN_MUTATION_OUTCOME')
    && (state.connectionUnpostedIntent || state.editorialReview?.phase === 'unposted' || state.pendingRepair?.phase === 'unposted'))
    throw new CliError('An unconfirmed admission cannot be reclassified as unposted', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
  if (canonical(state.account) !== canonical(client.account)) throw new CliError('Checkpoint account does not match --account', { code: 'WRONG_ACCOUNT' });
  if (state.baseUrl !== client.baseUrl) throw new CliError('Checkpoint server does not match --base-url', { code: 'WRONG_SERVER' });
  if (state.phase === 'unknown' && !state.pendingLocalAdmission && !state.pendingRepair) throw new CliError('Checkpoint contains an unknown mutation outcome; inspect server state before continuing', { code: 'UNKNOWN_MUTATION_OUTCOME' });
}

const admissionPath = kind => kind === 'prepare' ? '/api/engine/prepare' : '/api/approvals';
function admissionResult(kind, result, requestId) {
  const id = kind === 'prepare' ? result?.jobId : result?.id;
  if (!result || typeof id !== 'string' || !id || id.trim() !== id || /[\u0000-\u001f\u007f]/u.test(id)
    || (result.requestId !== undefined && result.requestId !== requestId))
    throw new UnknownMutationError('POST', admissionPath(kind), { code: 'INVALID_LOCAL_ADMISSION_RESPONSE' });
  return result;
}

async function recoverLocalAdmission(client, state, path) {
  const pending = state.pendingLocalAdmission;
  if (!pending) return state;
  const { kind, requestId } = pending;
  if (kind === 'execute') {
    try {
      if (state.executeRequestId !== requestId || !['unknown', 'execute-admitting', 'stopped'].includes(state.phase)
        || (state.error?.details?.path && state.error.details.path !== executeAdmissionPath(state.approvalId)))
        throw new UnknownMutationError('POST', executeAdmissionPath(state.approvalId), { code: 'INVALID_EXECUTION_ADMISSION' });
      const result = await recoverExecuteAdmission(client, { approvalId: state.approvalId, requestId, payloadHash: pending.payloadHash });
      state = { ...state, phase: 'executing', executeJobId: result.jobId, pendingLocalAdmission: null, error: null };
      return path ? await writeCheckpoint(path, state) : state;
    } catch (error) {
      if (typeof client.connectionRejection === 'function') error = await client.connectionRejection(error);
      await persistFailure(path, state, error); throw error;
    }
  }
  const field = kind === 'prepare' ? 'prepareRequestId' : 'approvalRequestId';
  if (!['prepare', 'approval'].includes(kind) || !requestId || state[field] !== requestId
    || !['unknown', `${kind}-admitting`, 'stopped'].includes(state.phase)
    || (state.error?.details?.path && state.error.details.path !== admissionPath(kind)))
    throw new CliError('Checkpoint local admission binding is invalid; inspect before continuing', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  try {
    const payload = kind === 'prepare' ? { itemIds: state.itemIds, instruction: state.instruction, ...(state.workflowMode ? { workflowMode: state.workflowMode } : {}) }
      : { proposals: state.approvedProposals };
    if (typeof pending.payloadHash !== 'string' || !/^[a-f0-9]{64}$/u.test(pending.payloadHash)
      || pending.payloadHash !== localAdmissionPayloadHash(payload))
      throw new UnknownMutationError('POST', admissionPath(kind), { code: 'INVALID_LOCAL_ADMISSION_PAYLOAD' });
    if (typeof client.localAdmission !== 'function') throw new Error('Admission inspection unavailable');
    const receipt = await client.localAdmission(kind, requestId);
    if (receipt?.kind !== kind || receipt?.requestId !== requestId || receipt.status !== 'committed'
      || receipt.payloadHash !== pending.payloadHash)
      throw new UnknownMutationError('POST', admissionPath(kind), { code: 'LOCAL_ADMISSION_UNCONFIRMED' });
    const result = admissionResult(kind, receipt.result, requestId);
    state = { ...state, pendingLocalAdmission: null, error: null,
      ...(kind === 'prepare' ? { phase: 'assistant-running', prepareJobId: result.jobId, prepareTransport: 'engine', prepareScopeReservation: result.scopeReservation || null }
        : { phase: 'approved', approvalId: result.id }) };
    return path ? await writeCheckpoint(path, state) : state;
  } catch (error) {
    const unknown = error?.code === 'UNKNOWN_MUTATION_OUTCOME' ? error
      : new UnknownMutationError('POST', admissionPath(kind), { code: 'LOCAL_ADMISSION_UNCONFIRMED' });
    await persistFailure(path, state, unknown);
    throw unknown;
  }
}

async function persistFailure(path, state, error) {
  if (connectionFailure(error)) {
    if (path) await writeCheckpoint(path, connectionWaitState(state, error));
    return;
  }
  if (path) await writeCheckpoint(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', error: checkpointError(error) });
}

const FAILED_PREPARE_STATUSES = new Set(['failed', 'error', 'cancelled', 'interrupted']);
const PREPARE_POLL_ERROR_CODES = new Set(['POLL_LIMIT', 'NETWORK_ERROR', 'READ_TIMEOUT', 'STOPPED', 'HTTP_ERROR',
  'INVALID_RESPONSE', 'JOB_NOT_FOUND', 'INVALID_PREPARE_JOB', 'JOB_FAILED', 'WRONG_ACCOUNT', 'STALE_OR_CONFLICT', 'UNKNOWN_MUTATION_OUTCOME',
  'READY_BATCH_STOPPED', 'INCOMPLETE_OPERATION_COVERAGE']);
PREPARE_POLL_ERROR_CODES.add('WORKSPACE_GENERATION_MISMATCH');
function preparePollingError(error, jobId) {
  return new CliError('Preparation polling stopped; resume inspection of the existing job', {
    code: PREPARE_POLL_ERROR_CODES.has(error?.code) ? error.code : 'PREPARE_POLL_FAILED',
    status: Number.isInteger(error?.status) && error.status >= 100 && error.status <= 599 ? error.status : null,
    details: { jobId }
  });
}
function terminalPrepareError(state) {
  const job = state.lastPrepareJob;
  if (!state.prepareJobId || job?.id !== state.prepareJobId || !FAILED_PREPARE_STATUSES.has(job.status))
    return new CliError('Terminal preparation checkpoint has no exact failed job binding', { code: 'INVALID_CHECKPOINT' });
  const error = new CliError(`Preparation job ended as ${job.status}; inspect the existing job before starting a new preparation`, {
    code: 'JOB_FAILED', details: { jobId: job.id, status: job.status, ...(job.finishedAt ? { finishedAt: job.finishedAt } : {}),
      ...(state.proposals?.length ? { readyProposals: state.proposals.map(({ id, revision, itemId }) => ({ id, revision, itemId })) } : {}) }
  });
  error.preparationState = state;
  return error;
}

async function settleWorkflowEditorial(client, initial, path, options) {
  let state = initial;
  const reads = createReadObserver(client, { signal: options.signal, pollMs: options.pollMs });
  const editorialClient = Object.create(client);
  editorialClient.getJob = async (...args) => {
    const job = await reads.getJob(...args);
    if (job?.id !== state.editorialReview?.jobId || job.kind !== 'editorial_review' || job.refId !== state.editorialReview?.requestId)
      throw new CliError('Editorial job differs from saved admission', { code: 'INVALID_EDITORIAL_REVIEW' });
    if (job.editorialProgress) {
      state = { ...state, editorialProgress: settledEditorialProgress(job.editorialProgress, state.proposals) };
      if (path) state = await writeCheckpoint(path, state);
      options.onProgress?.({ event: 'editorial.ready', jobId: job.id,
        references: state.editorialProgress.readyForOwnerApproval, pending: state.editorialProgress.pending,
        completedUnits: state.editorialProgress.completedUnits, totalUnits: state.editorialProgress.totalUnits });
    }
    return job;
  };
  let stoppedBeforeDispatch = false;
  if (!state.editorialReview?.requestId) checkAdmissionSignal(options.signal);
  if (state.pendingLocalAdmission && (state.pendingLocalAdmission.kind !== 'editorial'
    || state.pendingLocalAdmission.requestId !== state.editorialReview?.requestId
    || state.pendingLocalAdmission.payloadHash !== state.editorialReview?.payloadHash
    || state.error?.details?.path && state.error.details.path !== EDITORIAL_PATH))
    throw new CliError('Editorial checkpoint admission binding differs', { code: 'INVALID_CHECKPOINT' });
  try {
    await settleEditorialReview(editorialClient, state.proposals, state.editorialReview, { ...options, fresh: options.freshEditorial === true,
      onProgress: event => {
        options.onProgress?.(event);
        if (event.event === 'editorial.request' && options.signal?.aborted) {
          stoppedBeforeDispatch = true; checkAdmissionSignal(options.signal);
        }
      }, save: async editorialReview => {
      state = { ...state, editorialReview, error: null,
        phase: editorialReview.phase === 'held' ? 'editorial-held' : editorialReview.phase === 'complete' ? 'editorial-reviewed' : `editorial-${editorialReview.phase}`,
        pendingLocalAdmission: editorialReview.phase === 'admitting'
          ? { kind: 'editorial', requestId: editorialReview.requestId, payloadHash: editorialReview.payloadHash } : null };
      if (path) state = await writeCheckpoint(path, state);
    } });
    return state;
  } catch (error) {
    if (stoppedBeforeDispatch) {
      state = { ...state, editorialReview: null, pendingLocalAdmission: null, phase: 'prepared', error: checkpointError(error) };
      if (path) await writeCheckpoint(path, state);
    } else await persistFailure(path, state, error);
    throw error;
  }
}

// A repair is a canonical revision transition, never an arbitrary client edit.
// The saved intent is recovered by receipt inspection after any lost ACK.
async function settleWorkflowRepairs(client, initial, path, options) {
  let state = initial;
  const persist = async next => { state = path ? await writeCheckpoint(path, next) : next; return state; };
  while (state.pendingRepair || state.editorialReview?.outcome?.held.some(row => row.decision === 'revise')) {
    const round = (state.repairs || []).length;
    const held = state.editorialReview?.outcome?.held || [];
    if (!state.pendingRepair && round >= options.maxRepairRounds) break;
    const expected = state.pendingRepair?.expected || held.filter(row => row.decision === 'revise' && row.repairExpected).map(repairExpected);
    if (!expected.length) break;
    const parentReviewJobId = state.pendingRepair?.parentReviewJobId || state.editorialReview.jobId;
    const signature = JSON.stringify(expected.map(row => [row.proposalId, row.textSha256, row.contextDigest, row.rulesDigest]));
    if (!state.pendingRepair && (state.repairSignatures || []).includes(signature)) {
      await persist({ ...state, repairStopReason: 'repeated-editorial-proof' }); break;
    }
    const originProposals = state.originProposals || state.proposals;
    try {
      const repair = await settleEditorialRepair(client, parentReviewJobId, expected, state.pendingRepair, {
        signal: options.signal, save: pendingRepair => persist({ ...state, originProposals, pendingRepair,
          phase: 'editorial-repairing', error: null })
      });
      const review = await client.reviewItems(state.itemIds);
      if (review.coverage?.operationsComplete !== true) throw new CliError('Repaired proposal operation coverage is incomplete', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
      const replacements = new Map(repair.result.newRefs.map(ref => [ref.id, ref]));
      const proposals = state.proposals.map(prior => {
        const ref = replacements.get(prior.id); if (!ref) return prior;
        const rows = (review.proposals || []).filter(row => row.id === ref.id);
        const current = rows[0];
        if (rows.length !== 1 || current.revision !== ref.revision || current.itemId !== prior.itemId
          || current.prepareRunId !== state.prepareJobId || current.status !== 'draft')
          throw new CliError('Repaired proposal is not the current exact draft', { code: 'STALE_OR_CONFLICT' });
        return { id: current.id, revision: current.revision, itemId: current.itemId, kind: current.kind, text: current.text };
      });
      // Keep review history to explain every edit; next review gets a fresh key.
      await persist({ ...state, originProposals, proposals, repairs: [...(state.repairs || []), repair],
        repairSignatures: [...(state.repairSignatures || []), signature], pendingRepair: null,
        editorialHistory: [...(state.editorialHistory || []), state.editorialReview], editorialReview: null,
        pendingLocalAdmission: null, phase: 'prepared', error: null });
      await verifyRepairChain(client, state);
      state = await settleWorkflowEditorial(client, state, path, options);
    } catch (error) { await persistFailure(path, state, error); throw error; }
  }
  return state;
}

export async function runEditorialReview(client, references, { checkpointPath, resumePath, ...options } = {}) {
  const path = checkpointPath || resumePath;
  if (!path) throw new CliError('editorial-review requires --checkpoint or --resume', { code: 'USAGE' });
  if (checkpointPath && resumePath && resolve(checkpointPath) !== resolve(resumePath)) throw new CliError('Editorial resume must update its original checkpoint', { code: 'USAGE' });
  let state;
  if (resumePath) {
    state = await readCheckpoint(resumePath); validateResume(client, state);
    if (state.kind !== 'communityhero-editorial-review'
      || references.length && JSON.stringify(editorialReferences(references)) !== JSON.stringify(state.proposals))
      throw new CliError('Editorial checkpoint scope differs', { code: 'INVALID_CHECKPOINT' });
  } else {
    try { await readFile(path); throw new CliError('Checkpoint already exists; use --resume', { code: 'USAGE' }); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    state = { kind: 'communityhero-editorial-review', account: client.account, baseUrl: client.baseUrl,
      phase: 'starting', proposals: editorialReferences(references) };
  }
  state = await bindWorkflowGeneration(client, state, { resuming: Boolean(resumePath) });
  state = await writeCheckpoint(path, state);
  state = await settleWorkflowEditorial(client, state, path, options);
  return { mode: state.editorialReview.outcome.held.length ? 'editorial-held' : 'editorial-reviewed', checkpoint: state,
    outcome: state.editorialReview.outcome };
}

export async function generateProposals(client, itemIds, { instruction = DEFAULT_INSTRUCTION, checkpointPath, checkpoint = null, materialsAlreadyRefreshed = false, planAlreadyChecked = false, requireScopeReservation = false, recoverPreparationReads = false, workflowMode: requestedMode, workflowId, pollMs, maxPolls, signal, onProgress = () => {}, onReady, saveCheckpoint } = {}) {
  let state = checkpoint || { account: client.account, baseUrl: client.baseUrl, phase: 'starting', workflowId: workflowId || randomUUID(), itemIds: [...new Set(itemIds)], instruction, proposals: [] };
  if (checkpoint && !state.workflowId) state = { ...state, workflowIdentity: { kind: 'legacy', reason: 'legacy-checkpoint-without-workflow-id' } };
  if (workflowId && state.workflowId && state.workflowId !== workflowId) throw new CliError('Workflow identity differs', { code: 'INVALID_CHECKPOINT' });
  const mode = resolveWorkflowMode(checkpoint, requestedMode);
  if (mode) state = { ...state, workflowMode: mode };
  requireScopeReservation ||= state.scopeReservationRequired === true;
  if (requireScopeReservation) state = { ...state, scopeReservationRequired: true };
  const persist = next => saveCheckpoint ? saveCheckpoint(next) : checkpointPath ? writeCheckpoint(checkpointPath, next) : next;
  validateResume(client, state);
  state = await bindWorkflowGeneration(client, state, { resuming: Boolean(checkpoint) });
  if (checkpoint && itemIds?.length && JSON.stringify([...new Set(itemIds)]) !== JSON.stringify(state.itemIds))
    throw new CliError('Preparation selection differs from its checkpoint', { code: 'INVALID_CHECKPOINT' });
  if (!checkpoint && checkpointPath) {
    const target = resolve(checkpointPath); await mkdir(dirname(target), { recursive: true });
    state = { ...state, schemaVersion: VERSION, updatedAt: new Date().toISOString() };
    try { await writeFile(target, `${JSON.stringify(state, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' }); }
    catch (error) { if (error.code === 'EEXIST') throw new CliError('Checkpoint already exists; use --resume', { code: 'USAGE' }); throw error; }
  }
  if (state.phase === 'prepare-failed') throw terminalPrepareError(state);
  state = await recoverLocalAdmission(client, state, checkpointPath);
  // Admission recovery can advance straight to approved/executing without
  // polling preparation. Publish that transition to the same checkpoint owner.
  if (saveCheckpoint) state = await persist(state);
  if (requireScopeReservation && state.prepareJobId && !['assistant-running', 'stopped'].includes(state.phase)
    && !hasPrepareScopeReservation(state.prepareJobId, state.prepareScopeReservation))
    throw new CliError('Saved preparation reservation proof is missing or invalid', { code: 'INVALID_SCOPE_RESERVATION' });
  if (!state.itemIds.length || state.itemIds.length > 100) throw new CliError('Preparation requires 1 to 100 exact --item values', { code: 'USAGE' });
  // An acknowledged execute is observed through its own exact receipt/job;
  // preparation reads must not overwrite that observer checkpoint on resume.
  if (state.executeJobId) return state;
  try { await checkConnection(client); }
  catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
  // An existing durable job keeps its original identity during observation.
  // A new generation must use the native strict grouping contract; neither a
  // saved unadmitted legacy intent nor a 404 grants a conversation fallback.
  if (!state.prepareJobId) {
    if (state.prepareTransport === 'legacy-conversation')
      throw new CliError('Legacy preparation intent has no admitted job; select a strict preparation group', { code: 'STRICT_GROUPING_REQUIRED' });
    await client.requireStrictPreparation();
  }
  const preparationObserver = () => preparationReadObserver(client, { enabled: recoverPreparationReads, signal, pollMs,
    onObservation: async observation => { state = { ...state, observation }; state = await persist(state); } });
  let reads = state.prepareJobId ? preparationObserver() : client;
  const failedObservation = async error => {
    const failure = preparePollingError(error, state.prepareJobId);
    await persist({ ...state, phase: failure.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped',
      error: { ...checkpointError(failure), ...(failure.status === null ? {} : { status: failure.status }) } });
    return failure;
  };
  let snapshot;
  try { snapshot = await reads.reviewItems(state.itemIds); }
  catch (error) { throw state.prepareJobId ? await failedObservation(error) : error; }
  for (const id of state.itemIds) if (!(snapshot.items || []).some(row => row.id === id)) throw new CliError(`Item ${id} not found`, { code: 'ITEM_NOT_FOUND' });
  if (!state.materialsReady) {
    if (materialsAlreadyRefreshed) {
      state = { ...state, materialsReady: true, materialsSource: 'parent-refresh' };
      state = await persist(state);
    } else {
      if (!state.materialsJobId && !state.materialsImport) {
        onProgress({ event: 'materials.request' });
        try {
          const launched = await client.importMaterials(); state = { ...state, phase: 'materials-running', materialsImport: launched, materialsJobId: launched.jobId ?? null };
          state = await persist(state);
        } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
      }
      await settleMaterialsImport(client, state.materialsImport || { jobId: state.materialsJobId }, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
      state = { ...state, phase: 'starting', materialsReady: true, materialsSource: state.materialsJobId ? 'import-job' : 'communityhero-authority' };
      state = await persist(state);
    }
  }
  if (!state.prepareJobId && state.prepareTransport !== 'legacy-conversation' && !planAlreadyChecked) {
    const plan = await client.planPrepare(state.itemIds, state.instruction);
    const exactBatch = plan.held.length === 0 && plan.batches.length === 1
      && plan.batches[0].itemIds.length === state.itemIds.length
      && plan.batches[0].itemIds.every(id => state.itemIds.includes(id));
    if (!exactBatch) throw new CliError('Selected items require smaller preparation batches or explicit holds; use queue with a checkpoint', {
      code: 'PREPARE_PLAN_SPLIT_REQUIRED', details: { batches: plan.batches.map(row => row.itemIds), held: plan.held }
    });
  }
  if (!state.prepareJobId && state.prepareTransport !== 'legacy-conversation') {
    checkAdmissionSignal(signal);
    const requestId = state.prepareRequestId || randomUUID();
    const payloadHash = localAdmissionPayloadHash({ itemIds: state.itemIds, instruction: state.instruction, ...(mode ? { workflowMode: mode } : {}) });
    verifyUnpostedIntent(state, 'prepare', requestId, payloadHash);
    state = { ...state, phase: 'prepare-admitting', prepareRequestId: requestId, pendingLocalAdmission: { kind: 'prepare', requestId, payloadHash } };
    state = await persist(state);
    onProgress({ event: 'prepare.request', transport: 'engine', itemIds: state.itemIds });
    if (signal?.aborted) {
      state = { ...state, phase: 'starting', pendingLocalAdmission: null };
      state = await persist(state); checkAdmissionSignal(signal);
    }
    try {
      const launched = admissionResult('prepare', await client.prepareEngine({ itemIds: state.itemIds, instruction: state.instruction, requestId, ...(mode ? { workflowMode: mode } : {}) }), requestId);
      state = { ...state, phase: 'assistant-running', prepareJobId: launched.jobId, prepareTransport: 'engine', pendingLocalAdmission: null,
        ...(state.connectionUnpostedIntent ? { connectionUnpostedIntent: null } : {}),
        prepareScopeReservation: launched.scopeReservation || null };
      state = await persist(state);
      if (requireScopeReservation && !hasPrepareScopeReservation(state.prepareJobId, state.prepareScopeReservation))
        throw new CliError('Preparation reservation proof is missing or invalid', { code: 'INVALID_SCOPE_RESERVATION' });
      // Publish only the durable ACK. A request or an uncertain admission is
      // never evidence that another preparation may begin.
      onProgress({ event: 'prepare.admitted', jobId: state.prepareJobId, scopeReservation: state.prepareScopeReservation });
    } catch (error) {
      if (error.code !== 'UNKNOWN_MUTATION_OUTCOME' && !connectionFailure(error)) state = { ...state, pendingLocalAdmission: null };
      await persistFailure(checkpointPath, state, error);
      throw error;
    }
  }
  if (state.phase === 'assistant-running' || state.phase === 'stopped') {
    let settled; let observedFailure = null;
    if (reads === client) reads = preparationObserver();
    try {
      settled = await waitForJob(reads, state.prepareJobId, { pollMs, maxPolls, signal, includeSnapshot: false, onPoll: async job => {
        if (job.id !== state.prepareJobId) throw new CliError('Preparation polling returned a different job', { code: 'INVALID_PREPARE_JOB' });
        if (requireScopeReservation && !hasPrepareScopeReservation(job.id, job.scopeReservation))
          throw new CliError('Preparation job reservation proof is missing or invalid', { code: 'INVALID_SCOPE_RESERVATION' });
        if (hasPrepareScopeReservation(job.id, job.scopeReservation)) state = { ...state, prepareScopeReservation: job.scopeReservation };
        const status = String(job.status).toLowerCase();
        if (mode && job.workflowMode !== mode) throw new CliError('Preparation workflow mode differs', { code: 'INVALID_PREPARE_JOB' });
        if (job.progress) state = { ...state, progress: nativeProgress(job.progress) };
        if (['completed', 'failed', 'error', 'cancelled', 'interrupted'].includes(status)) {
          state = { ...state, lastPrepareJob: { id: job.id, status }, observation: { ...reads.observation,
            lastDurableOutcomeAt: new Date().toISOString() } }; state = await persist(state);
        }
        if (FAILED_PREPARE_STATUSES.has(status)) {
          // Retain only bounded terminal metadata, never the model bundle or raw job error.
          observedFailure = { id: job.id, status,
            ...(typeof job.finishedAt === 'string' && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/u.test(job.finishedAt)
              ? { finishedAt: job.finishedAt } : {}) };
        }
        onProgress({ event: 'job.poll', jobId: job.id, status });
        const ready = await discoverReadyProposals(reads, state, job);
        if (ready.length) {
          state = { ...state, durableReadyObserved: true, proposals: [...state.proposals, ...ready] };
          state = await persist(state);
          onProgress({ event: 'prepare.ready', jobId: job.id, status,
            proposals: ready.map(({ id, revision, itemId }) => ({ id, revision, itemId })) });
          if (onReady) {
            try { await onReady(state, async next => {
              state = await persist(next);
              return state;
            }); }
            catch (error) { throw readyBatchStopped(error, job.id); }
          }
        }
      } });
    } catch (error) {
      if (connectionFailure(error)) { await persistFailure(checkpointPath, state, error); throw error; }
      if (error.code === 'JOB_FAILED' && observedFailure) {
        state = { ...state, phase: 'prepare-failed', stopReason: 'prepare-job-failed', lastPrepareJob: observedFailure };
        const failure = terminalPrepareError(state); state.error = checkpointError(failure);
        state = await persist(state);
        onProgress({ event: 'prepare.failed', jobId: state.prepareJobId, status: observedFailure.status });
        throw failure;
      }
      // A read failure or polling bound says nothing about the server job's outcome.
      // Keep its admission/job identities so resume only inspects that same job.
      throw await failedObservation(error);
    }
    let finalProposals = [];
    // Grouped jobs expose only durably admitted candidates, including at the
    // terminal poll. The broad same-run scan is compatibility for legacy jobs.
    if (settled.job.preparationStages?.groupAdmission === undefined) {
      let current;
      try { current = await reads.reviewItems(state.itemIds); }
      catch (error) { throw await failedObservation(error); }
      const allowed = new Set(state.itemIds);
      finalProposals = (current.proposals || []).filter(row => row.prepareRunId === state.prepareJobId && allowed.has(row.itemId) && row.status === 'draft')
        .map(({ id, revision, itemId, kind, text }) => ({ id, revision, itemId, kind, text }));
    }
    const proposals = [...state.proposals, ...finalProposals.filter(row => !state.proposals.some(prior => prior.id === row.id))];
    state = { ...state, phase: 'prepared', prepareOutcome: settled.job.prepareOutcome || null, proposals, error: null, stopReason: null };
    state = await persist(state);
    onProgress({ event: 'prepare.completed', jobId: state.prepareJobId, scopeReservation: state.prepareScopeReservation,
      proposals: proposals.map(row => ({ id: row.id, revision: row.revision, itemId: row.itemId })) });
  }
  return state;
}

function operationOutcome(review, proposals, { approvalId, previous = [] } = {}) {
  const expected = new Set(proposals.map(row => row.id));
  const selected = (review.operations || []).filter(row => expected.has(row.proposalId));
  const invalidBindingOperationIds = selected.filter(row => typeof row.id !== 'string' || !row.id || row.id.length > 128
    || row.id.trim() !== row.id || /[\u0000-\u001f\u007f,]/u.test(row.id) || row.approvalId !== approvalId
    || row.itemId !== proposals.find(proposal => proposal.id === row.proposalId)?.itemId
    || previous.some(prior => prior.proposalId === row.proposalId && prior.id !== row.id)).map(row => row.id);
  const operations = selected.map(row => ({ id: row.id, proposalId: row.proposalId, itemId: row.itemId, approvalId: row.approvalId, status: row.status }));
  const byProposal = new Map();
  for (const row of operations) byProposal.set(row.proposalId, [...(byProposal.get(row.proposalId) || []), row]);
  const missingProposalIds = [...expected].filter(id => !byProposal.has(id));
  const duplicateProposalIds = [...expected].filter(id => (byProposal.get(id) || []).length > 1);
  const incomplete = review.coverage.operationsComplete !== true || missingProposalIds.length > 0
    || duplicateProposalIds.length > 0 || invalidBindingOperationIds.length > 0 || new Set(operations.map(row => row.id)).size !== operations.length
    || operations.some(row => !['succeeded', 'failed', 'stale'].includes(row.status));
  const failed = operations.some(row => ['failed', 'stale'].includes(row.status));
  return {
    phase: incomplete ? 'needs-reconciliation' : failed ? 'completed-with-failures' : 'complete',
    operations, operationEvidence: {
      operationsComplete: review.coverage.operationsComplete,
      missingProposalIds, duplicateProposalIds, invalidBindingOperationIds,
      pendingOperationIds: operations.filter(row => !['succeeded', 'failed', 'stale'].includes(row.status)).map(row => row.id)
    }
  };
}

function exactScanComplete(result) {
  return result?.stopReason === 'exhausted' && result?.hasMore === false && result?.nextResume === null
    && Array.isArray(result.coverage) && result.coverage.length > 0 && result.coverage.every(row => row?.state === 'exhausted')
    && Array.isArray(result.failures) && result.failures.length === 0;
}

export async function runScan(client, request, { checkpointPath, checkpoint = null, pollMs, maxPolls, signal, onProgress = () => {} } = {}) {
  let state = checkpoint?.kind === 'communityhero-provider-scan' ? checkpoint : {
    kind: 'communityhero-provider-scan', account: client.account, baseUrl: client.baseUrl,
    phase: 'starting', request, generation: Number(checkpoint?.generation || 0) + 1
  };
  validateResume(client, state);
  state = await bindWorkflowGeneration(client, state, { resuming: Boolean(checkpoint) });
  if (state.phase === 'unknown') throw new CliError('Scan dispatch outcome is unknown; inspect the engine job ledger before retrying', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  if (state.phase === 'complete') return { mode: 'complete', checkpoint: state, result: state.result };
  if (state.phase === 'incomplete') {
    if (!state.nextResume) throw new CliError('Scan coverage is incomplete but no nextResume token was returned', { code: 'INCOMPLETE_WITHOUT_RESUME' });
    state = { kind: state.kind, account: state.account, baseUrl: state.baseUrl, workflowGeneration: state.workflowGeneration, phase: 'starting', generation: Number(state.generation || 0) + 1, request: { ...request, resume: state.nextResume }, previousJobId: state.jobId };
  }
  if (!state.jobId) {
    onProgress({ event: 'scan.request', generation: state.generation, bounds: { pageSize: state.request.pageSize, maxPages: state.request.maxPages, maxItems: state.request.maxItems, maxElapsedMs: state.request.maxElapsedMs } });
    try {
      const launched = await client.mutate('/api/engine/scan', state.request);
      state = { ...state, phase: 'running', jobId: launched.jobId };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
  }
  const settled = await waitForJob(client, state.jobId, { pollMs, maxPolls, signal, includeSnapshot: false, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
  const result = settled.job.result;
  if (!result || typeof result !== 'object' || Array.isArray(result)) throw new CliError('Completed scan job has no structured result', { code: 'INVALID_RESPONSE' });
  const complete = exactScanComplete(result);
  const incompleteReason = complete ? null : `provider_${result.stopReason || 'coverage_incomplete'}`;
  state = { ...state, phase: complete ? 'complete' : 'incomplete', coverageComplete: complete, incompleteReason, result, nextResume: result.nextResume ?? null };
  if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
  onProgress({ event: 'scan.completed', jobId: state.jobId, coverageComplete: complete, stopReason: result.stopReason || null, resumable: state.nextResume !== null });
  return { mode: state.phase, checkpoint: state, result };
}

export async function runWorkflow(client, itemIds, options = {}) {
  const { checkpointPath, resumePath, instruction, materialsAlreadyRefreshed = false, planAlreadyChecked = false, autonomous = false, execute = false, reconcileUnknown = false, pollMs, maxPolls, signal, onProgress = () => {} } = options;
  let state = resumePath ? await readCheckpoint(resumePath) : null;
  if (state) validateResume(client, state);
  if (state) state = await bindWorkflowGeneration(client, state, { resuming: true });
  const mode = resolveWorkflowMode(state, options.workflowMode, { autonomous, execute });
  const reviewOnly = mode === PREPARE_REVIEW_ONLY;
  if (reviewOnly && (state?.approvalId || state?.approvalRequestId || state?.executeJobId || state?.executeRequestId
    || state?.readyBatches?.some(row => row.approvalId || row.executeJobId)
    || state?.pendingLocalAdmission && !['prepare', 'editorial'].includes(state.pendingLocalAdmission.kind)))
    throw new CliError('Prepare-review-only checkpoint contains effect authority', { code: 'INVALID_CHECKPOINT' });
  const flowPolicy = { freshEditorial: options.freshEditorial ?? state?.flowPolicy?.freshEditorial ?? reviewOnly,
    maxRepairRounds: options.maxRepairRounds ?? state?.flowPolicy?.maxRepairRounds ?? 0,
    continueHeld: options.continueHeld ?? state?.flowPolicy?.continueHeld ?? false };
  if (typeof flowPolicy.freshEditorial !== 'boolean' || typeof flowPolicy.continueHeld !== 'boolean'
    || !Number.isInteger(flowPolicy.maxRepairRounds) || flowPolicy.maxRepairRounds < 0 || flowPolicy.maxRepairRounds > 3
    || flowPolicy.maxRepairRounds > 0 && !flowPolicy.freshEditorial)
    throw new CliError('Repair policy requires fresh editorial and 0..3 repair rounds', { code: 'USAGE' });
  if (state?.flowPolicy && !sameFlowPolicy(state.flowPolicy, flowPolicy))
    throw new CliError('Resume repair policy differs from its checkpoint', { code: 'INVALID_CHECKPOINT' });
  const editorialOptions = { pollMs, maxPolls, signal, onProgress, ...flowPolicy,
    restartInterruptedFresh: !reviewOnly && flowPolicy.freshEditorial && flowPolicy.continueHeld, maxInterruptedRestarts: 1 };
  if (reviewOnly && (!flowPolicy.freshEditorial || flowPolicy.maxRepairRounds))
    throw new CliError('Prepare-review-only requires fresh editorial without automatic repairs', { code: 'USAGE' });
  if (state?.repairs?.length) await verifyRepairChain(client, state);
  if (checkpointPath && resumePath && resolve(checkpointPath) !== resolve(resumePath))
    throw new CliError('Resume must update its original checkpoint', { code: 'USAGE' });
  if (state && itemIds?.length && JSON.stringify([...new Set(itemIds)]) !== JSON.stringify(state.itemIds))
    throw new CliError('Resume selection differs from its checkpoint', { code: 'USAGE' });
  let path = checkpointPath || resumePath;
  const streaming = autonomous && !options.streamingChild;
  if ((streaming || reviewOnly) && !path) {
    path = resolve(homedir(), '.communityhero', 'checkpoints', `run-${randomUUID()}.json`);
    onProgress({ event: 'checkpoint.created', checkpointPath: path });
  }
  const drain = async (current, save) => drainReadyBatches(client, current, {
    path, execute, save, workflow: runWorkflow, read: readCheckpoint, write: writeCheckpoint,
    pollMs, maxPolls, signal, onProgress, reconcileUnknown, ...flowPolicy
  });
  if (streaming && (state?.readyBatches?.length || state?.durableReadyObserved && state.proposals?.length)) {
    try { state = await drain(state, async next => {
      state = path ? await writeCheckpoint(path, next) : next; return state;
    }); }
    catch (error) {
      if (connectionFailure(error)) await persistFailure(path, state, error);
      else if (path) await writeCheckpoint(path, { ...state, error: checkpointError(error) });
      throw error;
    }
  }
  const resumeExecutionReads = state?.executeJobId && ['exhausted', 'disconnected', 'blocked', 'stopped'].includes(state.observation?.state);
  if (state && (['complete', 'complete-with-holds', 'completed-with-failures', 'no-action'].includes(state.phase)
    || state.phase === 'needs-reconciliation' && !reconcileUnknown && !resumeExecutionReads)) return { mode: state.phase, checkpoint: state };
  if (state?.pendingRepair) state = await settleWorkflowRepairs(client, state, path, editorialOptions);
  if (state?.editorialReview && !state.approvalRequestId && !state.approvalId)
    state = await settleWorkflowEditorial(client, state, path, editorialOptions);
  else {
    const coordinator = streaming ? createReadyBatchCoordinator(client, state, {
      path, execute, save: next => writeCheckpoint(path, next), workflow: runWorkflow,
      read: readCheckpoint, write: writeCheckpoint, pollMs, maxPolls, signal, onProgress, reconcileUnknown, ...flowPolicy
    }) : null;
    let preparationError;
    try { state = await generateProposals(client, state?.itemIds || itemIds, { instruction, checkpointPath: path, checkpoint: state, workflowId: options.workflowId, materialsAlreadyRefreshed, planAlreadyChecked, recoverPreparationReads: reviewOnly || autonomous && execute, workflowMode: mode, pollMs, maxPolls, signal, onProgress,
      ...(coordinator ? { onReady: coordinator.observe, saveCheckpoint: coordinator.savePreparation } : {}) }); }
    catch (error) { preparationError = error; }
    // Join the owned consumer on every exit, including observer failure. A
    // child cannot continue dispatching after its coordinator has returned.
    if (coordinator) state = await coordinator.finish() || state;
    if (preparationError) {
      if (streaming && preparationError.code === 'JOB_FAILED' && state?.readyBatches?.length)
        return { mode: 'prepare-partial-failed', checkpoint: state };
      throw preparationError;
    }
  }
  if (!state.flowPolicy) { state = { ...state, flowPolicy }; if (path) state = await writeCheckpoint(path, state); }
  if (reviewOnly) {
    if (!state.proposals.length) {
      state = { ...state, phase: 'no-action' }; if (path) state = await writeCheckpoint(path, state);
      return { mode: state.phase, checkpoint: state };
    }
    state = await settleWorkflowEditorial(client, state, path, editorialOptions);
    const snapshot = await client.reviewItems(state.itemIds);
    const accepted = state.editorialReview.outcome.accepted;
    let readyReferences;
    try { readyReferences = currentReadyReferences(snapshot, state.proposals, accepted, state.readyReferences); }
    catch (error) {
      state = { ...state, phase: 'editorial-held', readyCurrent: false,
        stopReason: 'current-editorial-ready-unavailable', error: checkpointError(error) };
      if (path) await writeCheckpoint(path, state); throw error;
    }
    state = { ...state, readyReferences, phase: readyReferences.length ? READY_FOR_OWNER_APPROVAL : 'editorial-held',
      readyCurrent: true, progress: nativeProgress(snapshot.progress), error: null, stopReason: null };
    if (path) state = await writeCheckpoint(path, state);
    return { mode: state.phase, checkpoint: state, readyReferences, held: state.editorialReview.outcome.held };
  }
  if (streaming && state.readyBatches?.length) {
    state = await drain(state, async next => { state = path ? await writeCheckpoint(path, next) : next; return state; });
    const mode = state.readyBatches.some(batch => ['quarantined', 'quarantined-scope'].includes(batch.mode)) ? 'quarantined'
      : state.readyBatches.some(batch => ['editorial-held', 'complete-with-holds'].includes(batch.mode)) ? 'complete-with-holds'
      : !execute ? 'approved' : state.readyBatches.some(batch => batch.mode === 'completed-with-failures') ? 'completed-with-failures' : 'complete';
    return { mode, preparationStatus: state.phase, checkpoint: state };
  }
  if (state.editorialReview?.outcome?.held.length && !flowPolicy.maxRepairRounds && !flowPolicy.continueHeld) return { mode: 'editorial-held', checkpoint: state };
  if (!autonomous) return { mode: 'prepare-only', checkpoint: state };
  if (!state.proposals.length) {
    state = { ...state, phase: 'no-action' }; if (path) state = await writeCheckpoint(path, state);
    return { mode: state.phase, checkpoint: state };
  }
  if (state.proposals.length > 100) throw new CliError('Autonomous approval is limited to 100 exact proposals', { code: 'AUTONOMOUS_BATCH_LIMIT' });
  if (!state.approvalId) {
    const snapshot = await client.reviewItems([...new Set(state.proposals.map(row => row.itemId))]);
    if (!snapshot.coverage.operationsComplete) throw new CliError('Selected operation history is incomplete; inspect before approval', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
    state = await settleWorkflowEditorial(client, state, path, editorialOptions);
    if (flowPolicy.maxRepairRounds) state = await settleWorkflowRepairs(client, state, path, editorialOptions);
    if (state.editorialReview.outcome.held.length && !flowPolicy.continueHeld) return { mode: 'editorial-held', checkpoint: state };
    const accepted = new Set(state.editorialReview.outcome.accepted.map(ref => JSON.stringify([ref.id, ref.revision])));
    const candidates = flowPolicy.continueHeld ? state.proposals.filter(ref => accepted.has(JSON.stringify([ref.id, ref.revision]))) : state.proposals;
    if (!candidates.length) return { mode: 'editorial-held', checkpoint: state };
    // Repairs may have changed revisions, so fetch current exact drafts after
    // the final fresh review, and approve only its accepted partition.
    const currentSnapshot = await client.reviewItems(state.itemIds);
    if (currentSnapshot.coverage?.operationsComplete !== true) throw new CliError('Selected operation history is incomplete', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
    const exact = candidates.map(ref => {
      const current = (currentSnapshot.proposals || []).find(row => row.id === ref.id);
      if (!current || current.status !== 'draft' || current.revision !== ref.revision || current.itemId !== ref.itemId || current.prepareRunId !== state.prepareJobId)
        throw new CliError(`Proposal ${ref.id} changed; inspect and start a new approval`, { code: 'STALE_OR_CONFLICT' });
      return { id: current.id, revision: current.revision };
    });
    onProgress({ event: 'approval.request', proposals: exact });
    checkAdmissionSignal(signal);
    const requestId = state.approvalRequestId || randomUUID();
    const payloadHash = localAdmissionPayloadHash({ proposals: exact });
    verifyUnpostedIntent(state, 'approval', requestId, payloadHash);
    state = { ...state, phase: 'approval-admitting', approvalRequestId: requestId, approvedProposals: exact, pendingLocalAdmission: { kind: 'approval', requestId, payloadHash } };
    if (path) state = await writeCheckpoint(path, state);
    if (signal?.aborted) {
      state = { ...state, phase: 'editorial-reviewed', pendingLocalAdmission: null };
      if (path) state = await writeCheckpoint(path, state);
      checkAdmissionSignal(signal);
    }
    try {
      const approval = admissionResult('approval', await client.createApproval(exact, requestId), requestId);
      state = { ...state, phase: 'approved', approvalId: approval.id, approvedProposals: exact, pendingLocalAdmission: null,
        ...(state.connectionUnpostedIntent ? { connectionUnpostedIntent: null } : {}) };
      if (path) state = await writeCheckpoint(path, state);
    } catch (error) { if (error.code !== 'UNKNOWN_MUTATION_OUTCOME' && !connectionFailure(error)) state = { ...state, pendingLocalAdmission: null }; await persistFailure(path, state, error); throw error; }
  }
  if (!execute) return { mode: 'approved', checkpoint: state };
  if (!state.executeJobId) {
    checkAdmissionSignal(signal);
    const requestId = state.executeRequestId || randomUUID();
    const payloadHash = localAdmissionPayloadHash({ approvalId: state.approvalId });
    verifyUnpostedIntent(state, 'execute', requestId, payloadHash);
    state = { ...state, phase: 'execute-admitting', executeRequestId: requestId,
      pendingLocalAdmission: { kind: 'execute', requestId, payloadHash } };
    if (path) state = await writeCheckpoint(path, state);
    onProgress({ event: 'execute.request', approvalId: state.approvalId, requestId });
    if (signal?.aborted) {
      // This intent has not crossed the POST boundary. Retain its key and
      // exact approval, but do not mistake a known non-dispatch for UNKNOWN.
      state = { ...state, phase: 'approved', pendingLocalAdmission: null };
      if (path) state = await writeCheckpoint(path, state);
      checkAdmissionSignal(signal);
    }
    try {
      const launched = executeAdmissionResult(await client.execute(state.approvalId, requestId), state.approvalId, requestId);
      state = { ...state, phase: 'executing', executeJobId: launched.jobId, pendingLocalAdmission: null,
        ...(state.connectionUnpostedIntent ? { connectionUnpostedIntent: null } : {}) };
      if (path) state = await writeCheckpoint(path, state);
    } catch (error) { await persistFailure(path, state, error); throw error; }
  }
  const executionReads = createReadObserver(client, { signal, pollMs, onObservation: async observation => {
    state = { ...state, observation }; if (path) state = await writeCheckpoint(path, state);
  } });
  try {
  await waitForJob(executionReads, state.executeJobId, { pollMs, maxPolls, signal, includeSnapshot: false, onPoll: async job => {
    if (job.id !== state.executeJobId || job.kind !== 'execute' || job.refId !== state.approvalId)
      throw new CliError('Execution job binding is absent or invalid', { code: 'JOB_NOT_FOUND' });
    state = { ...state, lastExecuteJob: { id: job.id, kind: job.kind, refId: job.refId, status: String(job.status).toLowerCase() } };
    if (path) state = await writeCheckpoint(path, state);
    onProgress({ event: 'job.poll', jobId: job.id, status: job.status });
  } });
  } catch (error) {
    if (error.code !== 'JOB_FAILED') {
      state = { ...state, error: checkpointError(error) };
      if (path) state = await writeCheckpoint(path, state);
      throw error;
    }
    if (error.details?.status === 'interrupted') {
    const closure = await confirmedExecutionClosure(executionReads, state);
    if (!closure) throw new CliError('Interrupted execution has no complete original admission closure', { code: 'INVALID_EXECUTION_CLOSURE' });
    // Startup interruption is an observation boundary. Preserve the exact
    // admitted effects before any reconcile-only work or independent sender.
    state = { ...state, phase: closure.some(row => row.status === 'unknown') ? 'needs-reconciliation'
      : closure.some(row => ['failed', 'stale'].includes(row.status)) ? 'completed-with-failures' : 'complete', operations: closure };
    if (path) state = await writeCheckpoint(path, state);
    } else state = { ...state, nativeExecutionFailed: true };
  }
  const executedProposals = state.approvedProposals ? state.proposals.filter(row => state.approvedProposals.some(ref => ref.id === row.id && ref.revision === row.revision)) : state.proposals;
  const selectedItemIds = [...new Set(executedProposals.map(row => row.itemId))];
  let review;
  try { review = await executionReads.reviewItems(selectedItemIds); }
  catch (error) { state = { ...state, phase: 'needs-reconciliation', error: checkpointError(error) }; if (path) await writeCheckpoint(path, state); throw error; }
  state = { ...state, ...operationOutcome(review, executedProposals, { approvalId: state.approvalId, previous: state.operations }),
    error: null, stopReason: null, progress: nativeProgress(review.progress) };
  if (state.nativeExecutionFailed && state.phase === 'complete') state.phase = 'completed-with-failures';
  state = { ...state, observation: { ...executionReads.observation, state: state.phase === 'needs-reconciliation' ? 'unresolved' : 'complete',
    lastDurableOutcomeAt: new Date().toISOString(), coverage: state.operationEvidence } };
  if (path) state = await writeCheckpoint(path, state);
  onProgress({ event: 'execute.observed', jobId: state.executeJobId, phase: state.phase,
    operations: state.operations.map(({ id, proposalId, itemId, status }) => ({ id, proposalId, itemId, status })) });
  if (state.phase === 'needs-reconciliation' && reconcileUnknown) {
    const jobs = { ...(state.reconciliationJobs || {}) };
    for (const operation of state.operations.filter(row => row.status === 'unknown')) {
      let jobId = jobs[operation.id];
      if (!jobId) {
        onProgress({ event: 'reconcile.request', operationId: operation.id });
        checkAdmissionSignal(signal);
        try {
          const launched = await client.reconcile(operation.id); jobId = launched.jobId; jobs[operation.id] = jobId;
          state = { ...state, reconciliationJobs: jobs }; if (path) state = await writeCheckpoint(path, state);
        } catch (error) { await persistFailure(path, state, error); throw error; }
      }
      await waitForJob(client, jobId, { pollMs, maxPolls, signal, includeSnapshot: false, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    }
    const refreshed = await client.reviewItems(selectedItemIds);
    state = { ...state, ...operationOutcome(refreshed, executedProposals, { approvalId: state.approvalId, previous: state.operations }) };
    if (path) state = await writeCheckpoint(path, state);
  }
  if (state.phase === 'complete' && state.editorialReview?.outcome?.held.length) {
    state = { ...state, phase: 'complete-with-holds' }; if (path) state = await writeCheckpoint(path, state);
  }
  return { mode: state.phase, checkpoint: state };
}

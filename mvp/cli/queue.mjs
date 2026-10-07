import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { CliError, checkpointError, settleMaterialsImport, waitForJob } from './client.mjs';
import { DEFAULT_INSTRUCTION, generateProposals, hasPrepareScopeReservation, readCheckpoint, runWorkflow, sameFlowPolicy, writeCheckpoint } from './workflow.mjs';
import { confirmedExecutionClosure } from './ready-batches.mjs';
import { runBulk } from './bulk.mjs';
import { createPrepareProducers } from './prepare-producers.mjs';
import { createQueuePreparePlanning } from './queue-prepare-planning.mjs';
import { bindWorkflowGeneration, currentReadyReferences, nativeProgress, workflowMode as resolveWorkflowMode, PREPARE_REVIEW_ONLY, READY_FOR_OWNER_APPROVAL } from './read-observer.mjs';
import { checkConnection, connectionFailure, connectionWaitState } from './conductor-connection.mjs';

const ACTIVE_PROPOSALS = new Set(['draft', 'approved', 'dispatching', 'unknown', 'succeeded']);
const ACTIVE_OPERATIONS = new Set(['dispatching', 'unknown', 'succeeded']);
const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
const knownBranchKeys = item => [
  typeof item.branchId === 'string' && item.branchId ? `branch:${item.branchId}` : null,
  typeof item.conversationKey === 'string' && item.conversationKey ? `conversation:${item.conversationKey}` : null
].filter(Boolean);

export function queueCoverage(snapshot) {
  const sync = snapshot.sync || {};
  const canonicalCoverage = sync.openCoverage && typeof sync.openCoverage === 'object' ? sync.openCoverage : null;
  const frontier = canonicalCoverage || sync.openFrontier;
  const source = canonicalCoverage ? 'openCoverage' : 'openFrontier';
  if (!frontier || typeof frontier !== 'object') return { known: false, complete: false, pending: false, status: sync.status || null, reason: 'missing-open-coverage', signature: null };
  const done = frontier.done; const complete = done === true && frontier.coverageComplete === true;
  const pending = done === false && !frontier.invalidatedAt;
  const signature = JSON.stringify([source, frontier.scanId || null, frontier.id || null, frontier.cursor ?? null, frontier.pages ?? null, done]);
  const reason = complete ? null : pending ? 'open-frontier-pending' : done === true ? 'open-frontier-incomplete-evidence' : 'unknown-open-frontier';
  return { known: done === true || done === false, complete, pending, status: sync.status || null, reason, signature, source,
    openFrontier: { id: frontier.id || null, scanId: frontier.scanId || null, scope: frontier.scope || null, cursor: frontier.cursor ?? null, done, pages: frontier.pages ?? null, skipped: frontier.skipped ?? null, unknownDates: frontier.unknownDates ?? null, coverageComplete: frontier.coverageComplete === true, invalidatedAt: frontier.invalidatedAt || null },
    closedPending: sync.scan?.closed?.done === false };
}

export function unknownQueueBlockers(snapshot) {
  const itemIds = new Set(); const conversationKeys = new Set();
  const items = new Map((snapshot.items || []).map(item => [item.id, item]));
  for (const row of [...(snapshot.proposals || []), ...(snapshot.operations || [])]) {
    if (row.status !== 'unknown') continue;
    if (row.itemId) itemIds.add(row.itemId);
    for (const key of [row.conversationKey, row.action?.conversationKey, row.target?.conversationKey, items.get(row.itemId)?.conversationKey])
      if (typeof key === 'string' && key) conversationKeys.add(key);
  }
  return new Set((snapshot.items || []).filter(item => itemIds.has(item.id)
    || typeof item.conversationKey === 'string' && conversationKeys.has(item.conversationKey)).map(item => item.id));
}

export function eligible(snapshot, attempted) {
  const blocked = new Set();
  for (const id of unknownQueueBlockers(snapshot)) blocked.add(id);
  for (const row of snapshot.proposals || []) if (ACTIVE_PROPOSALS.has(row.status)) blocked.add(row.itemId);
  for (const row of snapshot.operations || []) if (ACTIVE_OPERATIONS.has(row.status)) blocked.add(row.itemId);
  return (snapshot.items || []).filter(item => item?.id && item.workflow === 'attention'
    && (!item.providerStatus || ['new', 'inprogress'].includes(item.providerStatus))
    && !blocked.has(item.id) && !attempted.has(item.id))
    .sort((a, b) => String(a.createdAt || '').localeCompare(String(b.createdAt || '')) || String(a.id).localeCompare(String(b.id)));
}

// This is discovery only. The normal editorial/approval/execute reducers still
// validate the complete current evidence and authority in their transactions.
export function currentDrafts(review, itemIds, { runId } = {}) {
  if (review.coverage?.operationsComplete !== true) throw new CliError('Existing drafts require complete operation history', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
  const candidates = []; const held = []; const quarantined = unknownQueueBlockers(review);
  for (const itemId of itemIds) {
    const items = (review.items || []).filter(row => row.id === itemId);
    if (items.length !== 1) throw new CliError('Existing draft recipient is absent or ambiguous', { code: 'INVALID_REVIEW_COVERAGE' });
    const item = items[0]; const proposals = (review.proposals || []).filter(row => row.itemId === itemId && ACTIVE_PROPOSALS.has(row.status));
    const draft = proposals[0]; let reason = null;
    const keys = knownBranchKeys(item);
    const operations = (review.operations || []).filter(op => op.itemId === itemId || op.target?.id === itemId
      || proposals.some(p => p.id === op.proposalId)
      || [op.conversationKey, op.action?.conversationKey, op.target?.conversationKey].some(key => key && keys.includes(`conversation:${key}`)));
    if (quarantined.has(itemId)) reason = 'unknown-conversation-held';
    else if (operations.length) reason = 'existing-draft-operation-held';
    else if (proposals.length !== 1 || draft?.status !== 'draft') reason = 'existing-draft-ambiguous-or-active';
    else if (!['attention', 'prepared'].includes(item.workflow) || !['new', 'inprogress'].includes(item.providerStatus)) reason = 'existing-draft-recipient-unavailable';
    else if (item.draftEdited === true || item.draft != null && (typeof item.draft !== 'string' || item.draft.trim())
      || draft.revision !== 1 || draft.origin != null || draft.recovery != null || draft.sourceProposalId != null
      || typeof draft.prepareRunId !== 'string' || !draft.prepareRunId
      || typeof draft.prepareBundleId !== 'string' || !draft.prepareBundleId
      || !/^[a-f0-9]{64}$/u.test(draft.prepareBundleDigest || '')
      || !/^[a-f0-9]{64}$/u.test(draft.reviewContextDigest || '')) reason = 'existing-draft-manual-or-edited';
    else if ((draft.conductorRunId != null || draft.grantGeneration != null)
      && (draft.conductorRunId !== runId || !Number.isSafeInteger(draft.grantGeneration) || draft.grantGeneration < 1)) reason = 'existing-draft-foreign-owner';
    else if (!Number.isSafeInteger(draft.itemRevision) || draft.itemRevision < 1 || draft.itemRevision !== item.revision
      || !/^[a-f0-9]{64}$/u.test(draft.contextEvidenceDigest || '') || !/^[a-f0-9]{64}$/u.test(draft.branchContextDigest || '')
      || draft.contextEvidenceDigest !== item.contextEvidenceDigest || draft.branchContextDigest !== item.branchContextDigest) reason = 'existing-draft-stale';
    else if (!['reply_and_close', 'close', 'hide', 'delete'].includes(draft.kind)) reason = 'existing-draft-action-unavailable';
    if (reason) held.push({ itemId, reason });
    else candidates.push({ id: draft.id, revision: draft.revision, itemId, kind: draft.kind });
  }
  return { candidates, held };
}

const draftRefs = rows => rows.map(({ id, revision, itemId }) => ({ id, revision, itemId }));
const draftOperations = child => child.kind === 'communityhero-reviewed-bulk' ? (child.slices || []).flatMap(row => row.operations || []) : [];
const draftEditorialHolds = child => child.kind === 'communityhero-reviewed-bulk' ? (child.slices || []).flatMap(row => [
  ...(row.editorialReview?.outcome?.held || []), ...(row.admission?.held || [])]) : [];

function needsStageUpgrade(slice, autonomous, execute) {
  if (!autonomous || slice.adoptedDrafts) return false;
  const phase = slice.child?.phase;
  if (phase === 'prepared') return true;
  if (slice.child?.editorialReview && !slice.child.editorialReview.outcome) return true;
  return execute && (['approved', 'executing', 'needs-reconciliation'].includes(phase)
    || slice.child?.pendingLocalAdmission?.kind === 'execute');
}

function executionStopReason(state, execute, continueHeld = false) {
  const slices = state.slices || [];
  if (!continueHeld && slices.some(slice => slice.status === 'editorial-held' || slice.child?.editorialReview?.outcome?.held.length))
    return 'editorial-review-held';
  if (slices.some(slice => slice.status === 'needs-reconciliation' || slice.child?.phase === 'needs-reconciliation'))
    return 'operation-outcomes-unresolved';
  const counts = summarizeQueue(state);
  if (counts.unresolvedTransport || counts.sliceFailures || counts.unresolvedItems)
    return 'slice-outcomes-unresolved';
  if (!execute) return null;
  if (counts.failed || counts.stale || slices.some(slice => slice.status === 'completed-with-failures'))
    return 'operation-failures';
  return null;
}

async function save(path, state) { return path ? writeCheckpoint(path, state) : state; }

// A failed policy read is not evidence of missing policy. Preserve bounded
// transport diagnostics, never arbitrary server bodies or exception messages.
function materialsError(error, stage) {
  const safe = checkpointError(error);
  return { ...safe, details: { ...safe.details,
    stage: ['import-admission','policy-verification'].includes(stage) ? stage : 'policy-verification' } };
}

export async function freshSync(client, state, path, poll) {
  if (!state.syncJobId) {
    try {
      const launched = await client.sync({}); state = await save(path, { ...state, syncJobId: launched.jobId, phase: 'syncing' });
    } catch (error) {
      if (connectionFailure(error)) { await save(path, connectionWaitState(state, error)); throw error; }
      // Only the server's explicit duplicate-sync response permits adoption.
      // Unknown POST outcomes and unrelated conflicts are never retried/adopted.
      if (error.status === 409 && error.code === 'STALE_OR_CONFLICT'
        && error.message === 'A job of this kind is already running') {
        const snapshot = await client.bootstrap();
        if (canonical(snapshot.account) !== canonical(client.account))
          throw new CliError('Active sync belongs to another account', { code: 'WRONG_ACCOUNT' });
        const active = (snapshot.jobs || []).filter(job => job.kind === 'sync'
          && ['running', 'queued'].includes(job.status));
        if (active.length === 1 && typeof active[0].id === 'string' && active[0].id
          && (!active[0].account || canonical(active[0].account) === canonical(client.account))) {
          state = await save(path, { ...state, syncJobId: active[0].id, phase: 'syncing',
            syncAdopted: true, stopReason: null, error: null });
        }
      }
      if (!state.syncJobId) {
      state = await save(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', stopReason: 'sync-launch-failed', error: checkpointError(error) });
      throw error;
      }
    }
  }
  const settled = await waitForJob(client, state.syncJobId, poll);
  const currentCoverage = queueCoverage(settled.snapshot);
  const stalled = currentCoverage.pending && state.lastFrontierSignature === currentCoverage.signature;
  state = await save(path, { ...state, phase: 'running', syncJobId: null, coverage: currentCoverage,
    coverageNeedsRefresh: false, lastFrontierSignature: currentCoverage.signature,
    frontierStalled: stalled, syncCycles: Number(state.syncCycles || 0) + 1 });
  return { state, snapshot: settled.snapshot };
}

export function summarizeQueue(state) {
  const operations = new Map((state.quarantinedOperations || []).map(row => [row.id, row]));
  let held = (state.scopeHolds || []).length; let prepared = 0; let adoptedDrafts = 0; let unresolvedTransport = 0; let sliceFailures = 0; let unresolvedItems = 0;
  for (const slice of state.slices || []) {
    const child = slice.child || {};
    if (slice.dependencyResolved === true && ['media_wait', 'media_pending', 'media_unavailable'].includes(slice.planHoldReason)
      && !slice.child) continue;
    const proposals = slice.adoptedDrafts || child.proposals || [];
    const proposalById = new Map(proposals.map(row => [row.id, row]));
    const proposedItemIds = new Set(proposals.map(row => row.itemId));
    const operationItemIds = new Set();
    prepared += (child.proposals || []).length;
    adoptedDrafts += (slice.adoptedDrafts || []).length;
    for (const op of [...(child.operations || []), ...draftOperations(child), ...(child.readyBatches || []).flatMap(batch => batch.operations || [])]) {
      const proposal = proposalById.get(op.proposalId);
      if (proposal?.itemId) operationItemIds.add(proposal.itemId);
      operations.set(op.id, { ...op, kind: proposal?.kind });
    }
    for (const itemId of new Set(slice.itemIds || [])) {
      if ((slice.dependencyResolvedItemIds || []).includes(itemId)) continue;
      if (operationItemIds.has(itemId)) continue;
      if (slice.status === 'editorial-held' || child.editorialReview?.outcome?.held.some(row => proposalById.get(row.reference.id)?.itemId === itemId)
        || draftEditorialHolds(child).some(row => proposalById.get(row.reference?.id)?.itemId === itemId)
        || child.readyBatches?.some(batch => batch.held?.some(row => proposalById.get(row.reference.id)?.itemId === itemId))) held += 1;
      else if (slice.status === 'unknown') unresolvedTransport += 1;
      else if (slice.error) sliceFailures += 1;
      else if ((child.executeJobId || child.slices?.some(row => row.executeAttemptId)) && proposedItemIds.has(itemId)) unresolvedItems += 1;
      else if (!proposedItemIds.has(itemId)) held += 1;
    }
  }
  let replies = 0; let noReply = 0; let failed = 0; let unknown = 0; let stale = 0;
  for (const op of operations.values()) {
    if (op.status === 'unknown') unknown += 1;
    else if (op.status === 'stale') stale += 1;
    else if (op.status === 'failed') failed += 1;
    else if (op.status === 'succeeded' && op.kind === 'reply_and_close') replies += 1;
    else if (op.status === 'succeeded' && op.kind && op.kind !== 'reply_and_close') noReply += 1;
  }
  return { verifiedReplies: replies, verifiedNoReply: noReply, failed, unknown, stale, held,
    unresolvedTransport, sliceFailures, unresolvedItems, prepared, ...(state.adoptCurrentDrafts ? { adoptedDrafts } : {}), slices: (state.slices || []).length };
}

export async function runQueue(client, options) {
  const { checkpointPath, resumePath, batchSize = 60, maxCycles = 1000, autonomous = false, execute = false, instruction, pollMs, maxPolls, signal, waitForDependencies, onProgress = () => {} } = options;
  const path = checkpointPath || resumePath;
  if (!path) throw new CliError('queue requires --checkpoint or --resume', { code: 'USAGE' });
  if (!Number.isInteger(batchSize) || batchSize < 1 || batchSize > 100) throw new CliError('queue --batch-size must be 1..100', { code: 'USAGE' });
  if (execute && !autonomous) throw new CliError('queue --execute requires --autonomous', { code: 'USAGE' });
  if (checkpointPath && !resumePath) {
    try { await readFile(checkpointPath, 'utf8'); throw new CliError('Queue checkpoint already exists; use --resume explicitly or choose another path', { code: 'USAGE' }); }
    catch (error) { if (error.code !== 'ENOENT' && error.code !== 'USAGE') throw error; if (error.code === 'USAGE') throw error; }
  }
  let state = resumePath ? await readCheckpoint(resumePath) : {
    kind: 'communityhero-queue', account: client.account, baseUrl: client.baseUrl, phase: 'starting',
    workflowId: options.workflowId || randomUUID(), createdAt: new Date().toISOString(), cycle: 0, attemptedItemIds: [], slices: [], batchSize, maxCycles
  };
  if (state.kind !== 'communityhero-queue' || canonical(state.account) !== canonical(client.account) || state.baseUrl !== client.baseUrl)
    throw new CliError('Queue checkpoint does not match this account/server', { code: 'WRONG_ACCOUNT' });
  state = await bindWorkflowGeneration(client, state, { resuming: Boolean(resumePath) });
  const mode = resolveWorkflowMode(resumePath ? state : null, options.workflowMode, { autonomous, execute });
  const reviewOnly = mode === PREPARE_REVIEW_ONLY;
  if (mode) state = { ...state, workflowMode: mode };
  if (!state.workflowId) state = { ...state, workflowIdentity: { kind: 'legacy', reason: 'legacy-checkpoint-without-workflow-id' } };
  if (options.workflowId && state.workflowId && options.workflowId !== state.workflowId) throw new CliError('Queue workflow identity differs', { code: 'INVALID_CHECKPOINT' });
  if (options.adoptCurrentDrafts !== undefined && typeof options.adoptCurrentDrafts !== 'boolean'
    || state.adoptCurrentDrafts !== undefined && typeof state.adoptCurrentDrafts !== 'boolean')
    throw new CliError('Invalid existing draft policy', { code: 'USAGE' });
  const pristine = state.phase === 'starting' && !state.cycle && !state.currentSlice && !state.pendingSlices?.length
    && !state.slices?.length && !state.attemptedItemIds?.length;
  // Old journals retain their owned recovery policy. Only a new/pristine run
  // may opt in; a saved explicit policy cannot change on resume.
  const adoptCurrentDrafts = state.adoptCurrentDrafts ?? (resumePath && !pristine ? false : options.adoptCurrentDrafts ?? false);
  if (state.adoptCurrentDrafts !== undefined && options.adoptCurrentDrafts !== undefined && options.adoptCurrentDrafts !== state.adoptCurrentDrafts)
    throw new CliError('Queue resume existing draft policy differs', { code: 'INVALID_CHECKPOINT' });
  if (adoptCurrentDrafts && (!autonomous || !execute)) throw new CliError('Existing draft adoption requires explicit autonomous execution', { code: 'USAGE' });
  const scopeItemIds = options.scopeItemIds ?? state.scopeItemIds;
  const cutoffUtc = options.cutoffUtc ?? state.cutoffUtc;
  if (scopeItemIds !== undefined && (!Array.isArray(scopeItemIds) || !scopeItemIds.length || scopeItemIds.length > 5000
    || new Set(scopeItemIds).size !== scopeItemIds.length || scopeItemIds.some(id => typeof id !== 'string' || !id.trim() || id.trim() !== id
      || id.length > 160 || /[\u0000-\u001f\u007f]/u.test(id))))
    throw new CliError('Queue manifest requires 1..5000 distinct item IDs', { code: 'USAGE' });
  if (cutoffUtc !== undefined && (typeof cutoffUtc !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/u.test(cutoffUtc) || !Number.isFinite(Date.parse(cutoffUtc))))
    throw new CliError('Queue cutoff requires a UTC timestamp', { code: 'USAGE' });
  if (resumePath && (JSON.stringify(state.scopeItemIds) !== JSON.stringify(scopeItemIds) || state.cutoffUtc !== cutoffUtc))
    throw new CliError('Queue resume manifest differs from its checkpoint', { code: 'INVALID_CHECKPOINT' });
  const flowPolicy = { freshEditorial: options.freshEditorial ?? state.flowPolicy?.freshEditorial ?? reviewOnly,
    maxRepairRounds: options.maxRepairRounds ?? state.flowPolicy?.maxRepairRounds ?? 0,
    continueHeld: options.continueHeld ?? state.flowPolicy?.continueHeld ?? false };
  if (typeof flowPolicy.freshEditorial !== 'boolean' || typeof flowPolicy.continueHeld !== 'boolean'
    || !Number.isInteger(flowPolicy.maxRepairRounds) || flowPolicy.maxRepairRounds < 0 || flowPolicy.maxRepairRounds > 3
    || flowPolicy.maxRepairRounds > 0 && !flowPolicy.freshEditorial)
    throw new CliError('Repair policy requires fresh editorial and 0..3 repair rounds', { code: 'USAGE' });
  if (state.flowPolicy && !sameFlowPolicy(state.flowPolicy, flowPolicy))
    throw new CliError('Queue resume policy differs from its checkpoint', { code: 'INVALID_CHECKPOINT' });
  if (reviewOnly && state.phase === READY_FOR_OWNER_APPROVAL) {
    try {
    for (const slice of state.slices || []) {
      const child = slice.child;
      if (!child?.readyReferences?.length) continue;
      const snapshot = await client.reviewItems(child.itemIds);
      currentReadyReferences(snapshot, child.proposals, child.editorialReview.outcome.accepted, child.readyReferences);
    }
    } catch (error) {
      await save(path, { ...state, phase: 'stopped', stopReason: 'current-editorial-ready-unavailable', error: checkpointError(error) }); throw error;
    }
    return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
  }
  if (adoptCurrentDrafts && (!scopeItemIds || !flowPolicy.freshEditorial))
    throw new CliError('Existing draft adoption requires a fixed manifest and fresh editorial', { code: 'USAGE' });
  state = await save(path, { ...state, ...(scopeItemIds ? { scopeItemIds } : {}), ...(cutoffUtc ? { cutoffUtc } : {}), flowPolicy,
    ...(adoptCurrentDrafts || state.adoptCurrentDrafts !== undefined ? { adoptCurrentDrafts } : {}) });
  const scope = scopeItemIds ? new Set(scopeItemIds) : null;
  const settledSlice = row => !row?.error && (['complete', 'no-action'].includes(row?.status)
    || !execute && ['prepare-only', 'approved', READY_FOR_OWNER_APPROVAL].includes(row?.status)
    || flowPolicy.continueHeld && ['editorial-held', 'complete-with-holds', 'prepare-partial-failed', 'quarantined'].includes(row?.status));
  if (state.phase === 'unknown') throw new CliError('Queue checkpoint has an unknown mutation outcome; inspect original admissions and jobs before resuming', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  if (state.pendingMaterialsImport) {
    const error = new CliError('Materials import admission has no retained result; inspect the original request before retrying', { code: 'UNKNOWN_MUTATION_OUTCOME' });
    state = await save(path, { ...state, phase: 'unknown', stopReason: 'materials-import-launch-unresolved', error: materialsError(error, 'import-admission') });
    throw error;
  }
  // Only an observed server guarantee permits another independent admission.
  // Old engines keep the sequential path; the request payload stays unchanged.
  const engine = (reviewOnly || autonomous && execute) && typeof client.engineStatus === 'function' ? await client.engineStatus() : null;
  if (engine) { state = await save(path, { ...state, progress: nativeProgress(engine.progress) }); }
  const overlap = engine?.prepareScopeReservations?.version === 1;
  if (overlap) state = { ...state, overlapVersion: 1 };
  const preserveOverlapQuarantine = overlap || state.overlapVersion === 1;
  const workerWidth = engine?.prepareWorkers?.version === 1
    && Number.isSafeInteger(engine.prepareWorkers.maxWorkers) && engine.prepareWorkers.maxWorkers >= 1
    && engine.prepareWorkers.maxWorkers <= 8 ? engine.prepareWorkers.maxWorkers : 1;
  const producers = createPrepareProducers({ maxProducers: Math.max(1, workerWidth), signal });
  let planning = null;
  const withProduced = (current, produced) => ({ ...current, pendingSlices: (current.pendingSlices || []).map(row => {
    const result = produced.find(entry => entry.id === row.id);
    return result?.error?.status === 409 ? { ...row, preparationError: checkpointError(result.error) }
      : result?.checkpoint ? { ...row, preparationReady: true } : row;
  }) });
  const startProducer = (slice, windowId, onAdmitted = () => {}) => {
    if (slice.planHoldReason || slice.adoptedDrafts || slice.preparationReady || slice.preparationError) return;
    // pendingSlices already records this child path before the first POST.
    // The producer owns only that separate checkpoint, never the queue journal.
    producers.start({ id: slice.id, windowId, run: async childSignal => {
      let checkpoint;
      try { checkpoint = await readCheckpoint(slice.childPath); }
      catch (error) { if (!(error.code === 'INVALID_CHECKPOINT' && error.details?.cause === 'ENOENT')) throw error; }
      if (checkpoint && JSON.stringify(checkpoint.itemIds) !== JSON.stringify(slice.itemIds))
        throw new CliError('Producer selection differs from its saved checkpoint', { code: 'INVALID_CHECKPOINT' });
      if (childSignal.aborted) throw new CliError('Stopped before lookahead preparation', { code: 'STOPPED' });
      return generateProposals(client, slice.itemIds, { checkpoint, checkpointPath: slice.childPath,
        instruction, workflowMode: mode, workflowId: state.workflowId, materialsAlreadyRefreshed: true, planAlreadyChecked: Number.isInteger(slice.plannedBytes), requireScopeReservation: true, recoverPreparationReads: true,
        pollMs, maxPolls, signal: childSignal,
        onProgress: event => {
          onProgress({ ...event, queueSliceId: slice.id, preparationOnly: true });
          if (event.event === 'prepare.admitted' && hasPrepareScopeReservation(event.jobId, event.scopeReservation)) onAdmitted();
        } });
    } });
  };
  const startLookahead = (pending, currentSlice, { early = false } = {}) => {
    if (!overlap || !pending?.length || signal?.aborted) return;
    const independentPlan = workerWidth > 1 && currentSlice?.familyPlanVersion === 1 && typeof currentSlice.familyWindow === 'string';
    if (early && !independentPlan) return;
    const candidates = independentPlan ? pending.filter(row => row.familyPlanVersion === 1
      && typeof row.familyWindow === 'string' && row.familyWindow !== currentSlice.familyWindow) : pending.slice(0, 1);
    // A foreground workflow retains its historical slot budget. A producer-
    // owned first family already counts in the pool's complete worker width.
    const limit = producers.has(currentSlice.id) ? workerWidth : Math.max(1, workerWidth - 1);
    for (const slice of candidates) {
      if (producers.size >= limit) break;
      startProducer(slice, independentPlan ? slice.familyWindow : slice.id);
    }
  };
  try {
  await checkConnection(client);
  const completedAtStart = state.phase === 'complete'
    || state.phase === 'stopped' && ['operation-outcomes-unresolved', 'slice-outcomes-unresolved', 'editorial-review-held'].includes(state.stopReason)
    || autonomous && (state.slices || []).some(slice => slice.child?.editorialReview && !slice.child.editorialReview.outcome)
    || execute && autonomous && (state.slices || []).some(slice => slice.child?.pendingLocalAdmission?.kind === 'execute'
      || slice.child?.executeJobId && ['executing', 'needs-reconciliation'].includes(slice.child.phase));
  const completedStopReason = state.stopReason;
  if (completedAtStart && !state.currentSlice && !state.pendingSlices?.length
    && !((flowPolicy.continueHeld || typeof waitForDependencies === 'function') && state.stopReason === 'operation-outcomes-unresolved')
    && !(typeof waitForDependencies === 'function' && (state.slices || []).some(slice => slice.child?.prepareOutcome?.factDependencies
      ?.some(row => row.kind === 'missing_public_fact' && !(slice.dependencyResolvedItemIds || []).includes(row.itemId))))
    && !(state.slices || []).some(slice => needsStageUpgrade(slice, autonomous, execute)))
    return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };

  // A fresh database has no account policy. Import it before any model request.
  if (!state.materialsReady) {
    if (!state.materialsJobId && !state.materialsImport) {
      // This endpoint has no durable requestId lookup. A crash after the POST
      // must not silently create a second import job when the queue resumes.
      state = await save(path, { ...state, phase: 'materials-admitting', pendingMaterialsImport: {
        version: 1, method: 'POST', path: '/api/materials/import', recordedAt: new Date().toISOString()
      } });
      let launched;
      try {
        launched = await client.importMaterials();
      } catch (error) {
        if (connectionFailure(error)) {
          const waiting = connectionWaitState(state, error);
          if (connectionFailure(error).beforeMutationPath === '/api/materials/import') {
            waiting.connectionUnpostedMaterialsImport = state.pendingMaterialsImport;
            waiting.pendingMaterialsImport = null; waiting.phase = 'starting';
          }
          state = await save(path, waiting); throw error;
        }
        state = await save(path, { ...state, pendingMaterialsImport: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? state.pendingMaterialsImport : null,
          phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', stopReason: 'materials-import-launch-failed', error: materialsError(error, 'import-admission') });
        throw error;
      }
      // Keep persistence outside the request catch: if writing the ACK fails,
      // the already-saved pending intent remains unresolved on disk.
      state = await save(path, { ...state, materialsImport: launched, materialsJobId: launched.jobId ?? null,
        pendingMaterialsImport: null, phase: 'materials-running', stopReason: null, error: null });
    }
    try {
      await settleMaterialsImport(client, state.materialsImport || { jobId: state.materialsJobId }, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    } catch (error) {
      state = await save(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', stopReason: 'materials-unavailable',
        error: materialsError(error, 'policy-verification') });
      throw error;
    }
    state = await save(path, { ...state, materialsReady: true, materialsJobId: null, phase: 'running', stopReason: null, error: null });
  }

  const processSlice = async (current, slice, preparingSlices = []) => {
    current = await save(path, { ...current, currentSlice: slice });
    let result; let status = 'complete'; let error = null; let child = null;
    try {
      let childExists = false; let savedChild = null;
      try { savedChild = await readCheckpoint(slice.childPath); childExists = true; }
      catch (e) { if (!(e.code === 'INVALID_CHECKPOINT' && e.details?.cause === 'ENOENT')) throw e; }
      if (slice.preparationError) throw new CliError('Next preparation was explicitly rejected; inspect its existing checkpoint', { code: slice.preparationError.code });
      const snapshot = await client.bootstrap();
      const blockers = unknownQueueBlockers(snapshot);
      let inspectingOriginalExecution = Boolean(savedChild?.executeJobId || savedChild?.pendingLocalAdmission?.kind === 'execute'
        || savedChild?.kind === 'communityhero-reviewed-bulk' && savedChild.slices.some(row => row.executeAttemptId));
      if (flowPolicy.continueHeld && !inspectingOriginalExecution && slice.itemIds.some(id => blockers.has(id))) {
        for (const batch of savedChild?.readyBatches || []) {
          if (!/^[a-f0-9]{64}$/u.test(batch.id || '')) continue;
          const nested = await readCheckpoint(resolve(`${slice.childPath}.ready`, `${batch.id}.json`));
          if (nested.executeJobId || nested.pendingLocalAdmission?.kind === 'execute') {
            // This permits inspection of the existing child only. The drain
            // validates its parent binding and canonical original closure.
            inspectingOriginalExecution = true; break;
          }
        }
      }
      // An already dispatched child may still reconcile its own attempt; the
      // workflow resumes its saved execute job without creating another send.
      if (!inspectingOriginalExecution
        && !(flowPolicy.continueHeld && savedChild?.readyBatches?.some(batch => ['quarantined', 'quarantined-scope'].includes(batch.mode)))
        && slice.itemIds.some(id => blockers.has(id)))
        throw new CliError('Queue slice contains an unresolved operation or conversation', { code: 'UNKNOWN_CONVERSATION_HELD' });
      if (slice.adoptedDrafts) {
        const refs = draftRefs(slice.adoptedDrafts);
        if (refs.length !== slice.itemIds.length || !refs.length || refs.length > 100
          || new Set(refs.map(row => row.id)).size !== refs.length
          || JSON.stringify(refs.map(row => row.itemId)) !== JSON.stringify(slice.itemIds)
          || childExists && (savedChild.kind !== 'communityhero-reviewed-bulk'
            || JSON.stringify(savedChild.references) !== JSON.stringify(refs)))
          throw new CliError('Adopted draft checkpoint differs from the saved exact scope', { code: 'INVALID_CHECKPOINT' });
        // An existing bulk child owns its saved admission keys. Rediscovery is
        // only for the initial handoff, never a substitute after a lost ACK.
        if (!childExists) {
          const discovery = currentDrafts(await client.reviewItems(slice.itemIds), slice.itemIds, { runId: current.conductorRunId });
          if (discovery.held.length || JSON.stringify(draftRefs(discovery.candidates)) !== JSON.stringify(refs))
            throw new CliError('Existing drafts changed before editorial admission', { code: 'EXISTING_DRAFT_CHANGED' });
        }
        result = await runBulk(client, refs, { ...(childExists ? { resumePath: slice.childPath } : { checkpointPath: slice.childPath }),
          execute: true, freshEditorial: flowPolicy.freshEditorial, pollMs, maxPolls, signal, onProgress });
        status = result.mode; child = result.checkpoint;
        if (status === 'stopped') {
          status = child.stopReason === 'editorial-review-held' ? 'editorial-held'
            : result.summary.unknown ? 'needs-reconciliation' : 'held';
          if (status === 'held') error = child.error || { code: 'BULK_STOPPED', message: child.stopReason };
        }
      } else {
      if (savedChild?.phase === 'assistant-running' && hasPrepareScopeReservation(savedChild.prepareJobId, savedChild.prepareScopeReservation))
        startLookahead(preparingSlices, slice, { early: true });
      if (['prepared', 'editorial-reviewed', 'approved'].includes(savedChild?.phase)
        && hasPrepareScopeReservation(savedChild.prepareJobId, savedChild.prepareScopeReservation)) startLookahead(preparingSlices, slice);
      result = await runWorkflow(client, slice.itemIds, { checkpointPath: childExists ? undefined : slice.childPath, resumePath: childExists ? slice.childPath : undefined, instruction, materialsAlreadyRefreshed: true,
        planAlreadyChecked: Number.isInteger(slice.plannedBytes), autonomous, execute, workflowMode: mode, workflowId: state.workflowId, reconcileUnknown: execute, pollMs, maxPolls, signal, ...flowPolicy,
        onProgress: event => {
          onProgress(event);
          if (hasPrepareScopeReservation(event.jobId, event.scopeReservation)) {
            if (event.event === 'prepare.admitted') startLookahead(preparingSlices, slice, { early: true });
            if (event.event === 'prepare.completed') startLookahead(preparingSlices, slice);
          }
        } });
      status = result.mode; child = result.checkpoint;
      if (flowPolicy.continueHeld && status === 'needs-reconciliation') {
        const closure = await confirmedExecutionClosure(client, child);
        if (closure?.some(row => row.status === 'unknown')) { status = 'quarantined'; child = { ...child, operations: closure }; }
      }
      }
    } catch (cause) {
      if (connectionFailure(cause)) {
        await save(path, connectionWaitState(current, cause));
        throw cause;
      }
      status = cause.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'held'; error = checkpointError(cause);
      try { child = await readCheckpoint(slice.childPath); } catch {}
    }
    const safe = settledSlice({ error, status });
    // Stop observing future preparation on UNKNOWN/failure/cancel; its exact
    // paid job remains recoverable. Never let a detached producer outlive us.
    const produced = safe ? await producers.joinReady() : await producers.joinAll({ stop: true });
    if (produced.length) current = withProduced(current, produced);
    const nextSlice = { ...slice, status, error, child };
    const exists = (current.slices || []).some(row => row.id === slice.id);
    const slices = exists ? current.slices.map(row => row.id === slice.id ? nextSlice : row) : [...(current.slices || []), nextSlice];
    return save(path, { ...current, currentSlice: null,
      coverageNeedsRefresh: true,
      attemptedItemIds: [...new Set([...(current.attemptedItemIds || []), ...slice.itemIds])],
      slices });
  };

  let lastSnapshot = null;
  if (state.currentSlice) {
    const recovering = state.currentSlice;
    state = await processSlice(state, recovering);
    const recovered = state.slices.find(slice => slice.id === recovering.id);
    const blocked = (preserveOverlapQuarantine || flowPolicy.continueHeld) && !settledSlice(recovered);
    state = await save(path, { ...state,
      ...(state.pendingSlices?.[0]?.id === recovering.id ? { pendingSlices: state.pendingSlices.slice(1) } : {}),
      ...(blocked ? { phase: 'stopped', stopReason: 'overlapped-slice-unresolved' } : {}) });
    if (blocked) {
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
  }
  if (state.stopReason === 'overlapped-slice-unresolved') {
    const unresolved = [...state.slices].reverse().find(slice => !settledSlice(slice));
    if (unresolved) {
      state = await processSlice(state, unresolved);
      const recovered = state.slices.find(slice => slice.id === unresolved.id);
      if (!settledSlice(recovered))
        return { mode: 'stopped', stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
    state = await save(path, { ...state, phase: 'running', stopReason: null });
  }
  const processPending = async (intake = null) => {
    while (state.pendingSlices?.length || intake && !intake.done) {
      if (intake) await intake.pull({ wait: !state.pendingSlices?.length });
      if (!state.pendingSlices?.length) continue;
      state = withProduced(state, await producers.joinReady());
      while (producers.has(state.pendingSlices[0].id)) {
        // Oldest ready family wins. A slow earlier model is still observed by
        // its producer, so never give its checkpoint to a second writer.
        const ready = state.pendingSlices.findIndex(row => row.preparationReady || row.preparationError);
        if (ready >= 0) {
          const selected = state.pendingSlices[ready];
          // Publish the new head before processSlice takes ownership. Existing
          // crash recovery can then resume that exact child without guessing.
          state = await save(path, { ...state, pendingSlices: [selected, ...state.pendingSlices.filter(row => row.id !== selected.id)] });
          break;
        }
        if (intake && !intake.done) {
          // Planning readers never consume producer results or write journals.
          // Wake on either event, then this single queue owner adopts it.
          await Promise.race([producers.waitReady(), intake.waitReady()]);
          await intake.pull({ wait: false });
          state = await save(path, withProduced(state, await producers.joinReady()));
        } else state = await save(path, withProduced(state, await producers.joinReady({ wait: true })));
      }
      const slice = state.pendingSlices[0];
      if (slice.planHoldReason) {
        state = await save(path, { ...state, pendingSlices: state.pendingSlices.slice(1),
          attemptedItemIds: [...new Set([...(state.attemptedItemIds || []), ...slice.itemIds])],
          slices: [...(state.slices || []), { ...slice, status: 'plan-held', child: null, error: null }] });
        continue;
      }
      state = await processSlice(state, slice, state.pendingSlices.slice(1));
      const settled = state.slices.find(row => row.id === slice.id);
      const blocked = (preserveOverlapQuarantine || flowPolicy.continueHeld) && !settledSlice(settled);
      // Remove the old head and publish its quarantine in the same rename.
      // A crash must never expose B as next while A's stop marker is absent.
      state = await save(path, { ...state, pendingSlices: state.pendingSlices.slice(1),
        ...(blocked ? { phase: 'stopped', stopReason: 'overlapped-slice-unresolved' } : {}) });
      if (blocked) return false;
    }
    return true;
  };
  if (!await processPending()) return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
  if (completedAtStart) {
    const upgradeIds = state.slices.filter(slice => needsStageUpgrade(slice, autonomous, execute)).map(slice => slice.id);
    let upgradeFailed = false;
    for (const id of upgradeIds) {
      const slice = state.slices.find(row => row.id === id);
      state = await processSlice(state, slice);
      const updated = state.slices.find(row => row.id === id);
      if (updated?.error || needsStageUpgrade(updated, autonomous, execute) && !settledSlice(updated)) upgradeFailed = true;
    }
    if (upgradeFailed) {
      state = await save(path, { ...state, phase: 'stopped', stopReason: 'stage-upgrade-incomplete' });
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
    // The old completion claim predates approval/execution and its required refresh.
    // Re-enter the bounded queue walk so newly observed comments cannot be skipped.
    state = await save(path, { ...state, phase: 'running', stopReason: null, previousStopReason: completedStopReason });
  }

  while (Number(state.syncCycles || 0) < maxCycles) {
    ({ state, snapshot: lastSnapshot } = await freshSync(client, state, path, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) }));
    if (state.frontierStalled) {
      state = await save(path, { ...state, phase: 'stopped', stopReason: 'provider-stalled' });
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
    const quarantined = unknownQueueBlockers(lastSnapshot);
    const unknownRows = [...(lastSnapshot.operations || []), ...(lastSnapshot.proposals || [])].filter(row => row.status === 'unknown');
    for (const row of unknownRows) if (typeof row.itemId === 'string') quarantined.add(row.itemId);
    if (flowPolicy.continueHeld || typeof waitForDependencies === 'function') {
      const quarantinedItemIds = [...quarantined].filter(id => !scope || scope.has(id));
      const conversations = new Set((lastSnapshot.items || []).filter(row => quarantinedItemIds.includes(row.id)).map(row => row.conversationKey).filter(Boolean));
      const quarantinedOperations = (lastSnapshot.operations || []).filter(row => row.status === 'unknown' && (!scope
        || quarantinedItemIds.includes(row.itemId) || [row.conversationKey, row.action?.conversationKey, row.target?.conversationKey]
          .some(key => conversations.has(key))))
        .map(({ id, proposalId, itemId, status }) => ({ id, proposalId, itemId, status }));
      state = await save(path, { ...state, quarantinedItemIds, quarantinedOperations });
    }
    const attempted = new Set(state.attemptedItemIds || []);
    if (adoptCurrentDrafts) {
      const window = (lastSnapshot.items || []).filter(row => !attempted.has(row.id) && (!scope || scope.has(row.id))
        && (!cutoffUtc || Number.isFinite(Date.parse(row.createdAt)) && Date.parse(row.createdAt) <= Date.parse(cutoffUtc))
        && (row.workflow === 'prepared' || (lastSnapshot.proposals || []).some(p => p.itemId === row.id && ACTIVE_PROPOSALS.has(p.status))))
        .sort((a, b) => String(a.createdAt || '').localeCompare(String(b.createdAt || '')) || String(a.id).localeCompare(String(b.id)))
        .slice(0, batchSize);
      if (window.length) {
        const discovery = currentDrafts(await client.reviewItems(window.map(row => row.id)), window.map(row => row.id), { runId: state.conductorRunId });
        const cycle = state.cycle + 1;
        const held = discovery.held.map((row, index) => ({ id: `cycle-${cycle}-draft-held-${index + 1}`,
          itemIds: [row.itemId], planHoldReason: row.reason, status: 'plan-held', child: null, error: null }));
        const candidates = discovery.candidates;
        state = await save(path, { ...state, cycle, slices: [...(state.slices || []), ...held],
          attemptedItemIds: [...new Set([...(state.attemptedItemIds || []), ...discovery.held.map(row => row.itemId)])],
          pendingSlices: candidates.length ? [{ id: `cycle-${cycle}-drafts`, itemIds: candidates.map(row => row.itemId),
            adoptedDrafts: candidates, childPath: `${path}.slices/cycle-${cycle}-drafts.json` }] : [] });
        for (const row of discovery.held) attempted.add(row.itemId);
        if (candidates.length) {
          onProgress({ event: 'drafts.adopted', count: candidates.length });
          if (!await processPending()) return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
          continue;
        }
      }
    }
    const rows = eligible(lastSnapshot, attempted).filter(row => (!scope || scope.has(row.id))
      && (!cutoffUtc || Number.isFinite(Date.parse(row.createdAt)) && Date.parse(row.createdAt) <= Date.parse(cutoffUtc)));
    if (!rows.length) {
      const known = queueCoverage(lastSnapshot); const complete = known.complete;
      if (known.pending) continue;
      if (scope && complete && typeof waitForDependencies === 'function') {
        if (signal?.aborted) throw new CliError('Stopped before dependency observation', { code: 'STOPPED' });
        const attemptedModels = new Set((state.slices || []).filter(slice => slice.child?.prepareJobId || slice.child?.prepareRequestId
          || slice.child?.pendingLocalAdmission || slice.child?.pendingRepair || slice.child?.proposals?.length
          || slice.child?.operations?.length).flatMap(slice => slice.itemIds || []));
        const occupied = new Set([...(lastSnapshot.operations || []), ...(lastSnapshot.proposals || [])].map(row => row.itemId));
        const heldSlices = (state.slices || []).filter(slice => slice.status === 'plan-held' && !slice.dependencyResolved
          && ['media_wait', 'media_pending', 'media_unavailable'].includes(slice.planHoldReason) && !slice.child
          && slice.itemIds.length === 1 && scope.has(slice.itemIds[0]) && attempted.has(slice.itemIds[0])
          && !attemptedModels.has(slice.itemIds[0]) && !occupied.has(slice.itemIds[0]) && !quarantined.has(slice.itemIds[0]));
        const factDependencies = [];
        for (const slice of state.slices || []) {
          if (slice.error || !slice.child?.prepareJobId) continue;
          for (const row of slice.child.prepareOutcome?.factDependencies || []) {
            if (row.kind !== 'missing_public_fact' || row.consumedByJobId != null
              || (slice.dependencyResolvedItemIds || []).includes(row.itemId)
              || !slice.itemIds.includes(row.itemId) || !scope.has(row.itemId) || !attempted.has(row.itemId)
              || occupied.has(row.itemId) || quarantined.has(row.itemId)) continue;
            factDependencies.push({ ...row, prepareJobId: slice.child.prepareJobId });
          }
        }
        const heldItemIds = [...heldSlices.map(slice => slice.itemIds[0]), ...factDependencies.map(row => row.itemId)];
        if (new Set(heldItemIds).size !== heldItemIds.length)
          throw new CliError('Dependency holds contain duplicate recipient ownership', { code: 'INVALID_CHECKPOINT' });
        if (heldItemIds.length) {
          const ready = await waitForDependencies({ heldItemIds, heldSlices, factDependencies, checkpoint: state, snapshot: lastSnapshot, signal });
          if (!Array.isArray(ready) || new Set(ready).size !== ready.length || ready.some(id => !heldItemIds.includes(id)))
            throw new CliError('Dependency readiness differs from exact typed hold scope', { code: 'INVALID_DEPENDENCY_SCOPE' });
          if (ready.length) {
            if (signal?.aborted) throw new CliError('Stopped before dependency requeue', { code: 'STOPPED' });
            const returned = new Set(ready);
            const ids = [...heldSlices.filter(slice => returned.has(slice.itemIds[0])).map(slice => slice.id),
              ...(state.slices || []).filter(slice => factDependencies.some(row => row.prepareJobId === slice.child?.prepareJobId
                && returned.has(row.itemId))).map(slice => slice.id)];
            // One atomic transition releases exact recipients. Fact evidence
            // enters a NEW preparation; siblings retain the old hold cursor.
            state = await save(path, { ...state, phase: 'running', stopReason: null,
              attemptedItemIds: state.attemptedItemIds.filter(id => !returned.has(id)),
              slices: state.slices.map(slice => ids.includes(slice.id) ? { ...slice,
                ...(slice.planHoldReason ? { dependencyResolved: true } : {}),
                dependencyResolvedItemIds: [...new Set([...(slice.dependencyResolvedItemIds || []), ...slice.itemIds.filter(id => returned.has(id))])] } : slice),
              dependencyRequeues: [...(state.dependencyRequeues || []), { itemIds: ready, sliceIds: ids }],
              scopeHolds: (state.scopeHolds || []).filter(row => !returned.has(row.itemId)) });
            onProgress({ event: 'dependency.requeued', itemIds: ready, sliceIds: ids });
            // Re-read canonical queue/UNKNOWN state before another admission.
            continue;
          }
        }
      }
      if (scope && complete) {
        const observed = new Map((lastSnapshot.items || []).map(row => [row.id, row]));
        const scopeHolds = scopeItemIds.filter(id => !attempted.has(id)).map(itemId => {
          const row = observed.get(itemId);
          const reason = quarantined.has(itemId) ? 'unknown-conversation-held' : !row ? 'scope-item-not-observed' : cutoffUtc && (!Number.isFinite(Date.parse(row.createdAt))
            || Date.parse(row.createdAt) > Date.parse(cutoffUtc)) ? 'scope-cutoff-excluded' : 'scope-item-not-eligible';
          return { itemId, reason };
        });
        state = await save(path, { ...state, scopeHolds });
      }
      const executionReason = complete ? state.quarantinedItemIds?.length ? 'operation-outcomes-unresolved'
        : executionStopReason(state, execute, flowPolicy.continueHeld) : null;
      state = await save(path, { ...state, phase: complete && !executionReason ? reviewOnly ? READY_FOR_OWNER_APPROVAL : 'complete' : 'stopped', coverage: known,
        stopReason: complete ? executionReason || 'known-complete-no-eligible' : known.reason || 'unknown-open-coverage' });
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
    // A conductor discovers confirmed publication families across its finite
    // remaining scope before any oldest-recipient window truncates that scope.
    // Ordinary clients retain their existing explicit-selection contract.
    const familyWindows = typeof client.selectPrepareFamilies === 'function'
      ? await client.selectPrepareFamilies(rows.map(row => row.id), batchSize, { maxBatches: overlap ? Math.max(2, workerWidth) : 1 }) : null;
    const byId = new Map(rows.map(row => [row.id, row]));
    const first = familyWindows ? familyWindows[0].map(id => byId.get(id)) : rows.slice(0, batchSize);
    const firstKeys = new Set(first.flatMap(knownBranchKeys));
    // Prefer actually independent known branches. The backend still derives
    // authoritative aliases; skipped same-branch rows stay eligible next sync.
    const windows = [first];
    if (overlap) for (const ids of familyWindows ? familyWindows.slice(1) : [rows.slice(batchSize).map(row => row.id)]) {
      const window = ids.map(id => byId.get(id)).filter(row => !knownBranchKeys(row).some(key => firstKeys.has(key))).slice(0, batchSize);
      if (!window.length) continue;
      windows.push(window); for (const key of window.flatMap(knownBranchKeys)) firstKeys.add(key);
    }
    const cycle = state.cycle + 1;
    state = await save(path, { ...state, cycle, phase: 'running', pendingSlices: [] });
    planning = createQueuePreparePlanning(client, windows.map(window => window.map(row => row.id)),
      instruction ?? DEFAULT_INSTRUCTION, { parallelism: overlap ? workerWidth : 1, signal });
    let firstProducer = null, firstAcknowledged = false;
    const intake = {
      get done() { return planning.done; },
      waitReady: () => planning.waitReady(),
      async pull({ wait }) {
        let plan = await planning.next({ wait });
        while (plan) {
          const window = plan.window;
          const added = [
            ...plan.batches.map((batch, index) => {
              // Identity depends on the original window, never completion order.
              const id = `cycle-${cycle}-window-${window + 1}-slice-${index + 1}`;
              return { id, itemIds: batch.itemIds, plannedBytes: batch.bytes,
                ...(familyWindows || plan.strictGroupContract === 'strict_post_family_v1'
                  ? { familyPlanVersion: 1, familyWindow: `${cycle}:${window}` } : {}),
                childPath: `${path}.slices/${id}.json` };
            }),
            ...plan.held.map((held, index) => ({ id: `cycle-${cycle}-window-${window + 1}-held-${index + 1}`,
              itemIds: [held.itemId], planHoldReason: held.reason,
              ...(typeof held.detail === 'string' ? { planHoldDetail: held.detail } : {}) }))
          ];
          // Persist exact child identities before the first producer POST.
          state = await save(path, { ...state, pendingSlices: [...state.pendingSlices, ...added] });
          onProgress({ event: 'prepare.plan.ready', queueCycle: cycle, window, itemIds: added.flatMap(row => row.itemIds) });
          if (!firstProducer && overlap && workerWidth > 1) {
            firstProducer = added.find(row => !row.planHoldReason && row.familyPlanVersion === 1) || null;
            if (firstProducer) startProducer(firstProducer, firstProducer.familyWindow, () => {
              firstAcknowledged = true;
              startLookahead(state.pendingSlices, firstProducer, { early: true });
            });
          }
          if (firstAcknowledged) startLookahead(state.pendingSlices, firstProducer, { early: true });
          plan = await planning.next({ wait: false });
        }
      }
    };
    if (!await processPending(intake)) return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    await planning.joinAll(); planning = null;
    // The next cycle's sync is the one fresh source read after this planned
    // group. Never infer post-dispatch coverage from the pre-slice snapshot.
  }
  const observedCoverage = lastSnapshot ? queueCoverage(lastSnapshot) : state.coverage;
  const boundedCoverage = state.coverageNeedsRefresh
    ? { ...observedCoverage, known: false, complete: false, pending: false, reason: 'post-slice-sync-required' }
    : observedCoverage;
  state = await save(path, { ...state, phase: 'stopped', stopReason: 'max-cycles', coverage: boundedCoverage });
  return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
  } catch (error) {
    if (connectionFailure(error)) {
      let latest = state;
      try { latest = await readCheckpoint(path); }
      catch (missing) { if (missing.code !== 'INVALID_CHECKPOINT' || missing.details?.cause !== 'ENOENT') throw missing; }
      await save(path, connectionWaitState(latest, error));
    }
    throw error;
  } finally {
    // Any thrown read/checkpoint error also ends this observer lifetime. Paid
    // server jobs and their child checkpoints remain available for recovery.
    await Promise.all([planning?.joinAll({ stop: true }), producers.joinAll({ stop: true })]);
  }
}

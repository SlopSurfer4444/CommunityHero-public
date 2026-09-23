import { readFile } from 'node:fs/promises';
import { CliError, waitForJob } from './client.mjs';
import { readCheckpoint, runWorkflow, writeCheckpoint } from './workflow.mjs';

const ACTIVE_PROPOSALS = new Set(['draft', 'approved', 'dispatching', 'unknown', 'succeeded']);
const ACTIVE_OPERATIONS = new Set(['dispatching', 'unknown', 'succeeded']);
const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');

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

function eligible(snapshot, attempted) {
  const blocked = new Set();
  for (const row of snapshot.proposals || []) if (ACTIVE_PROPOSALS.has(row.status)) blocked.add(row.itemId);
  for (const row of snapshot.operations || []) if (ACTIVE_OPERATIONS.has(row.status)) blocked.add(row.itemId);
  return (snapshot.items || []).filter(item => item?.id && item.workflow === 'attention'
    && (!item.providerStatus || ['new', 'inprogress'].includes(item.providerStatus))
    && !blocked.has(item.id) && !attempted.has(item.id))
    .sort((a, b) => String(a.createdAt || '').localeCompare(String(b.createdAt || '')) || String(a.id).localeCompare(String(b.id)));
}

function needsStageUpgrade(slice, autonomous, execute) {
  if (!autonomous) return false;
  const phase = slice.child?.phase;
  if (phase === 'prepared') return true;
  return execute && ['approved', 'executing', 'needs-reconciliation'].includes(phase);
}

async function save(path, state) { return path ? writeCheckpoint(path, state) : state; }

async function freshSync(client, state, path, poll) {
  if (!state.syncJobId) {
    try {
      const launched = await client.sync({}); state = await save(path, { ...state, syncJobId: launched.jobId, phase: 'syncing' });
    } catch (error) {
      state = await save(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', stopReason: 'sync-launch-failed', error: { code: error.code, message: error.message } });
      throw error;
    }
  }
  const settled = await waitForJob(client, state.syncJobId, poll);
  const currentCoverage = queueCoverage(settled.snapshot);
  const stalled = currentCoverage.pending && state.lastFrontierSignature === currentCoverage.signature;
  state = await save(path, { ...state, phase: 'running', syncJobId: null, coverage: currentCoverage,
    lastFrontierSignature: currentCoverage.signature, frontierStalled: stalled, syncCycles: Number(state.syncCycles || 0) + 1 });
  return { state, snapshot: settled.snapshot };
}

export function summarizeQueue(state) {
  const operations = new Map();
  let held = 0; let prepared = 0; let unresolvedTransport = 0; let sliceFailures = 0; let unresolvedItems = 0;
  for (const slice of state.slices || []) {
    const child = slice.child || {};
    const proposalById = new Map((child.proposals || []).map(row => [row.id, row]));
    const proposedItemIds = new Set((child.proposals || []).map(row => row.itemId));
    const operationItemIds = new Set();
    prepared += (child.proposals || []).length;
    for (const op of child.operations || []) {
      const proposal = proposalById.get(op.proposalId);
      if (proposal?.itemId) operationItemIds.add(proposal.itemId);
      operations.set(op.id, { ...op, kind: proposal?.kind });
    }
    for (const itemId of new Set(slice.itemIds || [])) {
      if (operationItemIds.has(itemId)) continue;
      if (slice.status === 'unknown') unresolvedTransport += 1;
      else if (slice.error) sliceFailures += 1;
      else if (child.executeJobId && proposedItemIds.has(itemId)) unresolvedItems += 1;
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
    unresolvedTransport, sliceFailures, unresolvedItems, prepared, slices: (state.slices || []).length };
}

export async function runQueue(client, options) {
  const { checkpointPath, resumePath, batchSize = 60, maxCycles = 1000, autonomous = false, execute = false, instruction, pollMs, maxPolls, signal, onProgress = () => {} } = options;
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
    createdAt: new Date().toISOString(), cycle: 0, attemptedItemIds: [], slices: [], batchSize, maxCycles
  };
  if (state.kind !== 'communityhero-queue' || canonical(state.account) !== canonical(client.account) || state.baseUrl !== client.baseUrl)
    throw new CliError('Queue checkpoint does not match this account/server', { code: 'WRONG_ACCOUNT' });
  if (state.phase === 'unknown') throw new CliError('Queue checkpoint has an unknown sync outcome; inspect jobs before resuming', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const completedAtStart = state.phase === 'complete';
  const completedStopReason = state.stopReason;
  if (completedAtStart && !(state.slices || []).some(slice => needsStageUpgrade(slice, autonomous, execute)))
    return { mode: 'complete', stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };

  // A fresh database has no account policy. Import it before any model request.
  if (!state.materialsReady) {
    if (!state.materialsJobId) {
      try {
        const launched = await client.importMaterials(); state = await save(path, { ...state, materialsJobId: launched.jobId, phase: 'materials-running' });
      } catch (error) {
        state = await save(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', stopReason: 'materials-import-launch-failed', error: { code: error.code, message: error.message } });
        throw error;
      }
    }
    const imported = await waitForJob(client, state.materialsJobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    if (!(imported.snapshot.materials || []).some(row => row.imported === true && row.kind === 'knowledge')) {
      state = await save(path, { ...state, phase: 'stopped', stopReason: 'materials-unavailable' });
      throw new CliError('Imported account policy materials are unavailable; queue generation was not started', { code: 'MATERIALS_UNAVAILABLE' });
    }
    state = await save(path, { ...state, materialsReady: true, materialsJobId: null, phase: 'running' });
  }

  const processSlice = async (current, slice) => {
    current = await save(path, { ...current, currentSlice: slice });
    let result; let status = 'complete'; let error = null; let child = null;
    try {
      let childExists = false; try { await readFile(slice.childPath, 'utf8'); childExists = true; } catch (e) { if (e.code !== 'ENOENT') throw e; }
      result = await runWorkflow(client, slice.itemIds, { checkpointPath: childExists ? undefined : slice.childPath, resumePath: childExists ? slice.childPath : undefined, instruction, materialsAlreadyRefreshed: true, autonomous, execute, reconcileUnknown: execute, pollMs, maxPolls, signal, onProgress });
      status = result.mode; child = result.checkpoint;
    } catch (cause) {
      status = cause.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'held'; error = { code: cause.code, message: cause.message };
      try { child = await readCheckpoint(slice.childPath); } catch {}
    }
    const nextSlice = { ...slice, status, error, child };
    const exists = (current.slices || []).some(row => row.id === slice.id);
    const slices = exists ? current.slices.map(row => row.id === slice.id ? nextSlice : row) : [...(current.slices || []), nextSlice];
    return save(path, { ...current, currentSlice: null,
      attemptedItemIds: [...new Set([...(current.attemptedItemIds || []), ...slice.itemIds])],
      slices });
  };

  if (state.currentSlice) {
    state = await processSlice(state, state.currentSlice);
    ({ state } = await freshSync(client, state, path, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) }));
  }

  let lastSnapshot = null;
  if (completedAtStart) {
    const upgradeIds = state.slices.filter(slice => needsStageUpgrade(slice, autonomous, execute)).map(slice => slice.id);
    let upgradeFailed = false;
    for (const id of upgradeIds) {
      const slice = state.slices.find(row => row.id === id);
      state = await processSlice(state, slice);
      const updated = state.slices.find(row => row.id === id);
      if (updated?.error || needsStageUpgrade(updated, autonomous, execute)) upgradeFailed = true;
      ({ state, snapshot: lastSnapshot } = await freshSync(client, state, path, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) }));
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
    const attempted = new Set(state.attemptedItemIds || []); const rows = eligible(lastSnapshot, attempted);
    if (!rows.length) {
      const known = queueCoverage(lastSnapshot); const complete = known.complete;
      if (known.pending) continue;
      state = await save(path, { ...state, phase: complete ? 'complete' : 'stopped', coverage: known, stopReason: complete ? 'known-complete-no-eligible' : known.reason || 'unknown-open-coverage' });
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
    const selected = rows.slice(0, batchSize); state = await save(path, { ...state, cycle: state.cycle + 1, phase: 'running' });
    const sliceLimit = 100;
    for (let offset = 0; offset < selected.length; offset += sliceLimit) {
      const itemIds = selected.slice(offset, offset + sliceLimit).map(row => row.id);
      const sliceId = `cycle-${state.cycle}-slice-${Math.floor(offset / sliceLimit) + 1}`;
      const childPath = `${path}.slices/${sliceId}.json`;
      state = await processSlice(state, { id: sliceId, itemIds, childPath });
      ({ state, snapshot: lastSnapshot } = await freshSync(client, state, path, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) }));
    }
    const after = queueCoverage(lastSnapshot);
    if (!after.complete && !after.pending) {
      state = await save(path, { ...state, phase: 'stopped', coverage: after, stopReason: after.reason || 'unknown-open-coverage' });
      return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
    }
  }
  state = await save(path, { ...state, phase: 'stopped', stopReason: 'max-cycles', coverage: lastSnapshot ? queueCoverage(lastSnapshot) : state.coverage });
  return { mode: state.phase, stopReason: state.stopReason, coverage: state.coverage, counts: summarizeQueue(state), checkpoint: state };
}

import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { CliError, localAdmissionPayloadHash } from './client.mjs';
import { verifyRepairChain } from './editorial.mjs';
import { connectionFailure } from './conductor-connection.mjs';

const invalid = () => new CliError('Durable preparation group binding is invalid', { code: 'INVALID_PREPARE_JOB' });
const reference = row => ({ id: row.id, revision: row.revision, itemId: row.itemId, kind: row.kind, text: row.text });
const exact = rows => rows.map(({ id, revision, itemId }) => ({ id, revision, itemId })).sort((a, b) => a.id.localeCompare(b.id));

export function readyBatchStopped(error, jobId) {
  if (connectionFailure(error)) return error;
  const code = error?.details?.causeCode || error?.code;
  const causeCode = typeof code === 'string' && /^[A-Z][A-Z0-9_]{0,63}$/u.test(code) ? code : 'CLI_ERROR';
  const status = error?.details?.causeStatus ?? error?.status;
  const causeStatus = Number.isInteger(status) && status >= 100 && status <= 599 ? status : null;
  // The queue retains this bounded message. Never retain transport bodies,
  // model errors, request arguments or capabilities from the nested failure.
  return new CliError(`Ready-batch drain stopped; inspect its existing checkpoint (${causeCode}${causeStatus === null ? '' : `/${causeStatus}`})`, {
    code: 'READY_BATCH_STOPPED', details: { jobId, causeCode, ...(causeStatus === null ? {} : { causeStatus }) }
  });
}

// Only acknowledged execution with complete canonical operation closure can
// quarantine a child and release independent work. An uncertain POST cannot.
export async function confirmedExecutionClosure(client, child) {
  if (!child?.executeJobId || !child.approvalId || child.pendingLocalAdmission || child.pendingRepair
    || !Array.isArray(child.approvedProposals) || !child.approvedProposals.length) return null;
  const job = await client.getJob(child.executeJobId);
  if (job?.id !== child.executeJobId || job.kind !== 'execute' || job.refId !== child.approvalId
    || !['completed', 'interrupted'].includes(job.status)) return null;
  const refs = child.approvedProposals;
  if (new Set(refs.map(ref => ref.id)).size !== refs.length) return null;
  const current = await client.reviewItems(child.itemIds);
  if (current.coverage?.operationsComplete !== true) return null;
  const saved = new Map((child.operations || []).map(row => [row.proposalId, row]));
  const operations = [];
  let capturedActor = null;
  for (const ref of refs) {
    const proposal = child.proposals.find(row => row.id === ref.id && row.revision === ref.revision);
    const rows = (current.operations || []).filter(row => row.proposalId === ref.id);
    const previous = saved.get(ref.id);
    const observedProposals = (current.proposals || []).filter(row => row.id === ref.id);
    if (!proposal || observedProposals.length !== 1 || observedProposals[0].revision !== ref.revision
      || child.prepareJobId && observedProposals[0].prepareRunId !== child.prepareJobId
      || observedProposals[0].itemId !== proposal.itemId || rows.length !== 1 || previous && previous.id !== rows[0].id || rows[0].itemId !== proposal.itemId
      || !['succeeded', 'failed', 'stale', 'unknown'].includes(rows[0].status)) return null;
    const op = rows[0]; const strict = job.status === 'interrupted' || !previous;
    if (strict && op.approvalId !== child.approvalId || op.approvalId !== undefined && op.approvalId !== child.approvalId) return null;
    const itemRows = (current.items || []).filter(row => row.id === proposal.itemId);
    if (strict && (itemRows.length !== 1 || !op.target || !op.action || !op.approvedBy || !op.executedBy)) return null;
    if (op.target || op.action) {
      if (itemRows.length !== 1 || !op.target || !op.action || op.target.id !== proposal.itemId || op.action.actionId !== op.id
        || op.action.action !== proposal.kind) return null;
      for (const key of ['itemId', 'objectId', 'conversationKey']) {
        if (typeof op.target[key] !== 'string' || !op.target[key] || op.action[key] !== op.target[key] || itemRows[0][key] !== op.target[key]) return null;
      }
      if (op.target.postKey !== undefined && op.target.postKey !== itemRows[0].postKey) return null;
      if (!op.target.connectorBinding || typeof op.target.connectorBinding !== 'object' || Array.isArray(op.target.connectorBinding)
        || !itemRows[0].connectorBinding || typeof itemRows[0].connectorBinding !== 'object' || Array.isArray(itemRows[0].connectorBinding)) return null;
      const binding = value => Object.fromEntries(Object.entries(value).filter(([key]) => key !== 'revision'));
      if (localAdmissionPayloadHash(binding(op.target.connectorBinding)) !== localAdmissionPayloadHash(binding(itemRows[0].connectorBinding))) return null;
      if (proposal.kind === 'reply_and_close' && (op.action.reply !== proposal.text || observedProposals[0].text !== proposal.text)) return null;
    }
    if (op.approvedBy || op.executedBy) {
      const approved = op.approvedBy; const executed = op.executedBy;
      if (typeof approved?.id !== 'string' || !approved.id || approved.id !== executed?.id
        || approved.role !== executed.role || approved.authorityGeneration !== executed.authorityGeneration
        || approved.role !== undefined && (typeof approved.role !== 'string' || !approved.role)
        || approved.authorityGeneration !== undefined && (typeof approved.authorityGeneration !== 'string' || !approved.authorityGeneration)) return null;
      const actor = { id: approved.id, ...(approved.role === undefined ? {} : { role: approved.role }),
        ...(approved.authorityGeneration === undefined ? {} : { authorityGeneration: approved.authorityGeneration }) };
      const signature = localAdmissionPayloadHash(actor);
      if (capturedActor && capturedActor !== signature) return null;
      capturedActor = signature;
      for (const savedActor of [child.approvedBy, child.executedBy]) {
        if (savedActor && ['id', 'role', 'authorityGeneration'].some(key => savedActor[key] !== undefined && savedActor[key] !== actor[key])) return null;
      }
    }
    if (job.conductorRunId !== undefined || job.grantGeneration !== undefined || op.conductorRunId !== undefined || op.grantGeneration !== undefined) {
      if (typeof job.conductorRunId !== 'string' || !job.conductorRunId || !Number.isSafeInteger(job.grantGeneration) || job.grantGeneration < 1
        || op.conductorRunId !== job.conductorRunId || op.grantGeneration !== job.grantGeneration) return null;
    }
    operations.push({ id: rows[0].id, proposalId: rows[0].proposalId, itemId: rows[0].itemId, status: rows[0].status });
  }
  return operations;
}

function conversationQuarantine(review) {
  const ids = new Set(); const keys = new Set(); const items = new Map((review.items || []).map(row => [row.id, row]));
  for (const row of [...(review.operations || []), ...(review.proposals || [])]) {
    if (row.status !== 'unknown') continue;
    ids.add(row.itemId);
    for (const key of [row.conversationKey, row.action?.conversationKey, row.target?.conversationKey, items.get(row.itemId)?.conversationKey])
      if (typeof key === 'string' && key) keys.add(key);
  }
  for (const item of items.values()) if (keys.has(item.conversationKey)) ids.add(item.id);
  return ids;
}

// Job payload proves admission; the selected review endpoint supplies current
// exact revisions and complete operation evidence. Never consume model output.
export async function discoverReadyProposals(client, state, job) {
  const groups = job.preparationStages?.groupAdmission;
  if (groups === undefined) return [];
  if (!Array.isArray(groups) || job.id !== state.prepareJobId || job.kind !== 'assistant'
    || !['engine_prepare', 'auto_prepare'].includes(job.purpose)) throw invalid();
  const allowed = new Set(state.itemIds); const candidates = new Map();
  for (const group of groups) {
    if (!Array.isArray(group?.itemIds) || group.itemIds.some(id => !allowed.has(id))) throw invalid();
    if (group.status !== 'admitted') continue;
    if (!Array.isArray(group.admission?.candidates)) throw invalid();
    for (const row of group.admission.candidates) {
      if (!group.itemIds.includes(row?.itemId)) throw invalid();
      if (row.status !== 'review') continue;
      if (typeof row.proposalId !== 'string' || !row.proposalId || candidates.has(row.proposalId)) throw invalid();
      candidates.set(row.proposalId, row.itemId);
    }
  }
  const observed = new Set((state.proposals || []).map(row => row.id));
  const newIds = [...candidates.keys()].filter(id => !observed.has(id));
  if (!newIds.length) return [];
  const review = await client.reviewItems(state.itemIds);
  if (review.coverage?.operationsComplete !== true) throw new CliError('Ready proposal operation coverage is incomplete', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
  return newIds.flatMap(id => {
    const rows = review.proposals.filter(row => row.id === id);
    if (rows.length !== 1) throw invalid();
    const row = rows[0];
    if (row.itemId !== candidates.get(id) || row.prepareRunId !== state.prepareJobId
      || !Number.isSafeInteger(row.revision) || row.revision < 1) throw invalid();
    if (row.status !== 'draft' || review.operations.some(op => op.itemId === row.itemId
      && ['pending', 'queued', 'dispatching', 'unknown'].includes(op.status))) return [];
    return [reference(row)];
  });
}

function batchId(state, proposals) {
  return createHash('sha256').update(JSON.stringify([state.account, state.baseUrl, state.prepareJobId, exact(proposals)])).digest('hex');
}
function validateChild(parent, batch, child) {
  if (batch.id !== batchId(parent, batch.proposals) || child.account !== parent.account || child.baseUrl !== parent.baseUrl
    || (child.workflowGeneration??null)!==(parent.workflowGeneration??null)
    || child.prepareJobId !== parent.prepareJobId || JSON.stringify(exact(child.originProposals || child.proposals)) !== JSON.stringify(exact(batch.proposals))
    || child.itemIds.length !== new Set(batch.proposals.map(row => row.itemId)).size
    || child.itemIds.some(id => !parent.itemIds.includes(id) || !batch.proposals.some(row => row.itemId === id)))
    throw new CliError('Ready-batch checkpoint scope differs from its parent', { code: 'INVALID_CHECKPOINT' });
}

function journalReady(state) {
  const batches = [...(state.readyBatches || [])];
  const assigned = new Set(batches.flatMap(batch => batch.proposals.map(row => row.id)));
  const fresh = state.proposals.filter(row => !assigned.has(row.id));
  if (fresh.length) batches.push({ id: batchId(state, fresh), proposals: fresh, mode: 'pending' });
  return { ...state, readyBatches: batches };
}

// One original preparation job, one sender consumer, one parent checkpoint
// writer. Only the producer changes preparation state; only the consumer
// changes batch outcomes. Waiting on a child must not stop observing later
// durable groups from this same job. This does not admit another prepare job.
export function createReadyBatchCoordinator(client, initial, { save, ...options }) {
  let current = initial; let writes = Promise.resolve(); let consumer = null;
  let requested = false; let failure = null;
  const update = reducer => {
    const writing = writes.then(async () => {
      current = await save(reducer(current)); return current;
    });
    writes = writing.then(() => {}, () => {});
    return writing;
  };
  const savePreparation = next => update(latest => ({ ...next,
    ...(latest?.readyBatches ? { readyBatches: latest.readyBatches } : {}) }));
  const saveBatches = next => update(latest => {
    // The producer may have journaled a later group while this child waited.
    // Update owned batch outcomes without replacing the newly appended tail.
    const changes = new Map(next.readyBatches.map(batch => [batch.id, batch]));
    return { ...latest, readyBatches: latest.readyBatches.map(batch => changes.get(batch.id) || batch) };
  });
  const start = () => {
    if (consumer || failure) return;
    consumer = (async () => {
      while (requested && !failure) {
        requested = false; await writes;
        await drainReadyBatches(client, current, { ...options, save: saveBatches });
      }
    })().catch(error => { failure = error; }).finally(() => {
      consumer = null;
      // A readiness notification can arrive between the last loop check and
      // this promise settling. Keep that wakeup without starting two senders.
      if (requested && !failure) start();
    });
  };
  return {
    savePreparation,
    async observe(next) {
      if (failure) throw failure;
      await update(latest => journalReady({ ...next,
        ...(latest?.readyBatches ? { readyBatches: latest.readyBatches } : {}) }));
      requested = true; start();
    },
    async finish() {
      while (consumer) await consumer;
      await writes;
      if (failure) throw readyBatchStopped(failure, current?.prepareJobId);
      return current;
    }
  };
}

// Parent journal is committed before creating the child or admitting any
// approval. A crash at either boundary resumes the same exact child state.
export async function drainReadyBatches(client, initial, { path, execute, save, workflow, read, write, ...options }) {
  if (!path) throw new CliError('Ready-batch drain requires a durable parent checkpoint', { code: 'INVALID_CHECKPOINT' });
  let state = initial;
  const journal = journalReady(state);
  if (journal.readyBatches.length !== (state.readyBatches || []).length) state = await save(journal);
  const batches = [...(state.readyBatches || [])];
  for (let index = 0; index < batches.length; index += 1) {
    const batch = batches[index];
    if (batch.id !== batchId(state, batch.proposals) || batch.proposals.some(row => !state.itemIds.includes(row.itemId)
      || !state.proposals.some(ref => ref.id === row.id && ref.revision === row.revision && ref.itemId === row.itemId)))
      throw new CliError('Ready-batch journal scope differs from its parent', { code: 'INVALID_CHECKPOINT' });
    if (['complete', 'complete-with-holds', 'completed-with-failures', 'no-action'].includes(batch.mode)
      || options.continueHeld && batch.mode === 'editorial-held' || batch.mode === 'approved' && !execute) continue;
    if (options.signal?.aborted) throw new CliError('Stopped before starting another ready child', { code: 'STOPPED' });
    const childPath = resolve(`${path}.ready`, `${batch.id}.json`);
    let child;
    try { child = await read(childPath); }
    catch (error) {
      if (error.code !== 'INVALID_CHECKPOINT' || error.details?.cause !== 'ENOENT') throw error;
      // A completed/unknown child cannot disappear and be recreated as new work.
      if (batch.mode !== 'pending') throw new CliError('Ready-batch checkpoint is missing', { code: 'INVALID_CHECKPOINT' });
      child = { account: state.account, baseUrl: state.baseUrl, itemIds: [...new Set(batch.proposals.map(row => row.itemId))],
        ...(Object.hasOwn(state,'workflowGeneration')?{workflowGeneration:state.workflowGeneration}:{}),
        phase: 'prepared', prepareJobId: state.prepareJobId, prepareTransport: state.prepareTransport,
        materialsReady: true, instruction: state.instruction, proposals: batch.proposals, originProposals: batch.proposals };
      validateChild(state, batch, child);
      await write(childPath, child);
    }
    validateChild(state, batch, child);
    if (child.repairs?.length || child.originProposals) await verifyRepairChain(client, child);
    if (options.continueHeld && batch.mode === 'quarantined') {
      const operations = await confirmedExecutionClosure(client, child);
      if (!operations) throw new CliError('Quarantined child lost its canonical execution closure', { code: 'READY_BATCH_STOPPED' });
      batches[index] = { ...batch, operations, mode: operations.some(row => row.status === 'unknown') ? 'quarantined'
        : operations.some(row => ['failed', 'stale'].includes(row.status)) ? 'completed-with-failures' : 'complete' };
      state = await save({ ...state, readyBatches: [...batches] });
      continue;
    }
    if (options.continueHeld) {
      const current = await client.reviewItems(state.itemIds);
      if (current.coverage?.operationsComplete !== true) throw new CliError('Selected operation coverage is incomplete', { code: 'INCOMPLETE_OPERATION_COVERAGE' });
      const quarantined = conversationQuarantine(current);
      if (child.itemIds.some(id => quarantined.has(id)) && !child.executeJobId && !child.pendingLocalAdmission) {
        batches[index] = { ...batch, mode: 'quarantined-scope', quarantinedItemIds: child.itemIds.filter(id => quarantined.has(id)) };
        state = await save({ ...state, readyBatches: [...batches] });
        continue;
      }
    }
    if (options.signal?.aborted) throw new CliError('Stopped before admitting a ready child', { code: 'STOPPED' });
    // Mark started before calling the existing workflow. Missing child on a
    // later resume then fails closed instead of recreating approval authority.
    batches[index] = { ...batch, mode: 'started' };
    state = await save({ ...state, readyBatches: [...batches] });
    if (options.signal?.aborted) throw new CliError('Stopped before admitting a ready child', { code: 'STOPPED' });
    const result = await workflow(client, child.itemIds, { ...options, autonomous: true, execute,
      resumePath: childPath, streamingChild: true });
    let mode = result.mode; let operations = result.checkpoint.operations || [];
    if (options.continueHeld && mode === 'needs-reconciliation') {
      const closure = await confirmedExecutionClosure(client, result.checkpoint);
      if (closure?.some(row => row.status === 'unknown')) { mode = 'quarantined'; operations = closure; }
    }
    batches[index] = { ...batch, mode, approvalId: result.checkpoint.approvalId || null,
      executeJobId: result.checkpoint.executeJobId || null, operations,
      held: result.checkpoint.editorialReview?.outcome?.held || [], repairs: result.checkpoint.repairs || [] };
    state = await save({ ...state, readyBatches: [...batches] });
    if (!['complete', 'complete-with-holds', 'completed-with-failures', 'no-action', 'approved'].includes(mode)
      && !(options.continueHeld && ['editorial-held', 'quarantined'].includes(mode)))
      throw new CliError('Ready-batch requires inspection before more work', { code: 'READY_BATCH_STOPPED', details: { batchId: batch.id, mode: result.mode } });
  }
  return state;
}

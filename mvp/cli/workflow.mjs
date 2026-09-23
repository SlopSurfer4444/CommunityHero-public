import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { CliError, waitForJob } from './client.mjs';

const VERSION = 1;
const DEFAULT_INSTRUCTION = 'Подготовь безопасные предложения для выбранных комментариев. Для каждого выбери ответ и закрытие, закрытие без ответа или явно объясни, почему нужно участие человека. Ничего не публикуй.';

export async function readCheckpoint(path) {
  let value;
  try { value = JSON.parse(await readFile(path, 'utf8')); }
  catch (error) { throw new CliError(`Cannot read checkpoint ${path}`, { code: 'INVALID_CHECKPOINT', details: { cause: error.code || error.name } }); }
  if (value?.schemaVersion !== VERSION) throw new CliError('Unsupported checkpoint version', { code: 'INVALID_CHECKPOINT' });
  return value;
}

export async function writeCheckpoint(path, value) {
  const target = resolve(path); await mkdir(dirname(target), { recursive: true });
  const tmp = `${target}.${process.pid}.tmp`; const next = { ...value, schemaVersion: VERSION, updatedAt: new Date().toISOString() };
  await writeFile(tmp, `${JSON.stringify(next, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  await rename(tmp, target); return next;
}

function validateResume(client, state) {
  const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
  if (canonical(state.account) !== canonical(client.account)) throw new CliError('Checkpoint account does not match --account', { code: 'WRONG_ACCOUNT' });
  if (state.baseUrl !== client.baseUrl) throw new CliError('Checkpoint server does not match --base-url', { code: 'WRONG_SERVER' });
  if (state.phase === 'unknown') throw new CliError('Checkpoint contains an unknown mutation outcome; inspect server state before continuing', { code: 'UNKNOWN_MUTATION_OUTCOME' });
}

async function persistFailure(path, state, error) {
  if (path) await writeCheckpoint(path, { ...state, phase: error.code === 'UNKNOWN_MUTATION_OUTCOME' ? 'unknown' : 'stopped', error: { code: error.code, message: error.message } });
}

export async function generateProposals(client, itemIds, { instruction = DEFAULT_INSTRUCTION, checkpointPath, checkpoint = null, materialsAlreadyRefreshed = false, pollMs, maxPolls, signal, onProgress = () => {} } = {}) {
  let state = checkpoint || { account: client.account, baseUrl: client.baseUrl, phase: 'starting', itemIds: [...new Set(itemIds)], instruction, proposals: [] };
  if (!state.itemIds.length || state.itemIds.length > 100) throw new CliError('Preparation requires 1 to 100 exact --item values', { code: 'USAGE' });
  const snapshot = await client.bootstrap();
  for (const id of state.itemIds) if (!(snapshot.items || []).some(row => row.id === id)) throw new CliError(`Item ${id} not found`, { code: 'ITEM_NOT_FOUND' });
  if (!state.materialsReady) {
    if (materialsAlreadyRefreshed) {
      state = { ...state, materialsReady: true, materialsSource: 'parent-refresh' };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } else {
      if (!state.materialsJobId) {
        onProgress({ event: 'materials.request' });
        try {
          const launched = await client.importMaterials(); state = { ...state, phase: 'materials-running', materialsJobId: launched.jobId };
          if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
        } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
      }
      const imported = await waitForJob(client, state.materialsJobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
      if (!(imported.snapshot.materials || []).some(row => row.imported === true && row.kind === 'knowledge'))
        throw new CliError('Imported account policy materials are unavailable; generation was not started', { code: 'MATERIALS_UNAVAILABLE' });
      state = { ...state, phase: 'starting', materialsReady: true, materialsSource: 'import-job' };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    }
  }
  if (!state.prepareJobId && state.prepareTransport !== 'legacy-conversation') {
    onProgress({ event: 'prepare.request', transport: 'engine', itemIds: state.itemIds });
    try {
      const launched = await client.prepareEngine({ itemIds: state.itemIds, instruction: state.instruction });
      state = { ...state, phase: 'assistant-running', prepareJobId: launched.jobId, prepareTransport: 'engine' };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } catch (error) {
      if (error.status !== 404) { await persistFailure(checkpointPath, state, error); throw error; }
      if (state.itemIds.length > 20) throw new CliError('This older engine supports at most 20 items through the legacy preparation path', { code: 'LEGACY_PREPARE_LIMIT' });
      state = { ...state, prepareTransport: 'legacy-conversation' };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    }
  }
  if (!state.prepareJobId && !state.conversationId) {
    onProgress({ event: 'conversation.request', itemIds: state.itemIds });
    try {
      const conversation = await client.createConversation({ title: 'Headless preparation', itemIds: [] });
      state = { ...state, phase: 'conversation-created', conversationId: conversation.id };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
  }
  if (!state.prepareJobId) {
    onProgress({ event: 'prepare.request', transport: 'legacy-conversation', conversationId: state.conversationId, itemIds: state.itemIds });
    try {
      const launched = await client.sendConversationMessage(state.conversationId, {
        text: state.instruction, itemIds: state.itemIds,
        screen: { kind: state.itemIds.length === 1 ? 'comment' : 'queue', selectedItemId: state.itemIds.length === 1 ? state.itemIds[0] : undefined }
      });
      state = { ...state, phase: 'assistant-running', prepareJobId: launched.jobId };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
  }
  if (state.phase === 'assistant-running' || state.phase === 'stopped') {
    const settled = await waitForJob(client, state.prepareJobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    const current = settled.snapshot; const allowed = new Set(state.itemIds);
    const proposals = (current.proposals || []).filter(row => row.prepareRunId === state.prepareJobId && allowed.has(row.itemId) && row.status === 'draft')
      .map(({ id, revision, itemId, kind, text }) => ({ id, revision, itemId, kind, text }));
    state = { ...state, phase: 'prepared', prepareOutcome: settled.job.prepareOutcome || null, proposals };
    if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    onProgress({ event: 'prepare.completed', jobId: state.prepareJobId, proposals: proposals.map(row => ({ id: row.id, revision: row.revision, itemId: row.itemId })) });
  }
  return state;
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
  if (state.phase === 'unknown') throw new CliError('Scan dispatch outcome is unknown; inspect the engine job ledger before retrying', { code: 'UNKNOWN_MUTATION_OUTCOME' });
  if (state.phase === 'complete') return { mode: 'complete', checkpoint: state, result: state.result };
  if (state.phase === 'incomplete') {
    if (!state.nextResume) throw new CliError('Scan coverage is incomplete but no nextResume token was returned', { code: 'INCOMPLETE_WITHOUT_RESUME' });
    state = { kind: state.kind, account: state.account, baseUrl: state.baseUrl, phase: 'starting', generation: Number(state.generation || 0) + 1, request: { ...request, resume: state.nextResume }, previousJobId: state.jobId };
  }
  if (!state.jobId) {
    onProgress({ event: 'scan.request', generation: state.generation, bounds: { pageSize: state.request.pageSize, maxPages: state.request.maxPages, maxItems: state.request.maxItems, maxElapsedMs: state.request.maxElapsedMs } });
    try {
      const launched = await client.mutate('/api/engine/scan', state.request);
      state = { ...state, phase: 'running', jobId: launched.jobId };
      if (checkpointPath) state = await writeCheckpoint(checkpointPath, state);
    } catch (error) { await persistFailure(checkpointPath, state, error); throw error; }
  }
  const settled = await waitForJob(client, state.jobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
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
  const { checkpointPath, resumePath, instruction, materialsAlreadyRefreshed = false, autonomous = false, execute = false, reconcileUnknown = false, pollMs, maxPolls, signal, onProgress = () => {} } = options;
  let state = resumePath ? await readCheckpoint(resumePath) : null;
  if (state) validateResume(client, state);
  const path = checkpointPath || resumePath;
  if (state && (['complete', 'no-action'].includes(state.phase) || state.phase === 'needs-reconciliation' && !reconcileUnknown)) return { mode: state.phase, checkpoint: state };
  state = await generateProposals(client, state?.itemIds || itemIds, { instruction, checkpointPath: path, checkpoint: state, materialsAlreadyRefreshed, pollMs, maxPolls, signal, onProgress });
  if (!autonomous) return { mode: 'prepare-only', checkpoint: state };
  if (!state.proposals.length) {
    state = { ...state, phase: 'no-action' }; if (path) state = await writeCheckpoint(path, state);
    return { mode: state.phase, checkpoint: state };
  }
  if (state.proposals.length > 100) throw new CliError('Autonomous approval is limited to 100 exact proposals', { code: 'AUTONOMOUS_BATCH_LIMIT' });
  if (!state.approvalId) {
    const snapshot = await client.bootstrap();
    const exact = state.proposals.map(ref => {
      const current = (snapshot.proposals || []).find(row => row.id === ref.id);
      if (!current || current.status !== 'draft' || current.revision !== ref.revision || current.itemId !== ref.itemId || current.prepareRunId !== state.prepareJobId)
        throw new CliError(`Proposal ${ref.id} changed; inspect and start a new approval`, { code: 'STALE_OR_CONFLICT' });
      return { id: current.id, revision: current.revision };
    });
    onProgress({ event: 'approval.request', proposals: exact });
    try {
      const approval = await client.createApproval(exact);
      state = { ...state, phase: 'approved', approvalId: approval.id, approvedProposals: exact };
      if (path) state = await writeCheckpoint(path, state);
    } catch (error) { await persistFailure(path, state, error); throw error; }
  }
  if (!execute) return { mode: 'approved', checkpoint: state };
  if (!state.executeJobId) {
    onProgress({ event: 'execute.request', approvalId: state.approvalId });
    try {
      const launched = await client.execute(state.approvalId);
      state = { ...state, phase: 'executing', executeJobId: launched.jobId };
      if (path) state = await writeCheckpoint(path, state);
    } catch (error) { await persistFailure(path, state, error); throw error; }
  }
  const settled = await waitForJob(client, state.executeJobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
  const ids = new Set(state.proposals.map(row => row.id));
  const operations = (settled.snapshot.operations || []).filter(row => ids.has(row.proposalId));
  state = { ...state, phase: operations.some(row => row.status === 'unknown') ? 'needs-reconciliation' : 'complete', operations: operations.map(row => ({ id: row.id, proposalId: row.proposalId, status: row.status })) };
  if (path) state = await writeCheckpoint(path, state);
  if (state.phase === 'needs-reconciliation' && reconcileUnknown) {
    const jobs = { ...(state.reconciliationJobs || {}) };
    for (const operation of state.operations.filter(row => row.status === 'unknown')) {
      let jobId = jobs[operation.id];
      if (!jobId) {
        onProgress({ event: 'reconcile.request', operationId: operation.id });
        try {
          const launched = await client.reconcile(operation.id); jobId = launched.jobId; jobs[operation.id] = jobId;
          state = { ...state, reconciliationJobs: jobs }; if (path) state = await writeCheckpoint(path, state);
        } catch (error) { await persistFailure(path, state, error); throw error; }
      }
      await waitForJob(client, jobId, { pollMs, maxPolls, signal, onPoll: job => onProgress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    }
    const refreshed = await client.bootstrap();
    const finalOperations = (refreshed.operations || []).filter(row => ids.has(row.proposalId));
    state = { ...state, phase: finalOperations.some(row => row.status === 'unknown') ? 'needs-reconciliation' : 'complete', operations: finalOperations.map(row => ({ id: row.id, proposalId: row.proposalId, status: row.status })) };
    if (path) state = await writeCheckpoint(path, state);
  }
  return { mode: state.phase, checkpoint: state };
}

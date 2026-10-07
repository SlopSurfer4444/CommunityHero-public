import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ConductorClient, rpcRequest } from '../cli/conductor-client.mjs';
import { runConductor, connectionCheckpointTree } from '../cli/conductor.mjs';
import { connectionFailure, hasUnconfirmedAdmission, validateConnectionDependency } from '../cli/conductor-connection.mjs';
import { localAdmissionPayloadHash, UnknownMutationError } from '../cli/client.mjs';
import { runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { settleEditorialReview, settleEditorialRepair } from '../cli/editorial.mjs';
import { readyBatchStopped } from '../cli/ready-batches.mjs';
import { runBulk } from '../cli/bulk.mjs';
import { createMediaDependencies } from '../cli/conductor-media.mjs';
import { createFactDependencies } from '../cli/conductor-facts.mjs';

const read = async path => JSON.parse(await readFile(path, 'utf8'));
async function fixture() {
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-connection-pure-'));
  const config = { version: 1, runId: 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa', leaseGeneration: 2,
    account: 'LikeAvto', capability: 'a'.repeat(64), mode: 'execute', scopeItemIds: ['item'],
    checkpointPath: join(dir, 'queue.json'), batchSize: 1, maxRepairRounds: 0, maxCycles: 2,
    resume: true, baseUrl: 'http://127.0.0.1:4185', rpcUrl: 'http://127.0.0.1:12345/rpc',
    connectionBinding: { id: 'native-fixture', workspaceId: 'local-pilot', accountId: 'LikeAvto', connector: 'vk', revision: 1, providerAccountId: 'synthetic-provider' } };
  const dependency = { version: 1, kind: 'conductor-connection-dependency', runId: config.runId,
    leaseGeneration: config.leaseGeneration, connectionBinding: config.connectionBinding,
    gateObservation: { kind: 'present', gateEpoch: 3, owner: { account: config.account, runtimeId: 'runtime', releaseSha256: 'b'.repeat(64), epoch: 1 },
      connectionBinding: config.connectionBinding, state: 'blocked', availabilityState: 'unverified' }, executeRejection: null };
  return { config, dependency };
}
function transport(config, handler) {
  const calls = [];
  const client = new ConductorClient(config, { fetchImpl: async (url, init) => {
    assert.equal(url, config.rpcUrl); const request = JSON.parse(init.body); calls.push(request);
    const body = await handler(request);
    return new Response(JSON.stringify(Number.isInteger(body?.status) ? body : { status: 200, body }));
  } });
  return { client, calls };
}
const seed = config => ({ kind: 'communityhero-queue', account: config.account, baseUrl: config.baseUrl,
  phase: 'starting', workflowId: config.runId, cycle: 0, attemptedItemIds: [], slices: [], scopeItemIds: config.scopeItemIds,
  flowPolicy: { freshEditorial: true, maxRepairRounds: 0, continueHeld: true } });

for (const [name, mutate] of [
  ['wrong run', value => { value.runId = 'bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb'; }],
  ['wrong lease', value => { value.leaseGeneration++; }],
  ['foreign connection', value => { value.connectionBinding.revision++; }],
  ['foreign owner', value => { value.gateObservation.owner.account = 'BAW Russia'; }],
  ['unsafe epoch', value => { value.gateObservation.gateEpoch = 9007199254740992; }],
  ['unknown state', value => { value.gateObservation.state = 'ready'; }],
  ['extra field', value => { value.ready = true; }],
  ['fake missing owner', value => { value.gateObservation = { kind: 'missing', owner: null }; }]
]) test(`private connection DTO rejects ${name}`, async () => {
  const { config, dependency } = await fixture(); const changed = structuredClone(dependency); mutate(changed);
  assert.throws(() => validateConnectionDependency(changed, config), { code: 'INVALID_CONDUCTOR_CONFIG' });
});

test('native typed dependency may explain missing gate or open-ready with a remaining hold', async () => {
  const { config, dependency } = await fixture();
  assert.equal(validateConnectionDependency({ ...dependency, gateObservation: { kind: 'missing' } }, config).gateObservation.kind, 'missing');
  dependency.gateObservation.state = 'open'; dependency.gateObservation.availabilityState = 'ready';
  assert.equal(validateConnectionDependency(dependency, config).gateObservation.state, 'open');
  for (const value of [{ account: config.account }, { account: config.account, connectionDependency: true }]) {
    const { client, calls } = transport(config, () => value);
    await assert.rejects(client.sync({}), { code: 'INVALID_CONDUCTOR_CONFIG' }); assert.equal(calls.length, 1);
  }
});

test('closed private connection emits one disposition only after the same atomic checkpoint save', async () => {
  const { config, dependency } = await fixture(); await writeCheckpoint(config.checkpointPath, seed(config));
  const { client, calls } = transport(config, () => ({ account: config.account, connectionDependency: dependency }));
  for (let repeat = 0; repeat < 2; repeat++) {
    const frames = [];
    const result = await runConductor(config, { client, pollMs: 0, maxPolls: 1, onOutput: async frame => {
      const saved = await read(config.checkpointPath); assert.deepEqual(saved.connectionDependency, dependency); frames.push(frame);
    } });
    assert.equal(result.mode, 'waiting_dependency'); assert.deepEqual(frames, [{ type: 'dependency_wait', dependency }]);
    const saved = await read(config.checkpointPath);
    assert.equal(saved.workflowId, config.runId); assert.equal(saved.phase, 'starting');
    assert.deepEqual(saved.scopeItemIds, ['item']); assert.deepEqual(saved.attemptedItemIds, []); assert.deepEqual(saved.slices, []);
  }
  assert.ok(calls.every(row => row.operation === 'engineStatus'));
});

async function approved(config) {
  const path = `${config.checkpointPath}.original.json`;
  await writeCheckpoint(path, { account: config.account, baseUrl: config.baseUrl, workflowId: config.runId,
    phase: 'approved', materialsReady: true, prepareJobId: 'prepare-original', itemIds: ['item'],
    proposals: [{ id: 'proposal-original', revision: 1, itemId: 'item', kind: 'reply_and_close' }],
    approvedProposals: [{ id: 'proposal-original', revision: 1 }], approvalId: 'approval-original', executeRequestId: 'request-original' });
  return path;
}
const workflowOptions = path => ({ resumePath: path, autonomous: true, execute: true, streamingChild: true, pollMs: 0, maxPolls: 1 });
function approvedReview(config, request) {
  assert.equal(request.operation, 'reviewItems');
  assert.deepEqual(request.args, { itemIds: ['item'] });
  return { account: config.account, selectedItemIds: ['item'], items: [{ id: 'item' }],
    proposals: [{ id: 'proposal-original', revision: 1, itemId: 'item', kind: 'reply_and_close',
      prepareRunId: 'prepare-original', status: 'approved' }], operations: [],
    coverage: { itemsReturned: 1, operationsReturned: 0, operationsComplete: true, historyTruncated: false } };
}

test('closure between intent save and POST retains the original key and later admits its first execute once', async () => {
  const { config, dependency } = await fixture(); const path = await approved(config); let statusReads = 0; let closed = true;
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed && ++statusReads > 1 ? dependency : null };
    if (request.operation === 'execute') return { jobId: 'execute-original', approvalId: 'approval-original', requestId: 'request-original', replayed: false };
    if (request.operation === 'job') return { id: 'execute-original', kind: 'execute', refId: 'approval-original', status: 'completed' };
    if (request.operation === 'reviewItems') return { account: config.account, selectedItemIds: ['item'], items: [{ id: 'item' }],
      proposals: [], operations: [{ id: 'operation-original', proposalId: 'proposal-original', itemId: 'item', approvalId: 'approval-original', status: 'succeeded' }],
      coverage: { itemsReturned: 1, operationsReturned: 1, operationsComplete: true, historyTruncated: false } };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  for (let turn = 0; turn < 2; turn++) {
    await assert.rejects(runWorkflow(client, [], workflowOptions(path)), error => Boolean(connectionFailure(error)));
    const saved = await read(path); assert.equal(saved.phase, 'approved'); assert.equal(saved.executeRequestId, 'request-original');
    assert.equal(saved.pendingLocalAdmission, null); assert.equal(saved.connectionUnpostedIntent.requestId, 'request-original');
    assert.equal(saved.connectionUnpostedIntent.payloadHash, localAdmissionPayloadHash({ approvalId: 'approval-original' }));
    assert.equal(calls.filter(row => row.operation === 'execute').length, 0);
  }
  closed = false;
  const result = await runWorkflow(client, [], workflowOptions(path)); assert.equal(result.mode, 'complete');
  assert.deepEqual(calls.filter(row => row.operation === 'execute'), [{ operation: 'execute', args: { approvalId: 'approval-original', requestId: 'request-original' } }]);
  assert.equal(result.checkpoint.executeJobId, 'execute-original'); assert.equal(result.checkpoint.connectionUnpostedIntent, null);
});

test('posted UNKNOWN cannot be converted to a before-POST connection wait by a saved marker', async () => {
  const { config } = await fixture(); const path = await approved(config);
  const state = await read(path); state.phase = 'unknown'; state.error = { code: 'UNKNOWN_MUTATION_OUTCOME' };
  state.pendingLocalAdmission = { kind: 'execute', requestId: state.executeRequestId, payloadHash: localAdmissionPayloadHash({ approvalId: state.approvalId }) };
  state.connectionUnpostedIntent = { ...state.pendingLocalAdmission, path: `/api/approvals/${state.approvalId}/execute` }; await writeCheckpoint(path, state);
  const { client, calls } = transport(config, () => assert.fail('Forged marker must fail before transport'));
  await assert.rejects(runWorkflow(client, [], workflowOptions(path)), { code: 'UNKNOWN_MUTATION_OUTCOME' }); assert.equal(calls.length, 0);
  assert.equal((await read(path)).phase, 'unknown');
  assert.equal(connectionFailure(new UnknownMutationError('POST', '/api/approvals/a/execute', {})), null);
  assert.equal(connectionFailure({ code: 'CONDUCTOR_CONNECTION_DEPENDENCY' }), null);
});

test('pending-negative read transport is exact and never extends private execute mutation fields', async () => {
  assert.deepEqual(rpcRequest(new URL('http://127.0.0.1:4185/api/conductor/connection-dependency?approvalId=original&requestId=key'), 'GET'),
    { operation: 'connectionDependency', args: { approvalId: 'original', requestId: 'key' } });
  for (const query of ['approvalId=a', 'approvalId=a&requestId=b&ready=true', 'approvalId=a&requestId=b&requestId=c'])
    assert.throws(() => rpcRequest(new URL(`http://127.0.0.1:4185/api/conductor/connection-dependency?${query}`), 'GET'));
  const { config } = await fixture(); const { client, calls } = transport(config, () => assert.fail('No private re-evaluation RPC'));
  await assert.rejects(client.execute('original', 'key', { reevaluate: { evaluationId: 'parent', receiptSha256: 'a'.repeat(64) } }),
    { code: 'CONDUCTOR_OPERATION_DENIED' });
  assert.equal(calls.length, 0);
});

for (const corruption of [null, 'request', 'evaluation', 'receipt', 'binding'])
  test(`exact negative survives reopening without an automatic execute${corruption ? `; rejects ${corruption} DTO drift` : ''}`, async () => {
    const { config, dependency } = await fixture(); const path = await approved(config);
    const payloadHash = localAdmissionPayloadHash({ approvalId: 'approval-original' });
    const negative = { kind: 'execute', status: 'rejected_local', account: config.account, approvalId: 'approval-original', requestId: 'request-original',
      payloadHash, result: null, viewStatus: 'waiting_dependency', evaluationId: 'evaluation-original', receiptSha256: 'd'.repeat(64), requestHash: 'e'.repeat(64),
      gateEpoch: 3, connectionBinding: config.connectionBinding, reevaluationAvailable: true, retryAuthorized: false,
      noAttemptProof: { executeJobCreated: false, operationCreated: false, approvalConsumed: false, providerDispatchArmed: false } };
    const observed = { ...structuredClone(dependency), executeRejection: { kind: 'execute-rejection', requestId: negative.requestId,
      approvalId: negative.approvalId, payloadHash, evaluationId: negative.evaluationId, receiptSha256: negative.receiptSha256 } };
    observed.gateObservation.state = 'open'; observed.gateObservation.availabilityState = 'ready';
    if (corruption === 'request') observed.executeRejection.requestId = 'foreign-request';
    if (corruption === 'evaluation') observed.executeRejection.evaluationId = 'stale-evaluation';
    if (corruption === 'receipt') observed.executeRejection.receiptSha256 = 'f'.repeat(64);
    if (corruption === 'binding') observed.connectionBinding.revision++;
    const { client, calls } = transport(config, request => {
      if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: null };
      if (request.operation === 'reviewItems') return approvedReview(config, request);
      if (request.operation === 'execute') return { status: 409, body: { error: 'Closed during validation' } };
      if (request.operation === 'localAdmission') return negative;
      if (request.operation === 'connectionDependency') {
        assert.deepEqual(request.args, { approvalId: 'approval-original', requestId: 'request-original' }); return observed;
      }
      assert.fail(`Unexpected RPC ${request.operation}`);
    });
    if (corruption) await assert.rejects(runWorkflow(client, [], workflowOptions(path)), { code: 'INVALID_CONDUCTOR_CONFIG' });
    else {
      await assert.rejects(runWorkflow(client, [], workflowOptions(path)), error => Boolean(connectionFailure(error)));
      const saved = await read(path); assert.equal(saved.phase, 'stopped');
      assert.equal(saved.error.code, 'REJECTED_LOCAL_ADMISSION'); assert.equal(saved.error.rejection.evaluationId, negative.evaluationId);
      assert.equal(saved.pendingLocalAdmission.requestId, negative.requestId); assert.equal(saved.pendingLocalAdmission.payloadHash, payloadHash);
      assert.deepEqual(saved.connectionDependency, observed);
      await assert.rejects(runWorkflow(client, [], workflowOptions(path)), error => Boolean(connectionFailure(error)));
      assert.equal((await read(path)).error.rejection.receiptSha256, negative.receiptSha256);
    }
    assert.equal(calls.filter(row => row.operation === 'execute').length, 1);
    assert.equal(calls.filter(row => row.operation === 'reviewItems').length, 1);
    assert.ok(calls.filter(row => row.operation === 'execute').every(row => Object.keys(row.args).sort().join(',') === 'approvalId,requestId'));
    assert.equal(calls.filter(row => ['prepare', 'sync', 'editorialReview', 'approval'].includes(row.operation)).length, 0);
  });

test('unposted editorial and repair retain exact request/revision scope without a model call while closed', async () => {
  const { config, dependency } = await fixture(); let closed = true; const refs = [{ id: 'proposal', revision: 1 }];
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed ? dependency : null };
    if (request.operation === 'editorialReview') return { jobId: 'editorial-job', requestId: request.args.requestId, replayed: false };
    if (request.operation === 'job') return { id: 'editorial-job', kind: 'editorial_review', refId: editorial.requestId, status: 'completed', editorialOutcome: { accepted: refs, reused: [], held: [] } };
    if (request.operation === 'editorialRepair') return { status: 'repaired', repairId: 'repair', parentReviewJobId: 'editorial-job', requestId: request.args.requestId,
      replayed: false, oldRefs: refs, newRefs: [{ id: 'proposal', revision: 2 }] };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  let editorial; const editorialOptions = { save: async value => { editorial = value; }, pollMs: 0, maxPolls: 1, fresh: true };
  let wait;
  await assert.rejects(settleEditorialReview(client, refs, null, editorialOptions), error => { wait = error; return Boolean(connectionFailure(error)); });
  assert.equal(editorial.phase, 'unposted'); const key = editorial.requestId;
  assert.equal(readyBatchStopped(wait, 'original-parent'), wait);
  assert.equal(calls.filter(row => row.operation !== 'engineStatus').length, 0);
  closed = false; await settleEditorialReview(client, refs, editorial, editorialOptions); assert.equal(editorial.requestId, key);
  assert.equal(calls.filter(row => row.operation === 'editorialReview').length, 1);
  const expected = [{ proposalId: 'proposal', proposalRevision: 1, textSha256: 'a'.repeat(64), contextDigest: 'a'.repeat(64), rulesDigest: 'a'.repeat(64), receiptSha256: 'a'.repeat(64) }];
  let repair; const repairOptions = { save: async value => { repair = value; } }; closed = true;
  await assert.rejects(settleEditorialRepair(client, 'editorial-job', expected, null, repairOptions), error => Boolean(connectionFailure(error)));
  assert.equal(repair.phase, 'unposted'); const repairKey = repair.requestId; closed = false;
  await settleEditorialRepair(client, 'editorial-job', expected, repair, repairOptions);
  assert.equal(repair.requestId, repairKey); assert.equal(repair.phase, 'repaired');
  assert.equal(calls.filter(row => row.operation === 'editorialRepair').length, 1);
});

for (const stage of ['approval', 'execute']) test(`bulk ${stage} preserves its first unposted key through repeated connection waits`, async () => {
  const { config, dependency } = await fixture(); const path = `${config.checkpointPath}.bulk.json`;
  const references = [{ id: 'proposal-original', revision: 1, itemId: 'item' }];
  const proposals = [{ id: 'proposal-original', revision: 1 }];
  const slice = { index: 0, references, phase: stage === 'approval' ? 'editorial-reviewed' : 'approved',
    editorialReview: { phase: 'complete', requestId: 'editorial-original', proposals,
      payloadHash: localAdmissionPayloadHash({ proposals }), outcome: { accepted: proposals, reused: [], held: [] } },
    ...(stage === 'execute' ? { approvalRequestId: 'approval-key', payloadHash: localAdmissionPayloadHash({ proposals }),
      admission: { id: 'approval-original', requestId: 'approval-key', accepted: proposals, held: [] } } : {}) };
  await writeCheckpoint(path, { kind: 'communityhero-reviewed-bulk', account: config.account, baseUrl: config.baseUrl,
    phase: 'ready', references, scopeHash: localAdmissionPayloadHash({ references }),
    partialAdmission: false, continuationPolicy: 'stop-on-mixed', slices: [slice] });
  let closed = true;
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed ? dependency : null };
    if (request.operation === 'approval') return { id: 'approval-original', requestId: request.args.requestId };
    if (request.operation === 'execute') return { jobId: 'execute-original', approvalId: 'approval-original', requestId: request.args.requestId, replayed: false };
    if (request.operation === 'job') return { id: 'execute-original', kind: 'execute', refId: 'approval-original', status: 'completed' };
    if (request.operation === 'reviewItems') return { account: config.account, selectedItemIds: ['item'], items: [{ id: 'item' }], proposals: [],
      operations: [{ id: 'operation-original', proposalId: 'proposal-original', itemId: 'item', approvalId: 'approval-original', status: 'succeeded' }],
      coverage: { itemsReturned: 1, operationsReturned: 1, operationsComplete: true, historyTruncated: false } };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  let retained;
  for (let turn = 0; turn < 2; turn++) {
    await assert.rejects(runBulk(client, [], { resumePath: path, execute: true, pollMs: 0, maxPolls: 1 }), error => Boolean(connectionFailure(error)));
    const saved = await read(path); const intent = saved.slices[0].connectionUnpostedIntent;
    retained ||= intent; assert.deepEqual(intent, retained);
    assert.deepEqual(saved.connectionDependency, dependency); assert.deepEqual(saved.references, references);
    assert.equal(calls.filter(row => ['approval', 'execute', 'editorialReview'].includes(row.operation)).length, 0);
  }
  closed = false;
  const result = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0, maxPolls: 1 });
  assert.equal(result.mode, 'complete'); assert.equal(calls.filter(row => row.operation === stage).length, 1);
  assert.equal(calls.find(row => row.operation === stage).args.requestId, retained.requestId);
  assert.equal(result.checkpoint.slices[0].connectionUnpostedIntent, null);
});

test('actual checkpoint tree reads current, pending and ready child journals instead of stale embedded snapshots', async () => {
  const { config } = await fixture(); const childPath = join(`${config.checkpointPath}.slices`, 'original.json');
  const pendingPath = join(`${config.checkpointPath}.slices`, 'pending.json'); const batchId = 'a'.repeat(64);
  const readyPath = join(`${childPath}.ready`, `${batchId}.json`);
  await writeCheckpoint(readyPath, { phase: 'unknown', error: { code: 'UNKNOWN_MUTATION_OUTCOME' }, executeRequestId: 'original' });
  await writeCheckpoint(childPath, { phase: 'assistant-running', readyBatches: [{ id: batchId, mode: 'started', child: { phase: 'complete' } }] });
  await writeCheckpoint(pendingPath, { phase: 'approved', executeRequestId: 'pending-original' });
  await writeCheckpoint(config.checkpointPath, { ...seed(config), currentSlice: { childPath, child: { phase: 'complete' } },
    pendingSlices: [{ childPath: pendingPath }], slices: [] });
  const nodes = await connectionCheckpointTree(config.checkpointPath);
  assert.equal(nodes.length, 4); assert.equal(nodes.some(hasUnconfirmedAdmission), true);
  assert.equal(nodes.find(row => row.executeRequestId === 'original').phase, 'unknown');
  assert.equal(nodes.find(row => row.executeRequestId === 'pending-original').phase, 'approved');
  await writeCheckpoint(readyPath, { phase: 'complete', executeRequestId: 'original' });
  const actual = await connectionCheckpointTree(config.checkpointPath);
  assert.equal(actual.some(hasUnconfirmedAdmission), false);
});

for (const corruption of ['foreign path', 'missing started child', 'missing current path']) test(`checkpoint tree rejects ${corruption}`, async () => {
  const { config } = await fixture(); const childPath = corruption === 'foreign path' ? join(tmpdir(), 'foreign-original.json')
    : join(`${config.checkpointPath}.slices`, 'missing.json');
  await writeCheckpoint(config.checkpointPath, { ...seed(config), currentSlice: corruption === 'missing current path' ? {} : { childPath } });
  await assert.rejects(connectionCheckpointTree(config.checkpointPath), { code: 'INVALID_CHECKPOINT' });
});

test('committed receipt after the explicit operator action recovers the original execute job by reads only', async () => {
  const { config } = await fixture(); const path = await approved(config); const saved = await read(path);
  saved.phase = 'stopped'; saved.pendingLocalAdmission = { kind: 'execute', requestId: saved.executeRequestId,
    payloadHash: localAdmissionPayloadHash({ approvalId: saved.approvalId }) };
  saved.error = { code: 'REJECTED_LOCAL_ADMISSION' }; await writeCheckpoint(path, saved);
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: null };
    if (request.operation === 'localAdmission') return { kind: 'execute', status: 'committed', requestId: saved.executeRequestId,
      payloadHash: saved.pendingLocalAdmission.payloadHash, result: { jobId: 'execute-original', approvalId: saved.approvalId, requestId: saved.executeRequestId, replayed: false } };
    if (request.operation === 'job') return { id: 'execute-original', kind: 'execute', refId: saved.approvalId, status: 'completed' };
    if (request.operation === 'reviewItems') return { account: config.account, selectedItemIds: ['item'], items: [{ id: 'item' }], proposals: [],
      operations: [{ id: 'operation-original', proposalId: 'proposal-original', itemId: 'item', approvalId: saved.approvalId, status: 'succeeded' }],
      coverage: { itemsReturned: 1, operationsReturned: 1, operationsComplete: true, historyTruncated: false } };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  const result = await runWorkflow(client, [], workflowOptions(path)); assert.equal(result.mode, 'complete');
  assert.equal(result.checkpoint.executeRequestId, saved.executeRequestId); assert.equal(result.checkpoint.executeJobId, 'execute-original');
  assert.equal(calls.filter(row => ['execute', 'prepare', 'approval', 'editorialReview', 'sync'].includes(row.operation)).length, 0);
});

test('lost execute ACK followed by closure remains UNKNOWN and reads the same key without another POST', async () => {
  const { config, dependency } = await fixture(); const path = await approved(config); let closed = false;
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed ? dependency : null };
    if (request.operation === 'reviewItems') return approvedReview(config, request);
    if (request.operation === 'execute') throw new TypeError('Synthetic lost reply from the injected in-memory callback');
    if (request.operation === 'localAdmission') return { status: 404, body: { error: 'Synthetic absent receipt' } };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  await assert.rejects(runWorkflow(client, [], workflowOptions(path)), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const before = await read(path); assert.equal(before.phase, 'unknown'); assert.equal(before.pendingLocalAdmission.requestId, 'request-original');
  closed = true;
  await assert.rejects(runWorkflow(client, [], workflowOptions(path)), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const after = await read(path); assert.equal(after.phase, 'unknown');
  assert.deepEqual(after.pendingLocalAdmission, before.pendingLocalAdmission); assert.equal(after.executeRequestId, before.executeRequestId);
  assert.equal(after.connectionUnpostedIntent, undefined); assert.equal(calls.filter(row => row.operation === 'execute').length, 1);
  assert.equal(calls.filter(row => row.operation === 'reviewItems').length, 1);
  assert.equal(calls.filter(row => row.operation === 'connectionDependency').length, 0);
});

test('bulk saved UNKNOWN refuses a forged unposted marker before any RPC', async () => {
  const { config } = await fixture(); const path = `${config.checkpointPath}.bulk-unknown.json`;
  const references = [{ id: 'proposal-original', revision: 1, itemId: 'item' }]; const proposals = [{ id: 'proposal-original', revision: 1 }];
  await writeCheckpoint(path, { kind: 'communityhero-reviewed-bulk', account: config.account, baseUrl: config.baseUrl,
    phase: 'stopped', references, scopeHash: localAdmissionPayloadHash({ references }), partialAdmission: false, continuationPolicy: 'stop-on-mixed',
    slices: [{ index: 0, references, phase: 'unknown', error: { code: 'UNKNOWN_MUTATION_OUTCOME' }, approvalRequestId: 'approval-key',
      payloadHash: localAdmissionPayloadHash({ proposals }), admission: { id: 'approval-original', requestId: 'approval-key', accepted: proposals, held: [] },
      executeAttemptId: 'request-original', executeAdmissionProtocol: 'local-admission-v1', executePayloadHash: localAdmissionPayloadHash({ approvalId: 'approval-original' }),
      connectionUnpostedIntent: { kind: 'execute', requestId: 'request-original', payloadHash: localAdmissionPayloadHash({ approvalId: 'approval-original' }) } }] });
  const { client, calls } = transport(config, () => assert.fail('UNKNOWN cannot become an unposted original'));
  await assert.rejects(runBulk(client, [], { resumePath: path, execute: true, pollMs: 0, maxPolls: 1 }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal(calls.length, 0); assert.equal((await read(path)).slices[0].phase, 'unknown');
});

for (const validProof of [true, false]) test(`lost-ACK UNKNOWN resolves only from the exact native negative4false receipt: ${validProof}`, async () => {
  const { config, dependency } = await fixture(); const path = await approved(config); const saved = await read(path);
  const payloadHash = localAdmissionPayloadHash({ approvalId: saved.approvalId });
  saved.phase = 'unknown'; saved.error = { code: 'UNKNOWN_MUTATION_OUTCOME' };
  saved.pendingLocalAdmission = { kind: 'execute', requestId: saved.executeRequestId, payloadHash }; await writeCheckpoint(path, saved);
  const negative = { kind: 'execute', status: 'rejected_local', account: config.account, approvalId: saved.approvalId, requestId: saved.executeRequestId,
    payloadHash, result: null, viewStatus: 'waiting_dependency', evaluationId: 'evaluation-original', receiptSha256: 'd'.repeat(64), requestHash: 'e'.repeat(64),
    gateEpoch: 3, connectionBinding: config.connectionBinding, reevaluationAvailable: true, retryAuthorized: false,
    noAttemptProof: { executeJobCreated: false, operationCreated: false, approvalConsumed: false, providerDispatchArmed: !validProof } };
  const observed = { ...dependency, executeRejection: { kind: 'execute-rejection', requestId: negative.requestId, approvalId: negative.approvalId,
    payloadHash, evaluationId: negative.evaluationId, receiptSha256: negative.receiptSha256 } };
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: null };
    if (request.operation === 'localAdmission') return negative;
    if (request.operation === 'connectionDependency') return observed;
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  await assert.rejects(runWorkflow(client, [], workflowOptions(path)), error => validProof ? Boolean(connectionFailure(error)) : error.code === 'UNKNOWN_MUTATION_OUTCOME');
  const after = await read(path); assert.deepEqual(after.pendingLocalAdmission, saved.pendingLocalAdmission);
  assert.equal(after.executeRequestId, saved.executeRequestId); assert.equal(after.approvalId, saved.approvalId);
  assert.equal(after.phase, validProof ? 'stopped' : 'unknown');
  assert.equal(after.error.code, validProof ? 'REJECTED_LOCAL_ADMISSION' : 'UNKNOWN_MUTATION_OUTCOME');
  assert.equal(calls.filter(row => row.operation === 'execute').length, 0);
  assert.equal(calls.filter(row => row.operation === 'connectionDependency').length, validProof ? 1 : 0);
});

test('closed media before-POST restores the original cursor and preserves the first admission for reopening', async () => {
  const { config, dependency } = await fixture(); let closed = true;
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed ? dependency : null };
    if (request.operation === 'media') return { jobId: 'media-original', status: 'running' };
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  client.planPrepare = async () => ({ batches: [], held: [{ itemId: 'item', reason: 'media_pending' }] });
  client.reviewItems = async () => ({ items: [{ id: 'item', postId: 'post-original' }] });
  client.mediaStatus = async () => ({ jobs: [{ id: 'media-original', status: 'running' }] });
  const observer = await createMediaDependencies(client, config, { pollMs: 0, maxPolls: 1 });
  for (let turn = 0; turn < 2; turn++) {
    await assert.rejects(observer.observeOnce({ heldItemIds: ['item'] }), error => Boolean(connectionFailure(error)));
    const saved = await read(`${config.checkpointPath}.dependencies.json`);
    assert.equal(saved.dependencies[0].phase, 'new'); assert.deepEqual(saved.dependencies[0].itemIds, ['item']);
    assert.equal(calls.filter(row => row.operation === 'media').length, 0);
  }
  closed = false; assert.deepEqual(await observer.observeOnce({ heldItemIds: ['item'] }), { readyItemIds: [], pending: true });
  assert.equal(calls.filter(row => row.operation === 'media').length, 1);
  assert.equal((await read(`${config.checkpointPath}.dependencies.json`)).dependencies[0].jobId, 'media-original');
});

test('closed public-fact before-POST retains the exact original parent and cursor without research', async () => {
  const { config, dependency } = await fixture(); let closed = true;
  const row = { id: 'fact-original', itemId: 'item', kind: 'missing_public_fact', status: 'pending', lastResearchJobId: null, consumedByJobId: null };
  const parent = { id: 'prepare-original', purpose: 'engine_prepare', conductorRunId: config.runId, grantGeneration: 1, factDependencies: [row] };
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: closed ? dependency : null };
    if (request.operation === 'job') { assert.equal(request.args.jobId, parent.id); return parent; }
    if (request.operation === 'resolvePublicFacts') {
      assert.deepEqual(request.args, { prepareJobId: parent.id, itemIds: ['item'] }); row.lastResearchJobId = 'research-original'; row.status = 'researching';
      return { jobIds: ['research-original'], readyItemIds: [], held: [] };
    }
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  const observer = await createFactDependencies(client, config, { pollMs: 0, maxPolls: 1 });
  const context = { factDependencies: [{ ...row, prepareJobId: parent.id }] };
  for (let turn = 0; turn < 2; turn++) {
    await assert.rejects(observer.observeOnce(context), error => Boolean(connectionFailure(error)));
    const saved = await read(`${config.checkpointPath}.facts.json`);
    assert.equal(saved.dependencies[0].phase, 'new'); assert.equal(saved.dependencies[0].id, row.id); assert.equal(saved.dependencies[0].prepareJobId, parent.id);
    assert.equal(calls.filter(request => request.operation === 'resolvePublicFacts').length, 0);
  }
  closed = false; assert.deepEqual(await observer.observeOnce(context), { readyItemIds: [], pending: true });
  assert.equal(calls.filter(request => request.operation === 'resolvePublicFacts').length, 1);
  assert.equal((await read(`${config.checkpointPath}.facts.json`)).dependencies[0].lastResearchJobId, 'research-original');
});

test('bulk reads its exact saved negative through reopen without another execute or new approval', async () => {
  const { config, dependency } = await fixture(); const path = `${config.checkpointPath}.bulk-negative.json`;
  const references = [{ id: 'proposal-original', revision: 1, itemId: 'item' }]; const proposals = [{ id: 'proposal-original', revision: 1 }];
  const payloadHash = localAdmissionPayloadHash({ approvalId: 'approval-original' });
  const negative = { kind: 'execute', status: 'rejected_local', account: config.account, approvalId: 'approval-original', requestId: 'request-original',
    payloadHash, result: null, viewStatus: 'waiting_dependency', evaluationId: 'evaluation-original', receiptSha256: 'd'.repeat(64), requestHash: 'e'.repeat(64),
    gateEpoch: 3, connectionBinding: config.connectionBinding, reevaluationAvailable: true, retryAuthorized: false,
    noAttemptProof: { executeJobCreated: false, operationCreated: false, approvalConsumed: false, providerDispatchArmed: false } };
  const observed = { ...structuredClone(dependency), executeRejection: { kind: 'execute-rejection', requestId: negative.requestId,
    approvalId: negative.approvalId, payloadHash, evaluationId: negative.evaluationId, receiptSha256: negative.receiptSha256 } };
  observed.gateObservation.state = 'open'; observed.gateObservation.availabilityState = 'ready';
  await writeCheckpoint(path, { kind: 'communityhero-reviewed-bulk', account: config.account, baseUrl: config.baseUrl,
    phase: 'running', references, scopeHash: localAdmissionPayloadHash({ references }), partialAdmission: false, continuationPolicy: 'stop-on-mixed',
    slices: [{ index: 0, references, phase: 'execute-admitting', approvalRequestId: 'approval-key', payloadHash: localAdmissionPayloadHash({ proposals }),
      admission: { id: negative.approvalId, requestId: 'approval-key', accepted: proposals, held: [] },
      executeAttemptId: negative.requestId, executeAdmissionProtocol: 'local-admission-v1', executePayloadHash: payloadHash }] });
  const { client, calls } = transport(config, request => {
    if (request.operation === 'engineStatus') return { account: config.account, connectionDependency: null };
    if (request.operation === 'localAdmission') return negative;
    if (request.operation === 'connectionDependency') return observed;
    assert.fail(`Unexpected RPC ${request.operation}`);
  });
  for (let turn = 0; turn < 2; turn++) {
    await assert.rejects(runBulk(client, [], { resumePath: path, execute: true, pollMs: 0, maxPolls: 1 }), error => Boolean(connectionFailure(error)));
    const saved = await read(path); const slice = saved.slices[0];
    assert.equal(slice.executeAttemptId, negative.requestId); assert.equal(slice.admission.id, negative.approvalId);
    assert.equal(slice.executePayloadHash, payloadHash); assert.equal(slice.error.rejection.evaluationId, negative.evaluationId);
    assert.equal(slice.error.rejection.receiptSha256, negative.receiptSha256); assert.equal(slice.executeJobId, undefined);
    assert.deepEqual(saved.connectionDependency, observed);
  }
  assert.ok(calls.every(row => ['engineStatus', 'localAdmission', 'connectionDependency'].includes(row.operation)));
});

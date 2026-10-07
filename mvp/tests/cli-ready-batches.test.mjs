import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm, unlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { generateProposals, runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { discoverReadyProposals, drainReadyBatches } from '../cli/ready-batches.mjs';
import { CliError, CommunityHeroClient, UnknownMutationError, localAdmissionPayloadHash } from '../cli/client.mjs';

const parent = { account: 'LikeAvto', baseUrl: 'http://localhost:4186', phase: 'assistant-running',
  itemIds: ['i1', 'i2'], instruction: 'Exact selected comments', proposals: [], materialsReady: true,
  prepareJobId: 'prepare', prepareRequestId: 'prepare-key', prepareTransport: 'engine', pendingLocalAdmission: null };
const proposal = n => ({ id: `p${n}`, revision: n, itemId: `i${n}`, kind: 'reply_and_close', text: `Synthetic ${n}`, status: 'draft', prepareRunId: 'prepare' });
const group = n => ({ key: `item:i${n}`, itemIds: [`i${n}`], status: 'admitted',
  admission: { candidates: [{ itemId: `i${n}`, status: 'review', proposalId: `p${n}` }] } });
const job = (status, groups = [group(1)]) => ({ id: 'prepare', kind: 'assistant', purpose: 'engine_prepare', status,
  preparationStages: { groupAdmission: groups }, error: 'PRIVATE_MODEL_ERROR' });
const saved = async path => JSON.parse(await readFile(path, 'utf8'));
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-ready-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'checkpoint.json'); await writeCheckpoint(path, parent); return path;
}
function api(prepareJobs) {
  const calls = []; const jobs = new Map(); const receipts = new Map(); const operations = [];
  let polls = 0;
  const client = { account: parent.account, baseUrl: parent.baseUrl,
    engineStatus: async () => ({ strictGrouping: { version: 1, contract: 'strict_post_family_v1' } }),
    requireStrictPreparation: CommunityHeroClient.prototype.requireStrictPreparation,
    reviewItems: async ids => ({ items: ids.map(id => ({ id })), proposals: [proposal(1), proposal(2)], operations,
      coverage: { operationsComplete: true } }),
    getJob: async id => {
      calls.push(['poll', id]);
      if (id === 'prepare') return prepareJobs[Math.min(polls++, prepareJobs.length - 1)];
      return jobs.get(id);
    },
    prepareEngine: () => assert.fail('never repost preparation'),
    createConversation: () => assert.fail('never fall back'),
    editorialReview: async (refs, requestId) => {
      calls.push(['editorial', refs]);
      const jobId = `editorial-${requestId}`;
      jobs.set(jobId, { id: jobId, kind: 'editorial_review', refId: requestId, status: 'completed',
        editorialOutcome: { accepted: refs, reused: refs, held: [] } });
      return { jobId, requestId, replayed: false };
    },
    createApproval: async (refs, requestId) => {
      calls.push(['approve', refs]);
      const result = { id: `approval-${refs[0].id}`, requestId, replayed: false };
      receipts.set(`approval:${requestId}`, { kind: 'approval', requestId, status: 'committed',
        payloadHash: localAdmissionPayloadHash({ proposals: refs }), result });
      return result;
    },
    execute: async (approvalId, requestId) => {
      calls.push(['execute', approvalId]); const proposalId = approvalId.slice('approval-'.length);
      const jobId = `execute-${proposalId}`;
      jobs.set(jobId, { id: jobId, kind: 'execute', refId: approvalId, status: 'completed' });
      operations.push({ id: `operation-${proposalId}`, approvalId, itemId: `i${proposalId.slice(1)}`, proposalId, status: 'succeeded' });
      const result = { approvalId, jobId, requestId, replayed: false };
      receipts.set(`execute:${requestId}`, { kind: 'execute', requestId, status: 'committed',
        payloadHash: localAdmissionPayloadHash({ approvalId }), result });
      return result;
    },
    localAdmission: async (kind, requestId) => { calls.push(['receipt', kind]); return receipts.get(`${kind}:${requestId}`); }
  };
  return { client, calls, jobs, operations };
}

test('prepare exposes durable exact revisions before terminal failure and retains them without duplicates', async t => {
  const path = await fixture(t); const { client } = api([job('running'), job('running'), job('failed')]); const events = [];
  await assert.rejects(generateProposals(client, parent.itemIds, { checkpoint: parent, checkpointPath: path, pollMs: 0,
    onProgress: event => events.push(event) }), error => error.code === 'JOB_FAILED' && error.details.readyProposals[0].revision === 1);
  const state = await saved(path);
  assert.equal(state.phase, 'prepare-failed'); assert.deepEqual(state.proposals.map(p => p.id), ['p1']);
  assert.equal(events.filter(e => e.event === 'prepare.ready').length, 1);
  assert.ok(events.findIndex(e => e.event === 'prepare.ready') < events.findIndex(e => e.event === 'prepare.failed'));
  assert.doesNotMatch(await readFile(path, 'utf8'), /PRIVATE_MODEL_ERROR/);
  client.getJob = () => assert.fail('terminal resume must not poll or regenerate');
  await assert.rejects(generateProposals(client, [], { checkpoint: state, checkpointPath: path }), { code: 'JOB_FAILED' });
});

test('autonomous execution joins first ready batch after observing partial preparation failure', async t => {
  const path = await fixture(t); const { client, calls } = api([job('running'), job('failed')]);
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'prepare-partial-failed');
  assert.equal(result.checkpoint.readyBatches[0].mode, 'complete');
  const prepPolls = calls.flatMap((call, index) => call[0] === 'poll' && call[1] === 'prepare' ? [index] : []);
  assert.equal(prepPolls.length, 2);
  assert.ok(calls.some(call => call[0] === 'execute'));
  const posts = calls.filter(c => ['approve', 'execute'].includes(c[0])); assert.equal(posts.length, 2);
  await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.deepEqual(calls.filter(c => ['approve', 'execute'].includes(c[0])), posts);
});

test('separate ready groups drain once each and final draft scan cannot execute them again', async t => {
  const path = await fixture(t); const { client, calls } = api([job('running'), job('running', [group(1), group(2)]), job('completed', [group(1), group(2)])]);
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete'); assert.equal(result.checkpoint.readyBatches.length, 2);
  assert.deepEqual(calls.filter(c => c[0] === 'approve').map(c => c[1]), [[{ id: 'p1', revision: 1 }], [{ id: 'p2', revision: 2 }]]);
  const count = calls.filter(c => c[0] === 'execute').length;
  await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(calls.filter(c => c[0] === 'execute').length, count);
});

for (const lost of ['approval', 'execute']) test(`lost ${lost} response resumes its child receipt without repeating POST`, async t => {
  const path = await fixture(t); const { client, calls } = api([job('running'), job('completed')]);
  const key = lost === 'approval' ? 'createApproval' : 'execute'; const original = client[key]; let fail = true;
  client[key] = async (...args) => { const result = await original(...args); if (fail) { fail = false;
    throw new UnknownMutationError('POST', lost === 'approval' ? '/api/approvals' : `/api/approvals/${args[0]}/execute`, { code: 'TIMEOUT' }); } return result; };
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 }), { code: 'READY_BATCH_STOPPED' });
  assert.equal((await saved(path)).readyBatches[0].mode, 'started');
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete');
  assert.equal(calls.filter(c => c[0] === 'approve').length, 1); // same-run unadmitted p2 is never consumed
  assert.equal(calls.filter(c => c[0] === 'execute' && c[1] === 'approval-p1').length, 1);
  assert.ok(calls.some(c => c[0] === 'receipt' && c[1] === lost));
});

test('missing started child fails closed and failed prepare-only refs can later use existing authority flags', async t => {
  const path = await fixture(t); const { client, calls } = api([job('failed')]);
  await assert.rejects(generateProposals(client, [], { checkpoint: parent, checkpointPath: path, pollMs: 0 }), { code: 'JOB_FAILED' });
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: false, pollMs: 0 });
  assert.equal(result.mode, 'prepare-partial-failed'); assert.equal(result.checkpoint.readyBatches[0].mode, 'approved');
  const files = await readdir(`${path}.ready`); await unlink(join(`${path}.ready`, files[0]));
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true }), { code: 'INVALID_CHECKPOINT' });
  assert.equal(calls.filter(c => c[0] === 'approve').length, 1); assert.equal(calls.filter(c => c[0] === 'execute').length, 0);
});

test('foreign group/lineage and incomplete history never produce consumable refs', async () => {
  const { client } = api([]);
  const foreign = job('running'); foreign.preparationStages.groupAdmission[0].itemIds = ['foreign'];
  await assert.rejects(discoverReadyProposals(client, parent, foreign), { code: 'INVALID_PREPARE_JOB' });
  const review = client.reviewItems;
  client.reviewItems = async ids => { const value = await review(ids); value.proposals[0].prepareRunId = 'other-run'; return value; };
  await assert.rejects(discoverReadyProposals(client, parent, job('running')), { code: 'INVALID_PREPARE_JOB' });
  client.reviewItems = async ids => ({ ...await review(ids), coverage: { operationsComplete: false } });
  await assert.rejects(discoverReadyProposals(client, parent, job('running')), { code: 'INCOMPLETE_OPERATION_COVERAGE' });
  client.reviewItems = async ids => ({ ...await review(ids), operations: [{ itemId: 'i1', status: 'unknown' }] });
  assert.deepEqual(await discoverReadyProposals(client, parent, job('running')), []);
});

test('crash after parent journal before child creation resumes the same exact batch', async t => {
  const path = await fixture(t); const { client, calls } = api([job('failed')]);
  const state = { ...parent, durableReadyObserved: true, proposals: [proposal(1)] };
  await assert.rejects(drainReadyBatches(client, state, { path, execute: true,
    save: async next => { await writeCheckpoint(path, next); throw new Error('interrupted after journal'); } }), /interrupted/);
  assert.equal((await saved(path)).readyBatches[0].mode, 'pending');
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'prepare-partial-failed');
  assert.equal(calls.filter(c => c[0] === 'approve').length, 1);
  assert.equal(calls.filter(c => c[0] === 'execute').length, 1);
});

test('completed child with stale started parent is recovered without repeating any POST', async t => {
  const path = await fixture(t); const { client, calls } = api([job('failed')]);
  await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  const state = await saved(path); state.readyBatches[0].mode = 'started'; await writeCheckpoint(path, state);
  const count = calls.length;
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'prepare-partial-failed');
  assert.equal(calls.length, count);
});

test('unconfirmed child approval cannot be reposted or allow later ready group execution', async t => {
  const path = await fixture(t); const { client, calls } = api([job('running'), job('completed', [group(1), group(2)])]);
  client.createApproval = async () => { calls.push(['approve']); throw new UnknownMutationError('POST', '/api/approvals', { code: 'TIMEOUT' }); };
  client.localAdmission = async () => null;
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 }), { code: 'READY_BATCH_STOPPED' });
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal(calls.filter(c => c[0] === 'approve').length, 1);
  assert.equal(calls.filter(c => c[0] === 'execute').length, 0);
  // Observation may advance, but the failed consumer never admits p2.
  assert.equal(calls.filter(c => c[0] === 'poll' && c[1] === 'prepare').length, 2);
});

test('blocked first send does not block later durable groups or parent terminal observation', async t => {
  const path = await fixture(t); const { client, calls } = api([
    job('running'), job('running', [group(1), group(2)]), job('completed', [group(1), group(2)])
  ]);
  let releaseSend; const senderWaiting = new Promise(resolve => { releaseSend = resolve; });
  let sendStarted; const started = new Promise(resolve => { sendStarted = resolve; });
  const originalGet = client.getJob; let sawLaterGroup = false;
  client.getJob = async id => {
    if (id === 'execute-p1') { sendStarted(); await senderWaiting; }
    // Force later readiness to arrive during the outstanding first send.
    if (id === 'prepare' && calls.filter(c => c[0] === 'poll' && c[1] === 'prepare').length === 1) await started;
    return originalGet(id);
  };
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0,
    onProgress: event => {
      if (event.event === 'prepare.ready' && event.proposals.some(p => p.id === 'p2')) sawLaterGroup = true;
      if (event.event === 'prepare.completed') {
        assert.equal(calls.filter(call => call[0] === 'execute').length, 1, 'only one sender while first child is blocked');
        releaseSend();
      }
    } });
  assert.equal(sawLaterGroup, true); assert.equal(result.mode, 'complete');
  const checkpoint = await saved(path);
  assert.equal(checkpoint.phase, 'prepared');
  assert.deepEqual(checkpoint.readyBatches.map(batch => batch.mode), ['complete', 'complete']);
  assert.deepEqual(checkpoint.proposals.map(ref => ref.id), ['p1', 'p2']);
  assert.equal(calls.filter(call => call[0] === 'execute').length, 2);
  const observedLater = calls.findIndex((call, index) => index > 0 && call[0] === 'poll' && call[1] === 'prepare');
  assert.ok(observedLater < calls.findIndex(call => call[0] === 'execute' && call[1] === 'approval-p2'));
  await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(calls.filter(call => call[0] === 'execute').length, 2);
});

test('UNKNOWN first child quarantines later journaled refs and resume never repeats its execute', async t => {
  const path = await fixture(t); const { client, calls, operations } = api([
    job('running'), job('running', [group(1), group(2)]), job('completed', [group(1), group(2)])
  ]);
  let release; const terminal = new Promise(resolve => { release = resolve; });
  let sendStarted; const started = new Promise(resolve => { sendStarted = resolve; });
  const originalExecute = client.execute;
  client.execute = async (...args) => {
    const result = await originalExecute(...args); operations.at(-1).status = 'unknown'; return result;
  };
  const originalGet = client.getJob;
  client.getJob = async id => {
    if (id === 'execute-p1') { sendStarted(); await terminal; }
    if (id === 'prepare' && calls.filter(c => c[0] === 'poll' && c[1] === 'prepare').length === 1) await started;
    return originalGet(id);
  };
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0,
    onProgress: event => { if (event.event === 'prepare.completed') release(); } }), { code: 'READY_BATCH_STOPPED' });
  const checkpoint = await saved(path);
  assert.equal(checkpoint.phase, 'prepared');
  assert.deepEqual(checkpoint.readyBatches.map(batch => batch.mode), ['needs-reconciliation', 'pending']);
  assert.deepEqual(checkpoint.readyBatches[0].operations.map(op => op.status), ['unknown']);
  assert.equal(calls.filter(call => call[0] === 'execute').length, 1);
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 }), { code: 'READY_BATCH_STOPPED' });
  assert.equal(calls.filter(call => call[0] === 'execute').length, 1);
  assert.equal(calls.filter(call => call[0] === 'approve').length, 1);
});

test('cancellation joins the blocked sender and resume observes its original execute job', async t => {
  const path = await fixture(t); const { client, calls } = api([
    job('running'), job('running', [group(1), group(2)]), job('completed', [group(1), group(2)])
  ]);
  const controller = new AbortController(); let sendStarted;
  const started = new Promise(resolve => { sendStarted = resolve; }); const originalGet = client.getJob;
  client.getJob = async id => {
    if (id === 'execute-p1') {
      sendStarted();
      await new Promise((resolve, reject) => {
        const stopped = () => reject(new CliError('Stopped observing existing execute job', { code: 'STOPPED' }));
        if (controller.signal.aborted) stopped(); else controller.signal.addEventListener('abort', stopped, { once: true });
      });
    }
    if (id === 'prepare' && calls.filter(c => c[0] === 'poll' && c[1] === 'prepare').length === 1) await started;
    return originalGet(id);
  };
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0,
    signal: controller.signal, onProgress: event => {
      if (event.event === 'prepare.ready' && event.proposals.some(p => p.id === 'p2')) controller.abort();
    } }), { code: 'READY_BATCH_STOPPED' });
  const checkpoint = await saved(path);
  assert.deepEqual(checkpoint.readyBatches.map(batch => batch.mode), ['started', 'pending']);
  const children = await readdir(`${path}.ready`);
  const child = await saved(join(`${path}.ready`, children[0]));
  assert.equal(child.executeJobId, 'execute-p1'); assert.equal(child.phase, 'executing');
  assert.equal(calls.filter(call => call[0] === 'execute').length, 1);
  client.getJob = originalGet;
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete');
  assert.equal(calls.filter(call => call[0] === 'execute' && call[1] === 'approval-p1').length, 1);
  assert.equal(calls.filter(call => call[0] === 'execute' && call[1] === 'approval-p2').length, 1);
});

for (const boundary of ['first-complete', 'editorial.request', 'approval.request', 'execute.request'])
test(`cancellation at ${boundary} prevents another admission and resumes original authority`, async t => {
  const path = await fixture(t); const { client, calls } = api([
    job('running'), job('running', [group(1), group(2)]), job('completed', [group(1), group(2)])
  ]);
  const controller = new AbortController(); let release; let start;
  const terminal = new Promise(resolve => { release = resolve; });
  const started = new Promise(resolve => { start = resolve; });
  const originalGet = client.getJob;
  client.getJob = async id => {
    if (id === 'execute-p1') { start(); await terminal; }
    if (id === 'prepare' && calls.filter(c => c[0] === 'poll' && c[1] === 'prepare').length === 1) await started;
    return originalGet(id);
  };
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0,
    signal: controller.signal, onProgress: event => {
      if (event.event === 'prepare.completed') release();
      if (boundary === 'first-complete' && event.event === 'execute.observed' && event.jobId === 'execute-p1' && event.phase === 'complete') controller.abort();
      if (event.event === boundary && (event.proposals?.some(ref => ref.id === 'p2') || event.approvalId === 'approval-p2')) controller.abort();
    } }), { code: 'READY_BATCH_STOPPED' });
  const checkpoint = await saved(path);
  assert.equal(checkpoint.readyBatches[0].mode, 'complete');
  assert.equal(calls.filter(call => call[0] === 'execute').length, 1);
  assert.equal(calls.filter(call => call[0] === 'approve').length, boundary === 'execute.request' ? 2 : 1);
  let retainedExecuteId;
  if (boundary !== 'first-complete') {
    const child = await saved(join(`${path}.ready`, `${checkpoint.readyBatches[1].id}.json`));
    assert.equal(child.pendingLocalAdmission, null);
    if (boundary === 'execute.request') {
      assert.equal(child.phase, 'approved'); assert.equal(child.approvalId, 'approval-p2');
      assert.ok(child.executeRequestId); retainedExecuteId = child.executeRequestId;
    }
  } else assert.equal(checkpoint.readyBatches[1].mode, 'pending');
  const originalExecute = client.execute;
  client.execute = async (...args) => {
    if (args[0] === 'approval-p2' && retainedExecuteId) assert.equal(args[1], retainedExecuteId);
    return originalExecute(...args);
  };
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete');
  assert.equal(calls.filter(call => call[0] === 'execute' && call[1] === 'approval-p1').length, 1);
  assert.equal(calls.filter(call => call[0] === 'execute' && call[1] === 'approval-p2').length, 1);
  assert.equal(calls.filter(call => call[0] === 'approve').length, 2);
});

test('fresh run cannot overwrite a checkpoint or retarget resumed selection/server path', async t => {
  const path = await fixture(t); const { client } = api([]); client.reviewItems = () => assert.fail('no read or mutation');
  const before = await readFile(path, 'utf8');
  await assert.rejects(runWorkflow(client, parent.itemIds, { checkpointPath: path, autonomous: true }), { code: 'USAGE' });
  await assert.rejects(runWorkflow(client, ['foreign'], { resumePath: path, autonomous: true }), { code: 'USAGE' });
  await assert.rejects(runWorkflow(client, [], { resumePath: path, checkpointPath: `${path}.other`, autonomous: true }), { code: 'USAGE' });
  assert.equal(await readFile(path, 'utf8'), before);
});

test('autonomous default persists and announces a recoverable parent before any API access', async t => {
  const { client } = api([]); let generatedPath;
  client.reviewItems = async () => { assert.ok(generatedPath); assert.deepEqual((await saved(generatedPath)).itemIds, parent.itemIds); throw new Error('offline stop'); };
  await assert.rejects(runWorkflow(client, parent.itemIds, { autonomous: true,
    onProgress: event => { if (event.event === 'checkpoint.created') generatedPath = event.checkpointPath; } }), /offline stop/);
  t.after(() => unlink(generatedPath));
  assert.equal((await saved(generatedPath)).phase, 'starting');
});

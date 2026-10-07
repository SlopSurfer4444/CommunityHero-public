import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { runBulk, reviewedReferences } from '../cli/bulk.mjs';
import { CliError, CommunityHeroClient, UnknownMutationError, localAdmissionPayloadHash } from '../cli/client.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';
import { resultExitCode } from '../cli/read-observer.mjs';

const refs = Array.from({ length: 3 }, (_, index) => ({ id: `p-${index}`, revision: 1, itemId: `i-${index}` }));
async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'ch-bulk-')); t.after(() => rm(directory, { recursive: true, force: true }));
  return join(directory, 'checkpoint.json');
}
async function saved(path) { return JSON.parse(await readFile(path, 'utf8')); }
function api(path, { statuses = {}, jobStatus = 'completed', partialHeld = false, editorialHeld = [] } = {}) {
  const calls = { approvals: [], editorials: [], executes: [], polls: [] }; const admissions = new Map(); const operations = []; const editorialJobs = new Map();
  const client = { account: 'LikeAvto', baseUrl: 'http://localhost:4186', calls,
    mutate: async (route, body) => {
      assert.equal(route, '/api/approvals'); const checkpoint = await saved(path);
      const slice = checkpoint.slices.find(row => row.approvalRequestId === body.requestId);
      assert.ok(slice); assert.equal(slice.payloadHash, localAdmissionPayloadHash(body)); assert.equal(slice.phase, 'approval-admitting');
      calls.approvals.push(body); const id = `a-${calls.approvals.length}`;
      const accepted = partialHeld ? body.proposals.slice(0, 1) : body.proposals;
      const held = partialHeld ? body.proposals.slice(1).map(reference => ({ reference, reason: 'unsafe', message: 'Review required', httpStatus: 409 })) : [];
      const result = { id: accepted.length ? id : null, requestId: body.requestId, status: accepted.length ? 'approved' : 'held', accepted, held, replayed: false };
      admissions.set(body.requestId, { kind: 'approval', requestId: body.requestId, status: 'committed', payloadHash: localAdmissionPayloadHash(body), result });
      return result;
    },
    localAdmission: async (_kind, key) => admissions.get(key),
    editorialReview: async (proposals, requestId) => {
      const checkpoint = await saved(path); const slice = checkpoint.slices.find(row => row.editorialReview?.requestId === requestId);
      assert.ok(slice); assert.equal(slice.editorialReview.payloadHash, localAdmissionPayloadHash({ proposals }));
      calls.editorials.push(proposals);
      const result = { jobId: `editorial-${requestId}`, requestId, replayed: false };
      admissions.set(requestId, { kind: 'editorial', requestId, status: 'committed', payloadHash: slice.editorialReview.payloadHash, result });
      const accepted = proposals.filter(ref => !editorialHeld.includes(ref.id));
      const held = proposals.filter(ref => editorialHeld.includes(ref.id)).map(reference => ({ reference, decision: 'revise', reason: 'Needs operator review', suggestedText: 'Do not apply automatically' }));
      editorialJobs.set(result.jobId, { id: result.jobId, kind: 'editorial_review', refId: requestId, status: 'completed', editorialOutcome: { accepted, reused: accepted, held } });
      return result;
    },
    execute: async (approvalId, requestId) => {
      const checkpoint = await saved(path); const slice = checkpoint.slices.find(row => row.admission?.id === approvalId);
      assert.ok(slice.executeAttemptId); assert.equal(slice.phase, 'execute-admitting'); calls.executes.push(approvalId);
      assert.equal(slice.executeAttemptId, requestId);
      assert.equal(slice.executePayloadHash, localAdmissionPayloadHash({ approvalId }));
      for (const ref of slice.admission.accepted) operations.push({ id: `o-${ref.id}`, approvalId, proposalId: ref.id,
        itemId: slice.references.find(row => row.id === ref.id).itemId, status: statuses[ref.id] || 'succeeded', providerRetryAllowed: false });
      const result = { jobId: `j-${approvalId}`, approvalId, requestId, replayed: false };
      admissions.set(requestId, { kind: 'execute', requestId, status: 'committed', payloadHash: slice.executePayloadHash, result });
      return result;
    },
    getJob: async id => { if (editorialJobs.has(id)) return editorialJobs.get(id); calls.polls.push(id); return { id, kind: 'execute', refId: id.slice(2), status: jobStatus }; },
    bootstrap: async () => ({ jobs: calls.executes.map(id => ({ id: `j-${id}`, kind: 'execute', refId: id, status: jobStatus })) }),
    reviewItems: async itemIds => ({ coverage: { operationsComplete: true }, operations: operations.filter(row => itemIds.includes(row.itemId)) })
  };
  return client;
}

test('bulk without explicit execute persists exact reviewed scope and performs no mutation', async t => {
  const path = await fixture(t); const client = api(path);
  const result = await runBulk(client, refs, { checkpointPath: path });
  assert.equal(result.mode, 'execution-required'); assert.equal(result.checkpoint.stopReason, 'explicit-execute-required');
  assert.deepEqual(result.checkpoint.references, refs); assert.equal(client.calls.approvals.length, 0); assert.equal(client.calls.executes.length, 0);
  const executed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(executed.mode, 'complete'); assert.equal(executed.summary.succeeded, 3);
});

test('bulk follows an admitted execution beyond 120 polls without creating another approval or execution', async t => {
  const path = await fixture(t); const client = api(path); const getJob = client.getJob; let polls = 0;
  client.getJob = async (id, options) => {
    const job = await getJob(id, options);
    return job.kind === 'execute' && ++polls <= 125 ? { ...job, status: 'running' } : job;
  };
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete'); assert.equal(polls, 126);
  assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
});

test('bulk Ctrl-C during an execution GET retains the bound job for inspection-only resume', async t => {
  const path = await fixture(t); const client = api(path); const getJob = client.getJob;
  const controller = new AbortController(); let started;
  const ready = new Promise(resolve => { started = resolve; });
  client.getJob = async (id, { signal } = {}) => {
    const job = await getJob(id);
    if (job.kind !== 'execute') return job;
    assert.equal(signal, controller.signal);
    return new Promise((resolve, reject) => {
      signal.addEventListener('abort', () => reject(new CliError('Stopped', { code: 'STOPPED' })), { once: true }); started();
    });
  };
  const pending = runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0, signal: controller.signal });
  await ready; controller.abort(); const result = await pending;
  assert.equal(result.checkpoint.stopReason, 'operator-stopped');
  assert.equal(result.checkpoint.slices[0].executeJobId, 'j-a-1');
  assert.equal(result.checkpoint.slices[0].phase, 'executing');
  client.getJob = getJob;
  assert.equal((await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 })).mode, 'complete');
  assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
});

const independent = { execute: true, partialAdmission: true, continuationPolicy: 'continue-independent', pollMs: 0 };

test('mixed editorial slice applies only explicit independent accepted subset and keeps original scope and suggestions', async t => {
  const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'] }); const events = [];
  const result = await runBulk(client, refs, { ...independent, checkpointPath: path, onProgress: event => events.push(event) });
  assert.equal(result.mode, 'complete-with-holds'); assert.equal(result.summary.succeeded, 2);
  assert.equal(result.summary.submitted, 3); assert.equal(result.summary.accepted, 2);
  assert.deepEqual(result.checkpoint.references, refs); assert.deepEqual(result.checkpoint.slices[0].references, refs);
  const accepted = [refs[0], refs[2]]; const exact = accepted.map(({ id, revision }) => ({ id, revision }));
  assert.deepEqual(result.checkpoint.slices[0].approvalReferences, accepted);
  assert.equal(result.checkpoint.slices[0].approvalScopeHash, localAdmissionPayloadHash({ references: accepted }));
  assert.deepEqual(client.calls.approvals[0].proposals, exact);
  assert.deepEqual(events.find(event => event.event === 'bulk.approval.request').proposals, exact);
  assert.equal(result.summary.editorialHeld[0].suggestedText, 'Do not apply automatically');
  await runBulk(client, [], { resumePath: path, execute: true });
  assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1); assert.equal(client.calls.editorials.length, 1);
});

for (const options of [{ partialAdmission: true }, { continuationPolicy: 'continue-independent' }]) {
  test(`mixed editorial subset needs BOTH opt-ins: ${JSON.stringify(options)}`, async t => {
    const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'] });
    await runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0, ...options });
    assert.equal(client.calls.approvals.length, 0); assert.equal(client.calls.executes.length, 0);
  });
}

test('all editorial-held slice makes no approval but independent later slice proceeds', async t => {
  const path = await fixture(t); const client = api(path, { editorialHeld: ['p-0', 'p-1'] });
  const result = await runBulk(client, refs, { ...independent, checkpointPath: path, batchSize: 2 });
  assert.equal(result.summary.editorialHeld.length, 2); assert.equal(result.summary.succeeded, 1);
  assert.deepEqual(client.calls.approvals[0].proposals, [{ id: 'p-2', revision: 1 }]);
});

test('restart after durable mixed verdict before subset persistence reuses review and derives exact subset', async t => {
  const path = await fixture(t); const client = api(path);
  await runBulk(client, refs, { ...independent, execute: false, checkpointPath: path });
  const state = await saved(path); const proposals = refs.map(({ id, revision }) => ({ id, revision }));
  state.phase = 'running'; state.slices[0].phase = 'editorial-held';
  state.slices[0].editorialReview = { requestId: 'saved-review', phase: 'held', proposals,
    payloadHash: localAdmissionPayloadHash({ proposals }), outcome: { accepted: proposals.slice(0, 2), reused: [],
      held: [{ reference: proposals[2], decision: 'hold', reason: 'Need context' }] } };
  await writeCheckpoint(path, state);
  const result = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(result.summary.succeeded, 2); assert.equal(client.calls.editorials.length, 0);
  assert.deepEqual(client.calls.approvals[0].proposals, proposals.slice(0, 2));
});

for (const stage of ['approval', 'execute']) {
  test(`mixed subset survives lost ${stage} response without resubmitting or widening scope`, async t => {
    const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'] });
    const method = stage === 'approval' ? 'mutate' : 'execute'; const original = client[method];
    client[method] = async (...args) => { await original(...args); throw new UnknownMutationError('POST', '/api/approvals', { name: 'TimeoutError' }); };
    await runBulk(client, refs, { ...independent, checkpointPath: path });
    const result = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
    assert.equal(result.summary.succeeded, 2); assert.equal(result.summary.editorialHeld.length, 1);
    assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1); assert.equal(client.calls.editorials.length, 1);
    assert.deepEqual(client.calls.approvals[0].proposals, [{ id: 'p-0', revision: 1 }, { id: 'p-2', revision: 1 }]);
  });
}

test('editorial accepted subset still passes server currentness admission and preserves an UNKNOWN without replay', async t => {
  const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'], partialHeld: true, statuses: { 'p-0': 'unknown' } });
  const result = await runBulk(client, refs, { ...independent, checkpointPath: path });
  assert.equal(result.summary.unknown, 1); assert.equal(result.summary.held.length, 1); assert.equal(result.summary.editorialHeld.length, 1);
  assert.equal(result.summary.held[0].reference.id, 'p-2');
  await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(client.calls.executes.length, 1); assert.equal(client.calls.approvals.length, 1);
});

test('shared authority failure after editorial partition stops all execution', async t => {
  const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'] });
  client.mutate = async () => { throw new CliError('Company permission revoked', { code: 'FORBIDDEN' }); };
  const result = await runBulk(client, refs, { ...independent, checkpointPath: path });
  assert.equal(result.mode, 'stopped'); assert.equal(client.calls.executes.length, 0);
  assert.equal(result.checkpoint.error.code, 'FORBIDDEN');
});

test('1200 reviewed refs retain a complete disjoint outcome partition across mixed slices and lost approval recovery', async t => {
  const path = await fixture(t);
  const many = Array.from({ length: 1200 }, (_, index) => ({ id: `p-${index}`, revision: 1, itemId: `i-${index}` }));
  const editorialHeld = many.filter((_, index) => index % 6 === 0).map(ref => ref.id);
  const client = api(path, { editorialHeld, statuses: { 'p-1': 'unknown' } });
  const original = client.mutate; let lost = false;
  client.mutate = async (...args) => {
    const result = await original(...args);
    if (!lost && client.calls.approvals.length === 5) { lost = true; throw new UnknownMutationError('POST', '/api/approvals', { name: 'TimeoutError' }); }
    return result;
  };
  const interrupted = await runBulk(client, many, { ...independent, checkpointPath: path });
  assert.equal(interrupted.checkpoint.stopReason, 'mutation-outcome-unknown');
  const result = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(result.summary.succeeded, 999); assert.equal(result.summary.unknown, 1);
  assert.equal(result.summary.editorialHeld.length, 200); assert.equal(result.summary.remaining, 0);
  assert.equal(client.calls.approvals.length, 12); assert.equal(client.calls.executes.length, 12);
  const outcomes = result.checkpoint.slices.flatMap(slice => [
    ...(slice.operations || []).map(op => op.proposalId),
    ...(slice.editorialReview?.outcome?.held || []).map(hold => hold.reference.id),
    ...(slice.admission?.held || []).map(hold => hold.reference.id)
  ]);
  assert.equal(outcomes.length, 1200); assert.equal(new Set(outcomes).size, 1200);
  assert.deepEqual(new Set(outcomes), new Set(many.map(ref => ref.id)));
  assert.deepEqual(result.checkpoint.references, many);
  await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(client.calls.approvals.length, 12); assert.equal(client.calls.executes.length, 12);
});

for (const corruption of ['widen', 'revision', 'binding', 'drop', 'drop-both', 'hash']) {
  test(`resume rejects derived editorial scope corruption: ${corruption}`, async t => {
    const path = await fixture(t); const client = api(path, { editorialHeld: ['p-1'] });
    await runBulk(client, refs, { ...independent, checkpointPath: path });
    const state = await saved(path); const slice = state.slices[0];
    if (corruption === 'widen') slice.approvalReferences = refs;
    if (corruption === 'revision') slice.approvalReferences[0].revision += 1;
    if (corruption === 'binding') slice.approvalReferences[0].itemId = 'different';
    if (corruption === 'drop') delete slice.approvalReferences;
    if (corruption === 'drop-both') {
      delete slice.approvalReferences; delete slice.approvalScopeHash;
      slice.payloadHash = localAdmissionPayloadHash({ proposals: refs.map(({ id, revision }) => ({ id, revision })), admissionMode: 'partial' });
    }
    if (corruption === 'hash') slice.approvalScopeHash = 'wrong';
    else if (slice.approvalReferences) slice.approvalScopeHash = localAdmissionPayloadHash({ references: slice.approvalReferences });
    await writeCheckpoint(path, state);
    await assert.rejects(runBulk(client, [], { resumePath: path, execute: true }), { code: 'INVALID_CHECKPOINT' });
    assert.equal(client.calls.executes.length, 1);
  });
}

test('known failed dependency stops finitely without creating or waiting for next slice', async t => {
  const path = await fixture(t); const client = api(path, { statuses: { 'p-0': 'failed' }, jobStatus: 'failed' });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, batchSize: 1, pollMs: 0 });
  assert.equal(result.mode, 'stopped'); assert.equal(result.checkpoint.stopReason, 'execution-job-failed');
  assert.equal(result.summary.failed, 1); assert.equal(result.summary.remaining, 2); assert.equal(client.calls.approvals.length, 1);
  assert.deepEqual(client.calls.polls, ['j-a-1']);
  await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(client.calls.executes.length, 1); assert.equal(client.calls.polls.length, 1);
});

test('explicit independent continuation keeps UNKNOWN immutable and advances other exact slices', async t => {
  const path = await fixture(t); const client = api(path, { statuses: { 'p-0': 'unknown', 'p-1': 'failed' } });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, batchSize: 1, continuationPolicy: 'continue-independent', pollMs: 0 });
  assert.equal(result.mode, 'stopped'); assert.equal(result.summary.unknown, 1); assert.equal(result.summary.failed, 1); assert.equal(result.summary.succeeded, 1);
  assert.equal(client.calls.executes.length, 3);
  await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(client.calls.executes.length, 3);
});

test('default stops on UNKNOWN and never retries an existing external operation', async t => {
  const path = await fixture(t); const client = api(path, { statuses: { 'p-0': 'unknown' } });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, batchSize: 1, pollMs: 0 });
  assert.equal(result.checkpoint.stopReason, 'operation-outcomes-unresolved'); assert.equal(client.calls.executes.length, 1);
  await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 }); assert.equal(client.calls.executes.length, 1);
});

test('lost approval response recovers immutable receipt after restart without a second POST', async t => {
  const path = await fixture(t); const client = api(path); const original = client.mutate;
  client.mutate = async (...args) => { await original(...args); throw new UnknownMutationError('POST', '/api/approvals', { name: 'TimeoutError' }); };
  const stopped = await runBulk(client, refs, { checkpointPath: path, execute: true });
  assert.equal(stopped.mode, 'stopped'); assert.equal(stopped.checkpoint.slices[0].phase, 'unknown');
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(resumed.mode, 'complete'); assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
});

test('lost editorial response resumes the exact review job before approval without a repeated model admission', async t => {
  const path = await fixture(t); const client = api(path); const original = client.editorialReview;
  client.editorialReview = async (...args) => { await original(...args); throw new UnknownMutationError('POST', '/api/proposals/editorial-review', { name: 'TimeoutError' }); };
  const stopped = await runBulk(client, refs, { checkpointPath: path, execute: true });
  assert.equal(stopped.checkpoint.slices[0].phase, 'unknown'); assert.equal(client.calls.approvals.length, 0);
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(resumed.mode, 'complete'); assert.equal(client.calls.editorials.length, 1);
  assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
});

test('crash after execute admission recovers exact receipt even outside bounded bootstrap history', async t => {
  const path = await fixture(t); const client = api(path); const execute = client.execute;
  client.execute = async (...args) => { await execute(...args); throw new UnknownMutationError('POST', '/api/approvals/a-1/execute', { name: 'TimeoutError' }); };
  await runBulk(client, refs, { checkpointPath: path, execute: true });
  client.bootstrap = () => assert.fail('keyed recovery must not search bounded history');
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(resumed.mode, 'complete'); assert.equal(client.calls.executes.length, 1);
});

test('missing execution job gives finite inspectable stop and no guessed waiter or retry', async t => {
  const path = await fixture(t); const client = api(path);
  client.execute = async () => { throw new UnknownMutationError('POST', '/api/approvals/a-1/execute', { name: 'TimeoutError' }); };
  await runBulk(client, refs, { checkpointPath: path, execute: true });
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(resumed.checkpoint.slices[0].phase, 'unknown'); assert.equal(client.calls.polls.length, 0); assert.equal(client.calls.approvals.length, 1);
});

for (const hasJob of [false, true]) {
  test(`legacy local-only execute attempt retains ${hasJob ? 'existing job recovery' : 'HOLD'} and is never queried as a receipt`, async t => {
    const path = await fixture(t); const client = api(path); const execute = client.execute;
    client.execute = async (...args) => { if (hasJob) await execute(...args); throw new UnknownMutationError('POST', '/api/approvals/a-1/execute', { name: 'TimeoutError' }); };
    await runBulk(client, refs, { checkpointPath: path, execute: true });
    const state = await saved(path); delete state.slices[0].executePayloadHash; delete state.slices[0].executeAdmissionProtocol;
    delete state.slices[0].editorialReview;
    await writeCheckpoint(path, state);
    client.localAdmission = () => assert.fail('legacy attempt was never a receipt key');
    client.execute = () => assert.fail('legacy execute must never be repeated');
    const result = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
    assert.equal(client.calls.editorials.length, 1); // No retroactive review of an admitted v53 execution.
    assert.equal(result.mode, hasJob ? 'complete' : 'stopped');
    if (!hasJob) assert.equal(result.checkpoint.stopReason, 'execution-admission-unconfirmed');
  });
}

test('bulk rejects tampered execute approval binding before receipt lookup or dispatch', async t => {
  const path = await fixture(t); const client = api(path);
  client.execute = async () => { throw new UnknownMutationError('POST', '/api/approvals/a-1/execute', { name: 'TimeoutError' }); };
  await runBulk(client, refs, { checkpointPath: path, execute: true });
  const state = await saved(path); state.slices[0].admission.id = 'other-approval'; await writeCheckpoint(path, state);
  client.localAdmission = () => assert.fail('tampered binding cannot read receipt');
  await assert.rejects(runBulk(client, [], { resumePath: path, execute: true }), { code: 'INVALID_CHECKPOINT' });
});

test('execute request collision stops without a replacement key or retry', async t => {
  const path = await fixture(t); const client = api(path); let posts = 0;
  client.execute = async () => { posts += 1; throw new CliError('Request key collision', { code: 'STALE_OR_CONFLICT', status: 409 }); };
  const first = await runBulk(client, refs, { checkpointPath: path, execute: true });
  const key = first.checkpoint.slices[0].executeAttemptId;
  assert.equal(first.checkpoint.slices[0].phase, 'failed');
  const resumed = await runBulk(client, [], { resumePath: path, execute: true });
  assert.equal(resumed.checkpoint.slices[0].executeAttemptId, key); assert.equal(posts, 1);
});

test('running job polling is bounded and restart uses existing job', async t => {
  const path = await fixture(t); const client = api(path, { jobStatus: 'running' });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, maxPolls: 2, pollMs: 0 });
  assert.equal(result.checkpoint.stopReason, 'poll-limit'); assert.equal(client.calls.polls.length, 2);
  client.getJob = async id => ({ id, kind: 'execute', refId: id.slice(2), status: 'completed' });
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 }); assert.equal(resumed.mode, 'complete'); assert.equal(client.calls.executes.length, 1);
});

test('partial admission is opt-in and all hold references and reasons remain durable', async t => {
  const path = await fixture(t); const client = api(path, { partialHeld: true });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, partialAdmission: true, continuationPolicy: 'continue-independent', pollMs: 0 });
  assert.equal(result.mode, 'complete-with-holds'); assert.equal(client.calls.approvals[0].admissionMode, 'partial');
  assert.deepEqual(result.summary.held.map(hold => hold.reference.id), ['p-1', 'p-2']); assert.equal(result.summary.held[0].reason, 'unsafe'); assert.equal(result.summary.succeeded, 1);
  await assert.rejects(runBulk(client, [], { resumePath: path, execute: true, partialAdmission: false }), { code: 'USAGE' });
});

test('misbound receipt never admits and changed checkpoint scope never dispatches', async t => {
  const path = await fixture(t); const client = api(path);
  client.mutate = async () => { throw new UnknownMutationError('POST', '/api/approvals', { name: 'TimeoutError' }); };
  await runBulk(client, refs, { checkpointPath: path, execute: true });
  client.localAdmission = async () => ({ kind: 'approval', requestId: 'wrong', status: 'committed', payloadHash: 'wrong', result: {} });
  const resumed = await runBulk(client, [], { resumePath: path, execute: true }); assert.equal(resumed.mode, 'stopped'); assert.equal(client.calls.executes.length, 0);
  const checkpoint = await saved(path); checkpoint.references[0].revision = 2; await writeCheckpoint(path, checkpoint);
  await assert.rejects(runBulk(client, [], { resumePath: path, execute: true }), { code: 'INVALID_CHECKPOINT' });
});

test('atomic admission rejection stops before execution and keeps reviewed refs', async t => {
  const path = await fixture(t); const client = api(path); client.mutate = async () => { throw new CliError('Dependency stale', { code: 'STALE_OR_CONFLICT', status: 409 }); };
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true });
  assert.equal(result.checkpoint.stopReason, 'approval-dependency-failed'); assert.equal(client.calls.executes.length, 0); assert.deepEqual(result.checkpoint.references, refs);
  assert.throws(() => reviewedReferences([{ id: 'p', revision: 0 }]), { code: 'USAGE' });
});

test('partial flag executes accepted subset and default policy stops before next slice', async t => {
  const path = await fixture(t); const client = api(path, { partialHeld: true });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, partialAdmission: true, batchSize: 2, pollMs: 0 });
  assert.equal(result.checkpoint.stopReason, 'partial-admission-held'); assert.equal(result.summary.succeeded, 1); assert.equal(result.summary.remaining, 1);
  assert.equal(client.calls.executes.length, 1);
  const resumed = await runBulk(client, [], { resumePath: path, execute: true });
  assert.equal(resumed.checkpoint.stopReason, 'partial-admission-held'); assert.equal(client.calls.approvals.length, 1);
});

test('malformed hold is UNKNOWN without a raw TypeError or accidental execution', async t => {
  const path = await fixture(t); const client = api(path);
  client.mutate = async (_route, body) => ({ id: null, requestId: body.requestId, status: 'held', accepted: [], held: [{ reason: 'bad' }] });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, partialAdmission: true });
  assert.equal(result.checkpoint.error.code, 'UNKNOWN_MUTATION_OUTCOME'); assert.equal(client.calls.executes.length, 0);
});

test('101 reviewed proposals split into exact finite slices no larger than 100', async t => {
  const path = await fixture(t); const client = api(path);
  const many = Array.from({ length: 101 }, (_, index) => ({ id: `p-${index}`, revision: 1, itemId: `i-${index}` }));
  const result = await runBulk(client, many, { checkpointPath: path, execute: true, pollMs: 0 });
  assert.equal(result.mode, 'complete'); assert.deepEqual(client.calls.approvals.map(body => body.proposals.length), [100, 1]);
  assert.equal(result.summary.succeeded, 101); assert.ok(client.calls.approvals.every(body => body.admissionMode === undefined));
});

test('checkpoint lock prevents concurrent execution and preserves stale owner evidence', async t => {
  const path = await fixture(t); await writeFile(`${path}.lock`, '{"pid":12345}');
  await assert.rejects(runBulk(api(path), refs, { checkpointPath: path, execute: true }), { code: 'CHECKPOINT_LOCKED' });
  assert.equal(await readFile(`${path}.lock`, 'utf8'), '{"pid":12345}');
});

test('CLI bulk parses file and policy with one trusted generation read and no mutation before explicit execution', async t => {
  const path = await fixture(t); const proposalFile = `${path}.proposals.json`; await writeFile(proposalFile, JSON.stringify({ proposals: refs }));
  const shim=`${path}.fetch.mjs`;const generation='e4e1d9f2-49b8-4b48-846f-ab113f519f89';
  await writeFile(shim,`globalThis.fetch=async(url,options)=>{
    if(options.method!=='GET'||url.pathname!=='/api/session')throw new Error('Unexpected fixture dispatch');
    return new Response(JSON.stringify({csrfToken:'fixture',storageGeneration:${JSON.stringify(generation)}}),{headers:{'x-communityhero-workspace-generation':${JSON.stringify(generation)}}});
  };`);
  const { stdout } = await promisify(execFile)(process.execPath, ['--import',pathToFileURL(shim).href,'mvp/cli/communityhero.mjs', 'bulk', '--account', 'LikeAvto',
    '--proposals-file', proposalFile, '--checkpoint', path, '--partial-admission', '--continuation-policy', 'continue-independent'], { cwd: process.cwd(),windowsHide:true,timeout:5000,
      env:{...process.env,COMMUNITYHERO_SESSION:''} });
  const result = JSON.parse(stdout); assert.equal(result.mode, 'execution-required'); assert.equal(result.checkpoint.partialAdmission, true);
  assert.equal(result.checkpoint.workflowGeneration,generation);
  assert.equal(result.checkpoint.continuationPolicy, 'continue-independent'); assert.equal(result.summary.remaining, 3);
});

for (const [code, status] of [['HTTP_ERROR', 500], ['INVALID_RESPONSE', 200], ['INVALID_RESPONSE', 500], ['READ_TIMEOUT', 200]]) {
  test(`bulk recovers bounded ${code}/${status} GET loss after execute without another admission`, async t => {
    const path = await fixture(t); const client = api(path, { statuses: { 'p-2': 'failed' } });
    const original = client.getJob; let executionReads = 0;
    client.getJob = async (...args) => {
      const job = await original(...args);
      if (job.kind === 'execute' && ++executionReads <= 2)
        throw new CliError('private-password-token-canary', { code, status });
      return job;
    };
    const review = client.reviewItems;
    client.reviewItems = async ids => {
      assert.equal((await saved(path)).slices[0].lastJob.status, 'completed', 'terminal job is checkpointed before outcome GET');
      return review(ids);
    };
    const result = await runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0 });
    assert.equal(result.summary.succeeded, 2); assert.equal(result.summary.failed, 1);
    assert.equal(resultExitCode(result), 1); assert.equal(executionReads, 3);
    assert.equal(result.checkpoint.slices[0].observation.retryCount, 2);
    assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
    assert.ok(!JSON.stringify(result).includes('private-password-token-canary'));
  });
}

test('bulk exhausted outcome GET preserves terminal durable job and resumes by reads of its original scope', async t => {
  const path = await fixture(t); const client = api(path); const original = client.reviewItems; let reads = 0;
  client.reviewItems = async () => { reads++; throw new CliError('secret-body-canary', { code: 'HTTP_ERROR', status: 500 }); };
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0 });
  assert.equal(reads, 3); assert.equal(resultExitCode(result), 4); assert.equal(result.summary.failed, 0);
  assert.equal(result.checkpoint.slices[0].lastJob.status, 'completed');
  assert.equal(result.checkpoint.slices[0].phase, 'needs-reconciliation');
  assert.equal(result.checkpoint.slices[0].observation.state, 'exhausted');
  assert.ok(!JSON.stringify(result).includes('secret-body-canary'));
  client.reviewItems = original;
  const resumed = await runBulk(client, [], { resumePath: path, execute: true, pollMs: 0 });
  assert.equal(resultExitCode(resumed), 0); assert.equal(resumed.summary.succeeded, 3);
  assert.equal(new Set(resumed.checkpoint.slices[0].operations.map(op => op.id)).size, 3);
  assert.equal(client.calls.approvals.length, 1); assert.equal(client.calls.executes.length, 1);
});

test('empty successful read is a typed transient observation failure, with no HTTP body retained', async () => {
  const client = new CommunityHeroClient({ account: 'BAW', fetchImpl: async () => new Response('', { status: 200 }) });
  await assert.rejects(client.request('/api/engine/jobs/exact-job'), { code: 'INVALID_RESPONSE', status: 200 });
});

test('35 accepted refs retain 33 native successes and two predispatch failures after bounded observer reconnect', async t => {
  const path = await fixture(t); const scope = Array.from({ length: 35 }, (_, index) => ({ id: `scope-${index}`, revision: 1, itemId: `recipient-${index}` }));
  const client = api(path, { statuses: { 'scope-33': 'failed', 'scope-34': 'failed' } });
  const original = client.getJob; let reads = 0;
  client.getJob = async (...args) => {
    const job = await original(...args);
    if (job.kind === 'execute' && ++reads <= 2) throw new CliError('GET failed', { code: 'HTTP_ERROR', status: 500 });
    return job;
  };
  const result = await runBulk(client, scope, { checkpointPath: path, execute: true, pollMs: 0 });
  assert.equal(result.summary.accepted, 35); assert.equal(result.summary.succeeded, 33); assert.equal(result.summary.failed, 2); assert.equal(result.summary.unknown, 0);
  assert.equal(resultExitCode(result), 1); assert.equal(client.calls.executes.length, 1); assert.equal(client.calls.approvals.length, 1);
});

test('bulk GET retry budget is shared across the terminal job and its exact native outcome read', async t => {
  const path = await fixture(t); const client = api(path); const original = client.getJob; let jobs = 0; let outcomes = 0;
  client.getJob = async (...args) => {
    const job = await original(...args);
    if (job.kind === 'execute' && ++jobs <= 2) throw new CliError('Read timeout', { code: 'READ_TIMEOUT' });
    return job;
  };
  client.reviewItems = async () => { outcomes++; throw new CliError('Outcome read lost', { code: 'NETWORK_ERROR' }); };
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, pollMs: 0 });
  assert.equal(jobs, 3); assert.equal(outcomes, 1); assert.equal(resultExitCode(result), 4);
  assert.equal(result.checkpoint.slices[0].lastJob.status, 'completed'); assert.equal(result.checkpoint.slices[0].observation.retryCount, 2);
  assert.equal(client.calls.executes.length, 1);
});

for (const [scenario, exitCode] of [['failed', 1], ['unknown', 4], ['exhausted', 4], ['operator', 130]]) {
  test(`actual CLI returns ${exitCode} for returned bulk ${scenario} without network or POST replay`, async t => {
    const path = await fixture(t); const seed = await runBulk(api(path), refs, { checkpointPath: path, execute: true, pollMs: 0 });
    seed.checkpoint.phase = 'running'; seed.checkpoint.slices[0].phase = 'executing'; seed.checkpoint.slices[0].operations = [];
    await writeCheckpoint(path, seed.checkpoint);
    const shim = `${path}.fetch.mjs`;
    const review = { account: 'LikeAvto', selectedItemIds: refs.map(ref => ref.itemId), items: refs.map(ref => ({ id: ref.itemId })), proposals: [],
      operations: refs.map((ref, index) => ({ id: `o-${ref.id}`, approvalId: 'a-1', proposalId: ref.id, itemId: ref.itemId,
        status: index === 0 && ['failed', 'unknown'].includes(scenario) ? scenario : 'succeeded' })),
      coverage: { itemsReturned: 3, operationsReturned: 3, operationsComplete: true, historyTruncated: false } };
    await writeFile(shim, `globalThis.fetch = async (url, options) => {
      if (options.method !== 'GET') throw new Error('POST replay is forbidden');
      if (${JSON.stringify(scenario)} === 'operator') process.emit('SIGINT');
      if (${JSON.stringify(scenario)} === 'exhausted') return new Response(JSON.stringify({error:'private-cli-secret-canary'}), {status:500});
      return new Response(JSON.stringify(url.pathname.startsWith('/api/engine/jobs/')
        ? {id:'j-a-1',kind:'execute',refId:'a-1',status:'completed'} : ${JSON.stringify(review)}));
    };`, 'utf8');
    const result = await promisify(execFile)(process.execPath, ['--import', pathToFileURL(shim).href, 'mvp/cli/communityhero.mjs', 'bulk', '--account', 'LikeAvto',
      '--base-url', seed.checkpoint.baseUrl, '--resume', path, '--execute', '--poll-ms', '1'], { cwd: process.cwd(), windowsHide: true, timeout: 5000,
      env: { ...process.env, COMMUNITYHERO_SESSION: '' } }).then(value => ({ ...value, code: 0 }), error => error);
    assert.equal(result.code, exitCode, result.stderr);
    assert.ok(!`${result.stdout}${result.stderr}${await readFile(path, 'utf8')}`.includes('private-cli-secret-canary'));
    assert.equal(JSON.parse(result.stdout).mode, 'stopped');
  });
}

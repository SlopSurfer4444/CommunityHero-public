import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CommunityHeroClient, UnknownMutationError, localAdmissionPayloadHash } from '../cli/client.mjs';
import { editorialOutcome, settleEditorialReview } from '../cli/editorial.mjs';
import { runEditorialReview, runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { runBulk } from '../cli/bulk.mjs';

const refs = [{ id: 'p1', revision: 1, itemId: 'i1' }, { id: 'p2', revision: 2, itemId: 'i2' }];
const exact = refs.map(({ id, revision }) => ({ id, revision }));
const accepted = () => ({ accepted: exact, reused: exact, held: [] });
const held = () => ({ accepted: exact.slice(0, 1), reused: [], held: [{ reference: exact[1], decision: 'revise', reason: 'Tone needs review', suggestedText: 'Suggestion only' }] });
const prepared = { account: 'LikeAvto', baseUrl: 'http://localhost:4186', phase: 'prepared', itemIds: refs.map(ref => ref.itemId),
  instruction: 'prepare', materialsReady: true, prepareJobId: 'prepare-job', prepareTransport: 'engine',
  proposals: refs.map(ref => ({ ...ref, kind: 'reply_and_close', text: `Exact ${ref.id}` })) };
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-editorial-')); t.after(() => rm(dir, { recursive: true, force: true }));
  return join(dir, 'checkpoint.json');
}
const read = async path => JSON.parse(await readFile(path, 'utf8'));
function api(path, { loseResponse = false, outcome = accepted() } = {}) {
  const calls = { editorial: 0, approval: 0, execute: 0, receipts: 0 }; const jobs = new Map(); let receipt;
  return { ...prepared, account: prepared.account, baseUrl: prepared.baseUrl, calls, jobs,
    editorialReview: async (proposals, requestId) => {
      calls.editorial += 1;
      const checkpoint = await read(path);
      const review = checkpoint.editorialReview || checkpoint.slices?.find(slice => slice.editorialReview?.requestId === requestId)?.editorialReview;
      assert.equal(review.requestId, requestId); assert.equal(review.payloadHash, localAdmissionPayloadHash({ proposals }));
      assert.equal(review.phase, 'admitting'); assert.deepEqual(proposals, exact);
      const result = { jobId: 'editorial-job', requestId, replayed: false };
      receipt = { kind: 'editorial', requestId, payloadHash: review.payloadHash, status: 'committed', result };
      jobs.set(result.jobId, { id: result.jobId, kind: 'editorial_review', refId: requestId, status: 'completed', editorialOutcome: outcome });
      if (loseResponse) throw new UnknownMutationError('POST', '/api/proposals/editorial-review', { name: 'TimeoutError' });
      return result;
    },
    localAdmission: async (kind, key) => { calls.receipts += 1; assert.equal(kind, 'editorial'); assert.equal(key, receipt.requestId); return receipt; },
    getJob: async id => jobs.get(id),
    reviewItems: async () => ({ items: refs.map(ref => ({ id: ref.itemId })), proposals: prepared.proposals.map(row => ({ ...row, status: 'draft', prepareRunId: 'prepare-job' })),
      operations: [], coverage: { operationsComplete: true } }),
    createApproval: async (proposals, requestId) => { calls.approval += 1; assert.deepEqual(proposals, exact); return { id: 'approval', requestId }; },
    mutate: async () => { calls.approval += 1; assert.fail('held editorial must not approve'); },
    execute: async () => { calls.execute += 1; assert.fail('must not execute'); }
  };
}

test('standalone durable editorial review recovers lost response and reuses saved outcome without any duplicate job', async t => {
  const path = await fixture(t); const client = api(path, { loseResponse: true });
  await assert.rejects(runEditorialReview(client, exact, { checkpointPath: path }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal((await read(path)).pendingLocalAdmission.kind, 'editorial');
  const resumed = await runEditorialReview(client, [], { resumePath: path, pollMs: 0 });
  assert.equal(resumed.mode, 'editorial-reviewed'); assert.deepEqual(resumed.outcome, accepted());
  await runEditorialReview(client, [], { resumePath: path });
  assert.deepEqual(client.calls, { editorial: 1, approval: 0, execute: 0, receipts: 1 });
});

test('workflow editorial hold retains exact texts and suggestions but never creates approval or edits a draft', async t => {
  const path = await fixture(t); await writeCheckpoint(path, prepared); const client = api(path, { outcome: held() });
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true });
  assert.equal(result.mode, 'editorial-held'); assert.deepEqual(result.checkpoint.proposals, prepared.proposals);
  assert.equal(result.checkpoint.editorialReview.outcome.held[0].suggestedText, 'Suggestion only');
  await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true });
  assert.deepEqual(client.calls, { editorial: 1, approval: 0, execute: 0, receipts: 0 });
});

test('bulk editorial hold does not silently shrink the reviewed approval even with partial admission enabled', async t => {
  const path = await fixture(t); const client = api(path, { outcome: held() });
  const result = await runBulk(client, refs, { checkpointPath: path, execute: true, partialAdmission: true, pollMs: 0 });
  assert.equal(result.checkpoint.stopReason, 'editorial-review-held'); assert.deepEqual(result.checkpoint.references, refs);
  assert.equal(result.summary.editorialHeld.length, 1); assert.equal(result.summary.accepted, 0);
  await runBulk(client, [], { resumePath: path, execute: true });
  assert.deepEqual(client.calls, { editorial: 1, approval: 0, execute: 0, receipts: 0 });
});

test('workflow recovers editorial admission before approval and does not resubmit either completed stage', async t => {
  const path = await fixture(t); await writeCheckpoint(path, prepared); const client = api(path, { loseResponse: true });
  await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const result = await runWorkflow(client, [], { resumePath: path, autonomous: true });
  assert.equal(result.mode, 'approved'); assert.equal(result.checkpoint.approvalId, 'approval');
  await runWorkflow(client, [], { resumePath: path, autonomous: true });
  assert.deepEqual(client.calls, { editorial: 1, approval: 1, execute: 0, receipts: 1 });
});

test('legacy v53 approval already admitted is not retrospectively editorial reviewed', async t => {
  const path = await fixture(t); await writeCheckpoint(path, { ...prepared, phase: 'approved', approvalId: 'legacy-approval' });
  const client = api(path); const result = await runWorkflow(client, [], { resumePath: path, autonomous: true });
  assert.equal(result.mode, 'approved'); assert.equal(client.calls.editorial, 0); assert.equal(client.calls.approval, 0);
});

for (const corruption of ['missing', 'kind', 'key', 'hash', 'result-key', 'job-kind', 'job-ref', 'scope']) {
  test(`editorial recovery rejects ${corruption} without another POST or approval`, async () => {
    const state = { requestId: 'key', payloadHash: localAdmissionPayloadHash({ proposals: exact }), proposals: exact, phase: 'admitting' };
    const receipt = { kind: 'editorial', requestId: 'key', payloadHash: state.payloadHash, status: 'committed', result: { jobId: 'job', requestId: 'key', replayed: false } };
    if (corruption === 'kind') receipt.kind = 'approval'; if (corruption === 'key') receipt.requestId = 'other';
    if (corruption === 'hash') receipt.payloadHash = '0'.repeat(64); if (corruption === 'result-key') receipt.result.requestId = 'other';
    const client = { editorialReview: () => assert.fail('no second POST'),
      localAdmission: async () => corruption === 'missing' ? { status: 'pending_or_unknown' } : receipt,
      getJob: async () => ({ id: 'job', kind: corruption === 'job-kind' ? 'execute' : 'editorial_review', refId: corruption === 'job-ref' ? 'other' : 'key', status: 'completed', editorialOutcome: accepted() }) };
    const references = corruption === 'scope' ? [{ id: 'p1', revision: 2 }] : exact;
    await assert.rejects(settleEditorialReview(client, references, state, { pollMs: 0 }), error => ['UNKNOWN_MUTATION_OUTCOME', 'INVALID_EDITORIAL_REVIEW'].includes(error.code));
  });
}

test('editorial outcome requires a complete nonoverlapping exact partition and reused subset', () => {
  for (const value of [{ accepted: exact.slice(0, 1), reused: [], held: [] },
    { accepted: exact, reused: [{ id: 'other', revision: 1 }], held: [] },
    { ...held(), accepted: exact }, { ...held(), held: [{ ...held().held[0], reference: { id: 'p2', revision: 3 } }] }])
    assert.throws(() => editorialOutcome(value, exact), { code: 'INVALID_EDITORIAL_REVIEW' });
});

test('client editorial endpoint uses exact keyed payload and actor/account-bound receipt route', async () => {
  const calls = []; const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async (url, options) => {
    calls.push({ path: url.pathname, method: options.method, body: options.body && JSON.parse(options.body) });
    return new Response(JSON.stringify(url.pathname === '/api/session' ? { csrfToken: 'csrf' } : { account: 'LikeAvto' }));
  } });
  await client.editorialReview(exact, 'key'); await client.localAdmission('editorial', 'key');
  assert.deepEqual(calls.find(row => row.method === 'POST').body, { proposals: exact, requestId: 'key' });
  assert.equal(calls.at(-1).path, '/api/local-admissions/editorial/key'); assert.equal(calls.at(-1).method, 'GET');
});

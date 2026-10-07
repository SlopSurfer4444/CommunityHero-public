import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { currentDrafts, runQueue } from '../cli/queue.mjs';
import { campaignHolds } from '../cli/conductor.mjs';
import { UnknownMutationError, localAdmissionPayloadHash } from '../cli/client.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';

const digest = 'a'.repeat(64);
function records(id = 'i') {
  return { item: { id, workflow: 'prepared', providerStatus: 'new', revision: 2, draftEdited: false,
    createdAt: '2026-09-30T00:00:00Z', conversationKey: `c-${id}`, contextEvidenceDigest: digest, branchContextDigest: digest },
    proposal: { id: `p-${id}`, itemId: id, status: 'draft', revision: 1, itemRevision: 2, kind: 'reply_and_close',
      prepareRunId: 'original-prepare', prepareBundleId: 'original-bundle', prepareBundleDigest: digest,
      contextEvidenceDigest: digest, branchContextDigest: digest, reviewContextDigest: digest } };
}
function review(item, proposal, operations = []) { return { items: [item], proposals: [proposal], operations, coverage: { operationsComplete: true } }; }

test('discovery keeps exact original draft reference and rejects manual, changed, foreign or operated recipients', () => {
  const { item, proposal } = records();
  assert.deepEqual(currentDrafts(review(item, proposal), ['i']).candidates, [{ id: 'p-i', revision: 1, itemId: 'i', kind: 'reply_and_close' }]);
  for (const change of [{ revision: 2 }, { origin: 'manual' }, { recovery: {} }, { prepareBundleId: null },
    { itemRevision: 1 }, { branchContextDigest: 'b'.repeat(64) }, { conductorRunId: 'foreign', grantGeneration: 1 }])
    assert.equal(currentDrafts(review(item, { ...proposal, ...change }), ['i'], { runId: 'owned' }).candidates.length, 0);
  assert.equal(currentDrafts(review({ ...item, draftEdited: true }, proposal), ['i']).held[0].reason, 'existing-draft-manual-or-edited');
  assert.equal(currentDrafts(review(item, proposal, [{ id: 'op', target: { id: 'i' }, status: 'failed' }]), ['i']).held[0].reason, 'existing-draft-operation-held');
  assert.equal(currentDrafts(review(item, proposal, [{ id: 'op', conversationKey: 'c-i', status: 'unknown' }]), ['i']).held[0].reason, 'unknown-conversation-held');
  assert.throws(() => currentDrafts({ ...review(item, proposal), coverage: { operationsComplete: false } }, ['i']), { code: 'INCOMPLETE_OPERATION_COVERAGE' });
});

async function fixture(t, { lost = false, held = false, mixed = false, status = 'succeeded' } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-queue-drafts-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'queue.json'); const { item, proposal } = records(); const original = structuredClone(proposal);
  const outside = records('outside'); const items = [item, outside.item]; const proposals = [proposal, outside.proposal];
  const scope = ['i'];
  if (mixed) { const j = records('j'); items.push(j.item); proposals.push(j.proposal); scope.push('j'); }
  const calls = []; const operations = []; const jobs = new Map(); const receipts = new Map(); let lose = lost;
  const snapshot = () => ({ account: 'BAW', items, proposals, operations, jobs: [...jobs.values()], sync: { openCoverage: { done: true, coverageComplete: true } } });
  const client = { account: 'BAW', baseUrl: 'http://draft.fixture.invalid', editorialReviewSupportsFresh: true,
    importMaterials: async () => ({ completed: true, status: 'completed' }),
    bootstrap: async () => snapshot(), sync: async () => ({ jobId: 'sync' }),
    prepareEngine: async () => assert.fail('Adoption must not regenerate'),
    reviewItems: async ids => ({ items: items.filter(row => ids.includes(row.id)), proposals: proposals.filter(row => ids.includes(row.itemId)),
      operations: operations.filter(row => ids.includes(row.itemId)), coverage: { operationsComplete: true } }),
    editorialReview: async (refs, requestId, options) => {
      assert.equal(options.fresh, true); const queue = JSON.parse(await readFile(path, 'utf8'));
      const child = JSON.parse(await readFile(queue.currentSlice.childPath, 'utf8'));
      assert.equal(child.freshEditorial, true); assert.equal(child.slices[0].editorialReview.payloadHash, localAdmissionPayloadHash({ proposals: refs, fresh: true }));
      calls.push('editorial'); const jobId = `editorial-${requestId}`; const result = { jobId, requestId, replayed: false };
      const heldRefs = refs.filter(row => held || mixed && row.id === 'p-j');
      jobs.set(jobId, { id: jobId, kind: 'editorial_review', refId: requestId, status: 'completed',
        editorialOutcome: { accepted: refs.filter(row => !heldRefs.includes(row)), reused: [],
          held: heldRefs.map(reference => ({ reference, decision: 'hold', reason: 'Requires a human' })) } });
      receipts.set(`editorial:${requestId}`, { kind: 'editorial', requestId, status: 'committed', payloadHash: child.slices[0].editorialReview.payloadHash, result });
      if (lose) { lose = false; throw new UnknownMutationError('POST', '/api/editorial-reviews'); } return result;
    },
    mutate: async (route, body) => { assert.equal(route, '/api/approvals'); assert.deepEqual(body.proposals, [{ id: 'p-i', revision: 1 }]); calls.push('approve'); return { id: 'approval', requestId: body.requestId }; },
    execute: async (approvalId, requestId) => { calls.push('execute'); const result = { jobId: 'execute', approvalId, requestId, replayed: false };
      jobs.set('execute', { id: 'execute', kind: 'execute', refId: approvalId, status: 'completed' });
      operations.push({ id: 'op-i', itemId: 'i', proposalId: 'p-i', approvalId, status, providerRetryAllowed: false }); return result; },
    getJob: async id => id === 'sync' ? { id, status: 'completed' } : jobs.get(id),
    localAdmission: async (kind, requestId) => receipts.get(`${kind}:${requestId}`) };
  // Match the Rust parent's pristine seed. It binds the immutable run scope.
  await writeCheckpoint(path, { kind: 'communityhero-queue', account: client.account, baseUrl: client.baseUrl, conductorRunId: 'run',
    scopeItemIds: scope, phase: 'starting', cycle: 0, slices: [], attemptedItemIds: [], materialsReady: true });
  const options = { resumePath: path, scopeItemIds: scope, autonomous: true, execute: true, adoptCurrentDrafts: true,
    freshEditorial: true, continueHeld: true, maxCycles: 3, pollMs: 0 };
  return { path, client, options, calls, proposal, original };
}

test('fresh scope adopts generated current draft through editorial, approval and sole bulk send without regeneration', async t => {
  const f = await fixture(t); const result = await runQueue(f.client, f.options);
  assert.equal(result.mode, 'complete'); assert.deepEqual(f.calls, ['editorial', 'approve', 'execute']);
  assert.equal(result.counts.prepared, 0); assert.equal(result.counts.adoptedDrafts, 1); assert.equal(result.counts.verifiedReplies, 1);
  assert.deepEqual(f.proposal, f.original); assert.deepEqual(result.checkpoint.slices[0].itemIds, ['i']);
  await assert.rejects(runQueue(f.client, { ...f.options, adoptCurrentDrafts: false }), { code: 'INVALID_CHECKPOINT' });
  await assert.rejects(runQueue({ ...f.client, account: 'LikeAvto' }, f.options), { code: 'WRONG_ACCOUNT' });
});

test('lost editorial ACK resumes original receipt and never regenerates or repeats review', async t => {
  const f = await fixture(t, { lost: true }); const first = await runQueue(f.client, f.options);
  assert.equal(first.mode, 'stopped'); assert.deepEqual(f.calls, ['editorial']);
  const resumed = await runQueue(f.client, f.options);
  assert.equal(resumed.counts.verifiedReplies, 1); assert.deepEqual(f.calls, ['editorial', 'approve', 'execute']);
});

test('currentness is checked again at the handoff and no replacement draft or sender is guessed', async t => {
  const f = await fixture(t); const read = f.client.reviewItems; let reads = 0;
  f.client.reviewItems = async ids => { if (++reads === 2) f.proposal.itemRevision = 1; return read(ids); };
  const result = await runQueue(f.client, f.options);
  assert.equal(result.mode, 'stopped'); assert.deepEqual(f.calls, []);
  assert.equal(result.checkpoint.slices[0].error.code, 'EXISTING_DRAFT_CHANGED');
});

test('old non-pristine owned journals retain their original no-adoption policy', async t => {
  const f = await fixture(t); const state = JSON.parse(await readFile(f.path, 'utf8'));
  await writeCheckpoint(f.path, { ...state, phase: 'running', cycle: 1 });
  const result = await runQueue(f.client, f.options);
  assert.equal(result.checkpoint.adoptCurrentDrafts, undefined); assert.deepEqual(f.calls, []);
  assert.equal(result.checkpoint.scopeHolds[0].reason, 'scope-item-not-eligible');
});

test('editorial hold is explicit and UNKNOWN original operation never receives another send', async t => {
  const f = await fixture(t, { held: true }); const held = await runQueue(f.client, f.options);
  assert.deepEqual(f.calls, ['editorial']); assert.equal(held.counts.held, 1);
  assert.equal(campaignHolds(held.checkpoint, ['i'])[0].reason, 'Requires a human');
  const u = await fixture(t, { status: 'unknown' }); const first = await runQueue(u.client, u.options);
  assert.equal(first.mode, 'stopped'); assert.equal(first.counts.unknown, 1);
  await runQueue(u.client, u.options); assert.deepEqual(u.calls, ['editorial', 'approve', 'execute']);
});

test('mixed editorial batch retains accepted-but-unsent recipients explicitly with no partial admission', async t => {
  const f = await fixture(t, { mixed: true }); const result = await runQueue(f.client, f.options);
  assert.deepEqual(f.calls, ['editorial']); assert.equal(result.counts.held, 2);
  const holds = campaignHolds(result.checkpoint, ['i', 'j']);
  assert.deepEqual(holds.map(row => row.itemId).sort(), ['i', 'j']);
  assert.equal(holds.find(row => row.itemId === 'j').reason, 'Requires a human');
  assert.equal(holds.find(row => row.itemId === 'i').reason, 'Draft retained with an editorial-held batch');
});

test('abandoned bulk lock is an explicit hold and never regenerates, repeats admission or removes owner evidence', async t => {
  const f = await fixture(t, { lost: true }); const first = await runQueue(f.client, f.options);
  const childPath = first.checkpoint.slices[0].childPath;
  const lock = JSON.stringify({ pid: 123456789, historicalOwner: true });
  await writeFile(`${childPath}.lock`, lock, { flag: 'wx' });
  const result = await runQueue(f.client, f.options);
  assert.equal(result.mode, 'stopped'); assert.equal(result.checkpoint.slices[0].error.code, 'CHECKPOINT_LOCKED');
  assert.deepEqual(f.calls, ['editorial']); assert.equal(await readFile(`${childPath}.lock`, 'utf8'), lock);
});

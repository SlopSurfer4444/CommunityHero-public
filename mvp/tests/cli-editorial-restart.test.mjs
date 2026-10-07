import test from 'node:test';
import assert from 'node:assert/strict';
import { settleEditorialReview } from '../cli/editorial.mjs';
import { localAdmissionPayloadHash, UnknownMutationError } from '../cli/client.mjs';

function fixture({ status = 'interrupted', loseAck = false, omitReceipt = false, interruptAgain = false } = {}) {
  const ref = { id: 'proposal', revision: 2, itemId: 'recipient', kind: 'reply_and_close', text: 'Saved repaired draft' };
  const refs = [{ id: ref.id, revision: ref.revision }];
  const payloadHash = localAdmissionPayloadHash({ proposals: refs, fresh: true });
  let current = { ...ref, status: 'draft' }; let operations = []; let complete = true;
  const jobs = new Map([['original', { id: 'original', kind: 'editorial_review', refId: 'old-key', status,
    editorialReferences: refs, editorialFresh: true }]]);
  const receipts = new Map([['old-key', { kind: 'editorial', requestId: 'old-key', status: 'committed', payloadHash,
    result: { jobId: 'original', requestId: 'old-key', replayed: false } }]]);
  const calls = []; let saved = { phase: 'reviewing', requestId: 'old-key', jobId: 'original', payloadHash, proposals: refs, fresh: true };
  const client = {
    editorialReviewSupportsFresh: true,
    getJob: async id => jobs.get(id), localAdmission: async (_kind, id) => receipts.get(id),
    reviewItems: async () => ({ proposals: [current], operations, coverage: { operationsComplete: complete } }),
    editorialReview: async (proposals, requestId, options) => {
      calls.push({ proposals, requestId, options, savedAtAdmission: structuredClone(saved) });
      const job = { id: 'replacement', kind: 'editorial_review', refId: requestId, status: interruptAgain ? 'interrupted' : 'completed',
        editorialReferences: refs, editorialFresh: true,
        ...(interruptAgain ? {} : { editorialOutcome: { accepted: refs, reused: [], held: [] } }) };
      jobs.set(job.id, job);
      const result = { jobId: job.id, requestId, replayed: false };
      if (!omitReceipt) receipts.set(requestId, { kind: 'editorial', requestId, status: 'committed', payloadHash, result });
      if (loseAck) throw new UnknownMutationError('POST', '/api/proposals/editorial-review', { code: 'HTTP_500' });
      return result;
    }
  };
  const run = () => settleEditorialReview(client, [ref], saved, { fresh: true, restartInterruptedFresh: true,
    pollMs: 0, maxPolls: 2, save: async value => { saved = structuredClone(value); } });
  return { ref, refs, jobs, calls, receipts, run, current, saved: () => saved,
    operations: value => { operations = value; }, coverage: value => { complete = value; }, client };
}

test('confirmed interrupted fresh review journals new key and reviews saved revision 2 without editing', async () => {
  const f = fixture(); const result = await f.run();
  assert.equal(result.phase, 'complete'); assert.equal(f.calls.length, 1);
  assert.deepEqual(f.calls[0].proposals, f.refs); assert.equal(f.calls[0].options.fresh, true);
  assert.notEqual(f.calls[0].requestId, 'old-key'); assert.equal(f.calls[0].savedAtAdmission.phase, 'admitting');
  assert.equal(f.calls[0].savedAtAdmission.restartHistory[0].jobId, 'original');
  assert.equal(f.current.revision, 2); assert.equal(f.current.text, 'Saved repaired draft');
});

for (const status of ['failed', 'cancelled']) test(`${status} fresh review remains a visible hold with no paid retry`, async () => {
  const f = fixture({ status }); const result = await f.run();
  assert.equal(result.phase, 'held'); assert.deepEqual(result.outcome.accepted, []);
  assert.equal(result.recoveryHold.status, status); assert.equal(f.calls.length, 0);
});

test('completed actual editorial denial remains the original judgment without new request', async () => {
  const f = fixture({ status: 'completed' });
  f.jobs.get('original').editorialOutcome = { accepted: [], reused: [], held: [{ reference: f.refs[0], decision: 'hold', reason: 'Actual factual denial' }] };
  assert.equal((await f.run()).outcome.held[0].reason, 'Actual factual denial'); assert.equal(f.calls.length, 0);
});

test('cancellation fence prevents paid readmission', async () => {
  const f = fixture(); const controller = new AbortController(); controller.abort();
  await assert.rejects(settleEditorialReview(f.client, [f.ref], f.saved(), { fresh: true,
    restartInterruptedFresh: true, signal: controller.signal }), error => error.code === 'STOPPED');
  assert.equal(f.calls.length, 0);
});

test('abort after journaling replacement but before POST preserves original resumable interrupted cursor', async () => {
  const f = fixture(); const controller = new AbortController(); let saved = f.saved();
  await assert.rejects(settleEditorialReview(f.client, [f.ref], saved, { fresh: true,
    restartInterruptedFresh: true, signal: controller.signal, pollMs: 0, maxPolls: 2,
    save: async next => { saved = structuredClone(next); if (next.phase === 'admitting' && next.restartHistory?.length === 1) controller.abort(); }
  }), error => error.code === 'STOPPED');
  assert.equal(f.calls.length, 0); assert.equal(saved.requestId, 'old-key'); assert.equal(saved.jobId, 'original');
  assert.equal(saved.restartHistory, undefined);
  const resumed = await settleEditorialReview(f.client, [f.ref], saved, { fresh: true,
    restartInterruptedFresh: true, pollMs: 0, maxPolls: 2, save: async next => { saved = structuredClone(next); } });
  assert.equal(resumed.phase, 'complete'); assert.equal(f.calls.length, 1); assert.equal(saved.restartHistory.length, 1);
});

for (const omitReceipt of [false, true]) test(`replacement lost ACK stays receipt-only (${omitReceipt ? 'missing' : 'committed'} receipt)`, async () => {
  const f = fixture({ loseAck: true, omitReceipt });
  await assert.rejects(f.run(), error => error.code === 'UNKNOWN_MUTATION_OUTCOME');
  assert.equal(f.saved().phase, 'admitting'); assert.equal(f.calls.length, 1);
  if (omitReceipt) await assert.rejects(f.run(), error => error.code === 'UNKNOWN_MUTATION_OUTCOME');
  else assert.equal((await f.run()).phase, 'complete');
  assert.equal(f.calls.length, 1); assert.equal(f.current.revision, 2);
});

test('one interrupted replacement exhausts policy as explicit hold instead of looping', async () => {
  const f = fixture({ interruptAgain: true }); const result = await f.run();
  assert.equal(result.phase, 'held'); assert.equal(result.restartHistory.length, 1);
  assert.deepEqual(result.outcome.accepted, []); assert.equal(f.calls.length, 1);
  await f.run(); assert.equal(f.calls.length, 1);
});

for (const corrupt of ['revision', 'text', 'unknown-operation', 'coverage', 'receipt'])
  test(`recovery rejects ${corrupt} before new paid work`, async () => {
    const f = fixture();
    if (corrupt === 'revision') f.current.revision++;
    if (corrupt === 'text') f.current.text = 'Different text';
    if (corrupt === 'unknown-operation') f.operations([{ itemId: 'recipient', status: 'unknown' }]);
    if (corrupt === 'coverage') f.coverage(false);
    if (corrupt === 'receipt') f.receipts.delete('old-key');
    await assert.rejects(f.run()); assert.equal(f.calls.length, 0);
  });

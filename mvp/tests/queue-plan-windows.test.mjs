import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runQueue } from '../cli/queue.mjs';

const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-plan-')); t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'q.json'), gates = [deferred(), deferred()], launched = deferred(), planReady=deferred();
  const calls = [], items = ['a', 'b'].map(id => ({ id, workflow: 'attention', providerStatus: 'new',
    conversationKey: `branch:${id}`, createdAt: '2026-10-01T00:00:00Z' }));
  const snapshot = () => ({ account: 'BAW', items, proposals: [], operations: [], jobs: [],
    materials: [{ kind: 'knowledge', imported: true }],
    sync: { openCoverage: { done: true, coverageComplete: true } } });
  const client = { account: 'BAW', baseUrl: 'http://fixture.invalid',
    engineStatus: async () => ({ prepareScopeReservations: { version: 1 }, prepareWorkers: { version: 1, maxWorkers: 2 } }),
    importMaterials: async () => ({ jobId: 'materials' }), getJob: async id => ({ id, status: 'completed' }),
    sync: async () => ({ jobId: 'sync' }), bootstrap: async () => snapshot(),
    selectPrepareFamilies: async () => [['a'], ['b']],
    async planPrepare(ids) { calls.push(ids[0]); if (calls.length === 2) launched.resolve();
      await gates[ids[0] === 'a' ? 0 : 1].promise;
      return { batches: [], held: [{ itemId: ids[0], reason: 'media_wait' }] }; },
    prepareEngine() { assert.fail('all recipients are typed-held; no paid request'); },
    createApproval() { assert.fail('no approval'); }, execute() { assert.fail('no social action'); } };
  return { path, gates, launched, planReady, calls, client, options: { checkpointPath: path, scopeItemIds: ['a', 'b'],
    autonomous: true, execute: true, maxCycles: 1, pollMs: 1, maxPolls: 3,
    onProgress:event=>{if(event.event==='prepare.plan.ready'&&event.window===1)planReady.resolve(event);} } };
}

test('real queue consumes a ready held family while an earlier advisory reader is still pending', { timeout: 5000 }, async t => {
  const f = await fixture(t); const pending = runQueue(f.client, f.options); await f.launched.promise;
  assert.deepEqual(f.calls, ['a', 'b']); f.gates[1].resolve(); await tick();
  // This notification follows the awaited durable checkpoint write. Avoid
  // opening the Windows destination while its next atomic rename is running.
  const observed=await f.planReady.promise;
  assert.deepEqual(observed.itemIds,['b']);assert.equal(observed.window,1);
  f.gates[0].resolve(); const result = await pending;
  assert.deepEqual(result.checkpoint.slices.map(row => row.itemIds), [['b'], ['a']]);
  assert.equal(result.checkpoint.slices[0].id,'cycle-1-window-2-held-1');
  assert.deepEqual(result.checkpoint.slices.map(row => row.planHoldReason), ['media_wait', 'media_wait']);
});

test('real queue rejects failed advisory cohort after joining sibling with no partial paid handoff', { timeout: 5000 }, async t => {
  const f = await fixture(t); const failure = new Error('advisory unavailable'); let rejected = false;
  const pending = runQueue(f.client, f.options).then(() => assert.fail('failed plan cohort accepted'), error => {
    rejected = true; assert.equal(error, failure); });
  await f.launched.promise; f.gates[0].reject(failure); await tick(); assert.equal(rejected, false);
  f.gates[1].resolve(); await pending;
  const saved = JSON.parse(await readFile(f.path, 'utf8'));
  assert.equal(saved.pendingSlices?.length || 0, 0); assert.equal(saved.slices.length, 0);
});

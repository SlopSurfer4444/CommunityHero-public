import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createMediaDependencies } from '../cli/conductor-media.mjs';
import { createCampaignDependencies } from '../cli/conductor.mjs';
import { runQueue, summarizeQueue } from '../cli/queue.mjs';
import { writeCheckpoint, readCheckpoint } from '../cli/workflow.mjs';

// Native shape: prepare_plan.rs:256,268-270,296-297 forwards the exact
// preparation_states reason from media_queue.rs:1919/1932. No kind alias or
// fabricated media_pending translation is present in that Rust path.
const nativePlan = (ids, reason = 'media_wait') => ({ account: 'BAW', byteLimit: 550000,
  batches: [], held: ids.map(itemId => ({ itemId, reason })), selectedItemIds: ids, advisory: true });
const forbiddenEffects = Object.fromEntries(['prepareEngine', 'createApproval', 'execute',
  'editorialReview', 'resolvePublicFacts', 'media', 'importMaterials'].map(method =>
  [method, () => assert.fail(`Observation cannot call ${method}`)]));

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-native-media-wait-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const config = { runId: 'native-run', account: 'BAW', checkpointPath: join(dir, 'queue.json'),
    scopeItemIds: ['native', 'legacy', 'unavailable', 'private', 'unknown', 'prepared', 'occupied'] };
  return { config, calls: [], baseUrl: 'http://native.fixture.invalid' };
}

for (const reason of ['media_wait', 'media_pending', 'media_unavailable'])
  test(`${reason} routes the exact typed lane while untyped/private/outside holds stay excluded`, async () => {
    let calls = 0;
    const media = { observeOnce: async context => {
      calls++; assert.deepEqual(context.heldItemIds, ['native']);
      return { readyItemIds: [], pending: false };
    } };
    assert.deepEqual(await createCampaignDependencies(media, null, { maxPolls: 1 })({
      heldItemIds: ['native', 'private'], heldSlices: [
        { itemIds: ['native'], planHoldReason: reason },
        { itemIds: ['private'], planHoldReason: 'private_fact' },
        { itemIds: ['outside'], planHoldReason: reason },
        { itemIds: ['private'], reason }
      ]
    }), []);
    assert.equal(calls, 1);
  });

test('native media_wait observes an owned running job; canonical plan alone releases readiness', async t => {
  const f = await fixture(t); let ready = false;
  const client = { ...forbiddenEffects, baseUrl: f.baseUrl,
    planPrepare: async ids => { f.calls.push(['plan', ids]); return ready
      ? { ...nativePlan(ids), batches: [{ itemIds: ids, bytes: 10 }], held: [] } : nativePlan(ids); },
    reviewItems: async ids => { f.calls.push(['review', ids]); return { items: ids.map(id => ({ id, postId: 'original-post' })) }; },
    mediaStatus: async postId => { f.calls.push(['status', postId]); ready = true;
      return { jobs: [{ id: 'original-job', status: 'running' }], readiness: { ready: false } }; }
  };
  await writeCheckpoint(`${f.config.checkpointPath}.dependencies.json`, {
    kind: 'communityhero-conductor-dependencies', ...f.config, baseUrl: f.baseUrl,
    dependencies: [{ postId: 'original-post', itemIds: ['native'], phase: 'observing', jobId: 'original-job' }]
  });
  const observer = await createMediaDependencies(client, f.config, { maxPolls: 1, pollMs: 0 });
  assert.deepEqual(await observer.observeOnce({ heldItemIds: ['native'] }), { readyItemIds: ['native'], pending: true });
  assert.deepEqual(f.calls, [['plan', ['native']], ['review', ['native']], ['status', 'original-post'], ['plan', ['native']]]);
  const saved = await readCheckpoint(`${f.config.checkpointPath}.dependencies.json`);
  assert.deepEqual(saved.dependencies, [{ postId: 'original-post', itemIds: ['native'], phase: 'observing', jobId: 'original-job' }]);
});

for (const phase of ['admitting', 'advancing', 'uncertain'])
  test(`native media_wait restart ${phase} observes queued original without enqueue retry or false readiness`, async t => {
    const f = await fixture(t);
    const client = { ...forbiddenEffects, baseUrl: f.baseUrl,
      planPrepare: async ids => nativePlan(ids),
      reviewItems: async ids => ({ items: ids.map(id => ({ id, postId: 'original-post' })) }),
      mediaStatus: async postId => { f.calls.push(['status', postId]);
        return { jobs: [{ id: 'original-job', status: 'queued' }], readiness: { ready: true } }; }
    };
    await writeCheckpoint(`${f.config.checkpointPath}.dependencies.json`, {
      kind: 'communityhero-conductor-dependencies', ...f.config, baseUrl: f.baseUrl,
      dependencies: [{ postId: 'original-post', itemIds: ['native'], phase }]
    });
    const observer = await createMediaDependencies(client, f.config, { maxPolls: 1, pollMs: 0 });
    assert.deepEqual(await observer.observeOnce({ heldItemIds: ['native'] }), { readyItemIds: [], pending: false });
    assert.deepEqual(f.calls, [['status', 'original-post']]);
    const saved = await readCheckpoint(`${f.config.checkpointPath}.dependencies.json`);
    assert.equal(saved.dependencies[0].phase, phase);
    assert.equal(saved.dependencies[0].jobId, 'original-job');
  });

test('native queue callback sees only unattempted-model media holds; UNKNOWN and occupied/paid siblings stay excluded', async t => {
  const f = await fixture(t);
  const items = f.config.scopeItemIds.map(id => ({ id, workflow: 'attention', providerStatus: 'new',
    conversationKey: id, createdAt: '2026-09-30T00:00:00Z' }));
  const operations = [{ id: 'original-unknown', itemId: 'unknown', status: 'unknown', conversationKey: 'unknown' }];
  const proposals = [{ id: 'original-draft', itemId: 'occupied', status: 'draft' }];
  const snapshot = () => ({ account: 'BAW', items, proposals, operations, jobs: [],
    sync: { openCoverage: { done: true, coverageComplete: true } } });
  const client = { ...forbiddenEffects, baseUrl: f.baseUrl, account: 'BAW',
    bootstrap: async () => snapshot(), sync: async () => ({ jobId: 'sync' }),
    getJob: async () => ({ id: 'sync', status: 'completed' }),
    reviewItems: async ids => ({ items: items.filter(row => ids.includes(row.id)), proposals,
      operations, coverage: { operationsComplete: true } }),
    planPrepare: () => assert.fail('No recipient has been released for a new plan')
  };
  const slices = f.config.scopeItemIds.map(itemId => ({ id: `hold-${itemId}`, itemIds: [itemId], status: 'plan-held',
    planHoldReason: itemId === 'legacy' ? 'media_pending' : itemId === 'unavailable' ? 'media_unavailable'
      : itemId === 'private' ? 'private_fact' : 'media_wait',
    ...(itemId === 'prepared' ? { child: { prepareJobId: 'original-paid', proposals: [] } } : {}) }));
  await writeCheckpoint(f.config.checkpointPath, { kind: 'communityhero-queue', account: 'BAW', baseUrl: f.baseUrl,
    phase: 'running', cycle: 1, syncCycles: 0, materialsReady: true,
    scopeItemIds: f.config.scopeItemIds, attemptedItemIds: f.config.scopeItemIds, slices });
  let callbacks = 0;
  const result = await runQueue(client, { resumePath: f.config.checkpointPath, autonomous: true, execute: false,
    scopeItemIds: f.config.scopeItemIds, continueHeld: true, maxCycles: 2, pollMs: 0, maxPolls: 1,
    waitForDependencies: async context => {
      callbacks++; assert.deepEqual(context.heldItemIds, ['native', 'legacy', 'unavailable']);
      assert.deepEqual(context.heldSlices.map(slice => slice.planHoldReason), ['media_wait', 'media_pending', 'media_unavailable']);
      assert.deepEqual(context.factDependencies, []); return [];
    } });
  assert.equal(callbacks, 1);
  assert.equal(result.counts.unknown, 1);
  assert.equal(result.checkpoint.dependencyRequeues, undefined);
  assert.deepEqual(result.checkpoint.slices, slices);
});

test('resolved native hold summary drops its historical hold; unresolved and private holds remain counted', () => {
  const counts = summarizeQueue({ slices: [
    { status: 'plan-held', itemIds: ['resolved'], planHoldReason: 'media_wait', dependencyResolved: true },
    { status: 'plan-held', itemIds: ['legacy-resolved'], planHoldReason: 'media_pending', dependencyResolved: true },
    { status: 'plan-held', itemIds: ['native'], planHoldReason: 'media_wait' },
    { status: 'plan-held', itemIds: ['private'], planHoldReason: 'private_fact', dependencyResolved: true }
  ] });
  assert.equal(counts.held, 2); assert.equal(counts.prepared, 0); assert.equal(counts.verifiedReplies, 0);
});

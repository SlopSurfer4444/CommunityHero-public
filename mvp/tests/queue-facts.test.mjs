import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runQueue } from '../cli/queue.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';
import { campaignHolds } from '../cli/conductor.mjs';
import { CommunityHeroClient } from '../cli/client.mjs';

async function fixture(t, { complete = false, unknown = false, typed = true } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-queue-facts-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'queue.json'); const calls = []; const jobs = new Map();
  const items = ['public', 'sibling', 'private'].map(id => ({ id, workflow: 'attention', providerStatus: 'new', conversationKey: id, createdAt: '2026-09-30T00:00:00Z' }));
  const operations = unknown ? [{ id: 'unknown-original', itemId: 'public', status: 'unknown', conversationKey: 'public' }] : [];
  const snapshot = () => ({ account: 'BAW', items, proposals: [], operations, jobs: [...jobs.values()], sync: { openCoverage: { done: true, coverageComplete: true } } });
  const client = { baseUrl: 'http://fixture.invalid', account: 'BAW', bootstrap: async () => snapshot(),
    engineStatus: async () => ({ strictGrouping: { version: 1, contract: 'strict_post_family_v1' } }),
    requireStrictPreparation: CommunityHeroClient.prototype.requireStrictPreparation,
    sync: async () => ({ jobId: 'sync' }), getJob: async id => id === 'sync' ? { id, status: 'completed' } : jobs.get(id),
    planPrepare: async ids => ({ batches: [{ itemIds: ids, bytes: 100 }], held: [] }),
    reviewItems: async ids => ({ items: items.filter(row => ids.includes(row.id)), proposals: [], operations, coverage: { operationsComplete: true } }),
    prepareEngine: async payload => {
      calls.push(payload.itemIds); const id = `prepare-new-${calls.length}`;
      jobs.set(id, { id, purpose: 'engine_prepare', status: 'completed', prepareOutcome: { factDependencies: [] } });
      // This fixture does not model approval or sending; it establishes new
      // exact preparation admission, source cursor and retained sibling holds.
      items.find(row => row.id === payload.itemIds[0]).workflow = 'closed';
      return { jobId: id, requestId: payload.requestId, replayed: false };
    } };
  const factDependencies = typed ? items.map(row => ({ id: `dep-${row.id}`, itemId: row.id,
    kind: row.id === 'private' ? 'private_company_fact' : 'missing_public_fact', status: row.id === 'private' ? 'held' : 'pending',
    consumedByJobId: null, lastResearchJobId: null })) : [];
  const scopeItemIds = items.map(row => row.id);
  await writeCheckpoint(path, { kind: 'communityhero-queue', baseUrl: client.baseUrl, account: 'BAW',
    phase: complete ? 'complete' : 'running', stopReason: complete ? 'known-complete-no-eligible' : null,
    cycle: 1, syncCycles: 0, materialsReady: true, attemptedItemIds: scopeItemIds, scopeItemIds,
    slices: [{ id: 'original', itemIds: scopeItemIds, status: 'held', error: null,
      child: { phase: 'no-action', prepareJobId: 'prepare-original', proposals: [], prepareOutcome: { factDependencies, reason: 'missing public fact' } } }] });
  const options = { resumePath: path, autonomous: true, execute: false, scopeItemIds, batchSize: 3, maxCycles: 10, pollMs: 0, maxPolls: 1, continueHeld: true };
  return { path, client, calls, options };
}

for (const complete of [false, true]) test(`typed fact wake from ${complete ? 'completed' : 'running'} checkpoint prepares only exact resolved recipient and retains siblings`, async t => {
  const f = await fixture(t, { complete }); let wakes = 0;
  const result = await runQueue(f.client, { ...f.options, waitForDependencies: async context => {
    wakes++; assert.deepEqual(context.factDependencies.map(row => row.itemId), wakes === 1 ? ['public', 'sibling'] : ['sibling']);
    assert.ok(context.factDependencies.every(row => row.prepareJobId === 'prepare-original'));
    return wakes === 1 ? ['public'] : [];
  } });
  assert.deepEqual(f.calls, [['public']]);
  assert.deepEqual(result.checkpoint.slices[0].dependencyResolvedItemIds, ['public']);
  assert.equal(result.checkpoint.slices[0].dependencyResolved, undefined);
  assert.equal(result.counts.held, 3); // two original holds + fixture new no-action
  const holds = campaignHolds(result.checkpoint, f.options.scopeItemIds);
  assert.deepEqual(holds.map(row => row.itemId), ['sibling', 'private']);
  await runQueue(f.client, { ...f.options, waitForDependencies: async context => {
    assert.deepEqual(context.factDependencies.map(row => row.itemId), ['sibling']); return [];
  } });
  assert.deepEqual(f.calls, [['public']]);
});

for (const boundary of ['before', 'after']) test(`restart ${boundary} atomic exact fact wake cannot replay original preparation or whole slice`, async t => {
  const f = await fixture(t); let fail = true;
  const waitForDependencies = async ({ factDependencies }) => {
    if (!factDependencies.some(row => row.itemId === 'public')) return [];
    if (boundary === 'before' && fail) { fail = false; throw new Error('fixture crash'); }
    return ['public'];
  };
  const onProgress = event => { if (boundary === 'after' && event.event === 'dependency.requeued' && fail) { fail = false; throw new Error('fixture crash'); } };
  await assert.rejects(runQueue(f.client, { ...f.options, waitForDependencies, onProgress }), /fixture crash/);
  const saved = JSON.parse(await readFile(f.path, 'utf8'));
  assert.equal(saved.slices[0].dependencyResolvedItemIds?.includes('public') === true, boundary === 'after');
  const result = await runQueue(f.client, { ...f.options, waitForDependencies });
  assert.deepEqual(f.calls, [['public']]);
  assert.deepEqual(result.checkpoint.dependencyRequeues[0].itemIds, ['public']);
});

test('UNKNOWN blocks only its recipient; reason-only and private holds never enter research callback', async t => {
  const f = await fixture(t, { unknown: true });
  const result = await runQueue(f.client, { ...f.options, waitForDependencies: async ({ heldItemIds, factDependencies }) => {
    assert.deepEqual(heldItemIds, ['sibling']); assert.deepEqual(factDependencies.map(row => row.itemId), ['sibling']); return [];
  } });
  assert.equal(result.counts.unknown, 1); assert.deepEqual(f.calls, []);
  const legacy = await fixture(t, { typed: false });
  await runQueue(legacy.client, { ...legacy.options, waitForDependencies: () => assert.fail('Reasons are not typed dependencies') });
});

test('typed fact callback cannot wake a held private sibling or expand scope', async t => {
  for (const id of ['private', 'outside']) {
    const f = await fixture(t);
    await assert.rejects(runQueue(f.client, { ...f.options, waitForDependencies: async () => [id] }), { code: 'INVALID_DEPENDENCY_SCOPE' });
    assert.deepEqual(f.calls, []);
  }
});

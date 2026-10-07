import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createFactDependencies } from '../cli/conductor-facts.mjs';
import { CliError } from '../cli/client.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-conductor-facts-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const config = { runId: 'campaign', leaseGeneration: 2, account: 'BAW', scopeItemIds: ['a', 'b', 'private'], checkpointPath: join(dir, 'queue.json') };
  const entries = ['a', 'b', 'private'].map(itemId => ({ id: `dep-${itemId}`, itemId,
    kind: itemId === 'private' ? 'private_company_fact' : 'missing_public_fact', status: itemId === 'private' ? 'held' : 'pending',
    lastResearchJobId: null, consumedByJobId: null }));
  const parent = { id: 'prepare-original', purpose: 'engine_prepare', conductorRunId: 'campaign', grantGeneration: 1, factDependencies: entries };
  const calls = []; const jobs = new Map([[parent.id, parent]]);
  const client = { baseUrl: 'http://fixture.invalid', getJob: async id => { calls.push(['get', id]); return jobs.get(id); },
    resolvePublicFacts: async (prepareJobId, itemIds) => {
      calls.push(['resolve', prepareJobId, itemIds]);
      const readyItemIds = []; const jobIds = []; const held = [];
      for (const itemId of itemIds) {
        const row = entries.find(row => row.itemId === itemId);
        if (row.status === 'resolved' && !row.consumedByJobId) { readyItemIds.push(itemId); continue; }
        if (row.status === 'held') { held.push({ itemId, reason: 'held' }); continue; }
        const id = `research-${itemId}-${row.lastResearchJobId ? 2 : 1}`;
        row.lastResearchJobId = id; row.status = 'researching'; jobIds.push(id);
        jobs.set(id, { id, purpose: 'public_fact_followup', parentPrepareJobId: parent.id, conductorRunId: 'campaign',
          grantGeneration: 2, requestedItemIds: [itemId], factDependencyIds: [row.id], status: 'running' });
      }
      return { jobIds, readyItemIds, held };
    } };
  const context = ids => ({ factDependencies: entries.filter(row => ids.includes(row.itemId)).map(row => ({ ...row, prepareJobId: parent.id })) });
  const options = { pollMs: 0, maxPolls: 2 };
  const read = async () => JSON.parse(await readFile(`${config.checkpointPath}.facts.json`, 'utf8'));
  const seed = async phase => writeCheckpoint(`${config.checkpointPath}.facts.json`, {
    kind: 'communityhero-conductor-facts', runId: config.runId, account: config.account, baseUrl: client.baseUrl, scopeItemIds: config.scopeItemIds,
    dependencies: [{ prepareJobId: parent.id, id: 'dep-a', itemId: 'a', kind: 'missing_public_fact', phase }] });
  return { config, client, calls, entries, parent, jobs, context, options, read, seed };
}

test('factory and private/owner/media holds admit no research', async t => {
  const f = await fixture(t); const observe = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(f.calls, []);
  for (const kind of ['private_company_fact', 'owner_decision', 'missing_media']) {
    const context = f.context(['private']); context.factDependencies[0].kind = kind;
    assert.deepEqual(await observe(context), []);
  }
  assert.deepEqual(f.calls, []);
});

test('fair single sweep returns resolved B while observing active A, without further admissions', async t => {
  const f = await fixture(t); const observe = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observe.observeOnce(f.context(['a', 'b'])), { readyItemIds: [], pending: true });
  f.entries[1].status = 'resolved'; f.jobs.get(f.entries[1].lastResearchJobId).status = 'completed';
  f.calls.length = 0;
  assert.deepEqual(await observe.observeOnce(f.context(['a', 'b'])), { readyItemIds: ['b'], pending: true });
  assert.deepEqual(f.calls.filter(row => row[0] === 'resolve').map(row => row[2]), [['b']]);
  assert.ok(f.calls.some(row => row[1] === 'research-a-1'));
});

for (const phase of ['admitting', 'uncertain', 'observing'])
  test(`restart ${phase} recovers original research ACK and never replays active request`, async t => {
    const f = await fixture(t); await f.client.resolvePublicFacts(f.parent.id, ['a']); await f.seed(phase);
    const observe = await createFactDependencies(f.client, f.config, f.options); f.calls.length = 0;
    assert.deepEqual(await observe.observeOnce(f.context(['a'])), { readyItemIds: [], pending: true });
    assert.equal(f.calls.some(row => row[0] === 'resolve'), false);
    assert.equal((await f.read()).dependencies[0].lastResearchJobId, 'research-a-1');
  });

test('missing ACK with no original canonical identity stays inspection-only after restart', async t => {
  const f = await fixture(t); let posts = 0;
  f.client.resolvePublicFacts = async () => { posts++; throw new CliError('Lost ACK', { code: 'UNKNOWN_MUTATION_OUTCOME' }); };
  const first = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(await first.observeOnce(f.context(['a'])), { readyItemIds: [], pending: true });
  const second = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(await second.observeOnce(f.context(['a'])), { readyItemIds: [], pending: false });
  assert.equal(posts, 1); assert.equal((await f.read()).dependencies[0].phase, 'uncertain');
});

test('lost ACK with canonical original can resolve after restart without a second research identity', async t => {
  const f = await fixture(t); const resolve = f.client.resolvePublicFacts; let posts = 0;
  f.client.resolvePublicFacts = async (...args) => { posts++; const result = await resolve(...args);
    if (posts === 1) throw new CliError('Lost ACK', { code: 'UNKNOWN_MUTATION_OUTCOME' }); return result; };
  const first = await createFactDependencies(f.client, f.config, f.options); await first.observeOnce(f.context(['a']));
  f.entries[0].status = 'resolved'; f.jobs.get('research-a-1').status = 'completed';
  const second = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(await second(f.context(['a'])), ['a']);
  assert.equal(f.entries[0].lastResearchJobId, 'research-a-1');
  assert.equal(f.jobs.size, 2);
  f.entries[0].consumedByJobId = 'prepare-new';
  assert.deepEqual(await second(f.context(['a'])), []);
});

test('only observed interruption permits one next canonical attempt; failed lookup remains held', async t => {
  const f = await fixture(t); await f.client.resolvePublicFacts(f.parent.id, ['a']);
  f.jobs.get('research-a-1').status = 'interrupted'; await f.seed('uncertain');
  const observe = await createFactDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observe.observeOnce(f.context(['a'])), { readyItemIds: [], pending: true });
  assert.equal(f.entries[0].lastResearchJobId, 'research-a-2');
  f.jobs.get('research-a-2').status = 'failed'; f.calls.length = 0;
  assert.deepEqual(await observe(f.context(['a'])), []);
  assert.equal(f.calls.some(row => row[0] === 'resolve'), false);
});

test('source/grant/parent and recipient retargeting fail before research admission', async t => {
  for (const mutate of [f => { f.parent.conductorRunId = 'another'; }, f => { f.parent.grantGeneration = 3; },
    f => { f.parent.factDependencies[0].itemId = 'b'; }, f => { f.parent.purpose = 'other'; }]) {
    const f = await fixture(t); const context = f.context(['a']); mutate(f);
    const observe = await createFactDependencies(f.client, f.config, f.options);
    await assert.rejects(observe(context), { code: 'INVALID_CHECKPOINT' });
    assert.equal(f.calls.some(row => row[0] === 'resolve'), false);
  }
});

test('checkpoint company/scope binding and hostile resolver widening cannot requeue', async t => {
  const f = await fixture(t); await f.seed('new');
  await assert.rejects(createFactDependencies(f.client, { ...f.config, account: 'LikeAvto' }, f.options), { code: 'INVALID_CHECKPOINT' });
  const observe = await createFactDependencies(f.client, f.config, f.options);
  await assert.rejects(observe({ factDependencies: [{ prepareJobId: f.parent.id, id: 'dep-x', itemId: 'outside', kind: 'missing_public_fact' }] }), { code: 'INVALID_DEPENDENCY_SCOPE' });
  f.client.resolvePublicFacts = async () => ({ jobIds: [], readyItemIds: ['b'], held: [] });
  await assert.rejects(observe(f.context(['a'])), { code: 'INVALID_DEPENDENCY_SCOPE' });
});

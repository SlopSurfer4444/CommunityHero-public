import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createMediaDependencies } from '../cli/conductor-media.mjs';
import { CliError } from '../cli/client.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-conductor-media-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const config = { runId: 'run', account: 'BAW', checkpointPath: join(dir, 'queue.json'), scopeItemIds: ['a', 'b', 'safe'] };
  const calls = []; const ready = new Set(); const jobs = new Map();
  const client = {
    baseUrl: 'http://127.0.0.1:4185',
    planPrepare: async ids => {
      calls.push(['plan', ids]);
      return { batches: ids.filter(id => ready.has(id)).map(id => ({ itemIds: [id], bytes: 10 })),
        held: ids.filter(id => !ready.has(id)).map(itemId => ({ itemId, reason: 'media_pending' })) };
    },
    reviewItems: async ids => { calls.push(['review', ids]); return { items: ids.map(id => ({ id, postId: `post-${id}` })) }; },
    media: async postId => {
      calls.push(['media', postId]);
      jobs.set(postId, { id: `job-${postId}`, status: 'running' });
      return { jobId: `job-${postId}`, status: 'running' };
    },
    mediaStatus: async postId => { calls.push(['status', postId]); return { jobs: jobs.has(postId) ? [jobs.get(postId)] : [] }; }
  };
  const options = { pollMs: 0, maxPolls: 2 };
  const read = async () => JSON.parse(await readFile(`${config.checkpointPath}.dependencies.json`, 'utf8'));
  const seed = async dependencies => writeCheckpoint(`${config.checkpointPath}.dependencies.json`, {
    kind: 'communityhero-conductor-dependencies', runId: config.runId, account: config.account,
    baseUrl: client.baseUrl, scopeItemIds: config.scopeItemIds, dependencies
  });
  return { config, calls, ready, jobs, client, options, read, seed };
}

test('factory performs no manifest plan, review or media admission before ready queue work can start', async t => {
  const f = await fixture(t);
  for (const method of ['planPrepare', 'reviewItems', 'media', 'mediaStatus'])
    f.client[method] = () => assert.fail('Factory must not call the engine before a typed queue hold exists');
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.equal(typeof observer, 'function'); assert.deepEqual(f.calls, []);
});

test('running A cannot delay canonically ready B until A finishes or reaches its poll bound', async t => {
  const f = await fixture(t);
  const status = f.client.mediaStatus;
  f.client.mediaStatus = async postId => {
    const result = await status(postId);
    if (postId === 'post-b') { f.ready.add('b'); return { jobs: [{ id: 'job-post-b', status: 'completed' }], readiness: { ready: true } }; }
    return result;
  };
  const observer = await createMediaDependencies(f.client, f.config, { ...f.options, maxPolls: 1 });
  assert.deepEqual(await observer({ heldItemIds: ['a', 'b'] }), ['b']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'status').map(row => row[1]), ['post-a', 'post-b']);
  assert.deepEqual((await f.read()).dependencies.map(row => row.phase), ['observing', 'observing']);
  assert.equal(f.ready.has('a'), false);
});

test('an already ready subset returns before admitting another held media post', async t => {
  const f = await fixture(t); f.ready.add('b');
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['a', 'b'] }), ['b']);
  assert.deepEqual(f.calls.map(row => row[0]), ['plan']);
});

test('each pending dependency receives one read per bounded sweep, without repeated admission', async t => {
  const f = await fixture(t);
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  await assert.rejects(observer({ heldItemIds: ['a', 'b'] }), { code: 'POLL_LIMIT' });
  assert.deepEqual(f.calls.filter(row => row[0] === 'status').map(row => row[1]), ['post-a', 'post-b', 'post-a', 'post-b']);
  assert.equal(f.calls.filter(row => row[0] === 'media').length, 2);
});

test('one-sweep observation reports pending work without sleeping or exhausting another lane', async t => {
  const f = await fixture(t);
  const observer = await createMediaDependencies(f.client, f.config, { pollMs: 60000, maxPolls: 100 });
  assert.deepEqual(await observer.observeOnce({ heldItemIds: ['a', 'b'] }), { readyItemIds: [], pending: true });
  assert.equal(f.calls.filter(row => row[0] === 'status').length, 2);
  assert.equal(f.calls.filter(row => row[0] === 'media').length, 2);
  await observer.observeOnce({ heldItemIds: ['a', 'b'] });
  assert.equal(f.calls.filter(row => row[0] === 'media').length, 2);
});

test('terminal jobs without canonical readiness return no recipients and do not infer readiness', async t => {
  const f = await fixture(t);
  f.client.mediaStatus = async () => ({ jobs: [{ id: 'job-post-a', status: 'completed' }], readiness: { ready: true } });
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['a'] }), []);
});

for (const phase of ['observing', 'admitting', 'advancing', 'uncertain'])
  test(`restart ${phase} observes the existing canonical job without a new media admission`, async t => {
    const f = await fixture(t);
    await f.seed([{ postId: 'post-a', itemIds: ['a'], phase, ...(phase === 'observing' ? { jobId: 'job-post-a' } : {}) }]);
    f.client.media = () => assert.fail('Restart must inspect the original admission');
    let reads = 0;
    f.client.mediaStatus = async () => {
      reads++; if (reads === 2) f.ready.add('a');
      return { jobs: [{ id: 'job-post-a', status: reads === 2 ? 'completed' : 'running' }] };
    };
    const observer = await createMediaDependencies(f.client, f.config, f.options);
    assert.deepEqual(await observer({ heldItemIds: ['a'] }), ['a']);
    const saved = await f.read(); assert.equal(saved.dependencies[0].jobId, 'job-post-a');
    assert.equal(saved.dependencies[0].phase, phase);
  });

test('lost initial media acknowledgement remains inspection-only on restart', async t => {
  const f = await fixture(t); let posts = 0;
  f.client.media = async () => { posts++; throw new CliError('Lost ACK', { code: 'UNKNOWN_MUTATION_OUTCOME' }); };
  f.client.mediaStatus = async () => ({ jobs: [{ id: 'original', status: 'queued' }] });
  const first = await createMediaDependencies(f.client, f.config, { ...f.options, maxPolls: 1 });
  await assert.rejects(first({ heldItemIds: ['a'] }), { code: 'POLL_LIMIT' });
  assert.equal((await f.read()).dependencies[0].phase, 'uncertain');
  const second = await createMediaDependencies(f.client, f.config, { ...f.options, maxPolls: 1 });
  await assert.rejects(second({ heldItemIds: ['a'] }), { code: 'POLL_LIMIT' });
  assert.equal(posts, 1);
});

test('lost queued continuation acknowledgement cannot repeat its POST after restart', async t => {
  const f = await fixture(t); let posts = 0;
  await f.seed([{ postId: 'post-a', itemIds: ['a'], phase: 'observing', jobId: 'original' }]);
  f.client.media = async () => { posts++; throw new CliError('Lost continuation ACK', { code: 'UNKNOWN_MUTATION_OUTCOME' }); };
  f.client.mediaStatus = async () => ({ jobs: [{ id: 'original', status: 'queued' }] });
  const first = await createMediaDependencies(f.client, f.config, f.options);
  await assert.rejects(first({ heldItemIds: ['a'] }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal((await f.read()).dependencies[0].phase, 'uncertain');
  const second = await createMediaDependencies(f.client, f.config, { ...f.options, maxPolls: 1 });
  await assert.rejects(second({ heldItemIds: ['a'] }), { code: 'POLL_LIMIT' });
  assert.equal(posts, 1);
});

test('queued continuation happens only after the complete read sweep and canonical readiness check', async t => {
  const f = await fixture(t);
  await f.seed([{ postId: 'post-a', itemIds: ['a'], phase: 'observing', jobId: 'original-a' },
    { postId: 'post-b', itemIds: ['b'], phase: 'observing', jobId: 'original-b' }]);
  f.client.mediaStatus = async postId => {
    f.calls.push(['status', postId]);
    if (postId === 'post-b') { f.ready.add('b'); return { jobs: [{ id: 'original-b', status: 'completed' }] }; }
    return { jobs: [{ id: 'original-a', status: 'queued' }] };
  };
  f.client.media = () => assert.fail('Ready B must return before any queued continuation');
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['a', 'b'] }), ['b']);
});

test('acknowledged queued continuation retains its original job and is observed to readiness', async t => {
  const f = await fixture(t); let reads = 0; let posts = 0;
  await f.seed([{ postId: 'post-a', itemIds: ['a'], phase: 'observing', jobId: 'original' }]);
  f.client.mediaStatus = async () => {
    reads++; if (reads === 2) f.ready.add('a');
    return { jobs: [{ id: 'original', status: reads === 1 ? 'queued' : 'completed' }] };
  };
  f.client.media = async () => {
    posts++; const saved = await f.read(); assert.equal(saved.dependencies[0].phase, 'advancing');
    return { jobId: 'original', status: 'running' };
  };
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['a'] }), ['a']);
  assert.equal(posts, 1); assert.equal(reads, 2);
  const saved = await f.read(); assert.equal(saved.dependencies[0].jobId, 'original');
  assert.equal(saved.dependencies[0].phase, 'observing');
});

test('legacy unstarted dependency outside the current held subset is never admitted', async t => {
  const f = await fixture(t);
  await f.seed([{ postId: 'post-a', itemIds: ['a'], phase: 'new' }, { postId: 'post-b', itemIds: ['b'], phase: 'new' }]);
  f.client.mediaStatus = async () => ({ jobs: [{ id: 'job-post-b', status: 'failed' }] });
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['b'] }), []);
  assert.deepEqual(f.calls.filter(row => row[0] === 'media').map(row => row[1]), ['post-b']);
  assert.equal((await f.read()).dependencies[0].phase, 'new');
});

test('out-of-scope holds, changed checkpoint binding and retargeted dependency reject before media POST', async t => {
  const f = await fixture(t);
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  await assert.rejects(observer({ heldItemIds: ['outside'] }), { code: 'INVALID_DEPENDENCY_SCOPE' });
  assert.deepEqual(f.calls, []);
  await f.seed([{ postId: 'old', itemIds: ['a'], phase: 'observing', jobId: 'original' }]);
  await assert.rejects(createMediaDependencies(f.client, { ...f.config, account: 'LikeAvto' }, f.options), { code: 'INVALID_CHECKPOINT' });
  const restarted = await createMediaDependencies(f.client, f.config, f.options);
  await assert.rejects(restarted({ heldItemIds: ['a'] }), { code: 'INVALID_CHECKPOINT' });
  assert.equal(f.calls.some(row => row[0] === 'media'), false);
});

test('non-media canonical hold does not become a media request', async t => {
  const f = await fixture(t);
  f.client.planPrepare = async () => ({ batches: [], held: [{ itemId: 'a', reason: 'private_fact' }] });
  f.client.reviewItems = () => assert.fail('Private facts do not discover media');
  f.client.media = () => assert.fail('Private facts do not admit media');
  const observer = await createMediaDependencies(f.client, f.config, f.options);
  assert.deepEqual(await observer({ heldItemIds: ['a'] }), []);
});

test('abort while polling finishes the observer and leaves no subsequent admissions', async t => {
  const f = await fixture(t); const controller = new AbortController();
  f.client.mediaStatus = async () => { controller.abort(); return { jobs: [{ id: 'job-post-a', status: 'running' }] }; };
  const observer = await createMediaDependencies(f.client, f.config, { ...f.options, signal: controller.signal });
  await assert.rejects(observer({ heldItemIds: ['a'] }), { code: 'STOPPED' });
  assert.equal(f.calls.filter(row => row[0] === 'media').length, 1);
  await assert.rejects(observer({ heldItemIds: ['a'] }), { code: 'STOPPED' });
  assert.equal(f.calls.filter(row => row[0] === 'media').length, 1);
});

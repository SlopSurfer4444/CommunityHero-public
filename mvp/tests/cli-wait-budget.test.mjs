import test from 'node:test';
import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { CommunityHeroClient, waitForJob, waitForPoll } from '../cli/client.mjs';

const json = value => ({ ok: true, status: 200, text: async () => JSON.stringify(value) });

test('only exact loopback admission POST routes receive the longer default, and explicit overrides win', async t => {
  const deadlines = [];
  t.mock.method(AbortSignal, 'timeout', ms => { deadlines.push(ms); return new AbortController().signal; });
  const client = new CommunityHeroClient({ account: 'likeavto', fetchImpl: async () => json({}) });
  const admissions = ['/api/engine/prepare', '/api/approvals', '/api/proposals/editorial-review',
    '/api/approvals/a1/execute', '/api/engine/items/i1/context-refresh'];
  for (const path of admissions) {
    await client.request(path, { method: 'POST', csrf: false });
    assert.equal(deadlines.at(-1), 300_000, path);
    await client.request(path);
    assert.equal(deadlines.at(-1), 120_000, `GET ${path}`);
  }
  for (const path of ['/api/proposals', '/api/engine/prepare/plan', '/api/approvals/a1/execute/extra',
    '/api/sync', 'https://example.invalid/api/approvals']) {
    await client.request(path, { method: 'POST', csrf: false });
    assert.equal(deadlines.at(-1), 120_000, path);
  }
  for (const baseUrl of ['http://localhost:4186', 'http://[::1]:4186', 'http://127.0.0.2:4186',
    'https://communityhero.ru', 'http://localhost.example.invalid', 'http://192.168.1.2:4186']) {
    const local = new CommunityHeroClient({ account: 'likeavto', baseUrl, fetchImpl: async () => json({}) });
    await local.request('/api/approvals', { method: 'POST', csrf: false });
    assert.equal(deadlines.at(-1), /localhost:|\[::1\]|127\.0\.0\.2/.test(baseUrl) ? 300_000 : 120_000, baseUrl);
  }
  const override = new CommunityHeroClient({ account: 'likeavto', timeoutMs: 4321, fetchImpl: async () => json({}) });
  for (const method of ['GET', 'POST']) {
    await override.request('/api/approvals', { method, csrf: false });
    assert.equal(deadlines.at(-1), 4321);
  }
  await override.request('/api/approvals', { method: 'POST', csrf: false, timeoutMs: 1234 });
  assert.equal(deadlines.at(-1), 1234);
});

test('invalid request deadlines fail before CSRF or mutation network dispatch, never as UNKNOWN', async () => {
  let calls = 0;
  const options = { account: 'likeavto', fetchImpl: async () => { calls++; return json({}); } };
  const client = new CommunityHeroClient(options);
  for (const timeoutMs of [0, -1, 1.5, NaN, Infinity, 3_600_001, Number.MAX_SAFE_INTEGER, null]) {
    assert.throws(() => new CommunityHeroClient({ ...options, timeoutMs }), { code: 'USAGE' });
    await assert.rejects(client.request('/api/approvals', { method: 'POST', timeoutMs }), { code: 'USAGE' });
  }
  assert.equal(calls, 0);
});

test('default observation follows the same durable job beyond 120 polls without leaked abort listeners', async () => {
  const controller = new AbortController(); let calls = 0;
  const result = await waitForJob({ getJob: async (id, { signal }) => {
    assert.equal(id, 'job'); assert.equal(signal, controller.signal);
    assert.equal(getEventListeners(signal, 'abort').length, 0);
    return { id, status: ++calls > 125 ? 'completed' : 'running' };
  } }, 'job', { pollMs: 0, signal: controller.signal, includeSnapshot: false });
  assert.equal(result.job.status, 'completed'); assert.equal(calls, 126);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('explicit observation limit retains job identity and never sends a POST', async () => {
  const requests = [];
  const client = new CommunityHeroClient({ account: 'likeavto', fetchImpl: async (url, init) => {
    requests.push([url.pathname, init.method]); return json({ id: 'job', status: 'running' });
  } });
  await assert.rejects(waitForJob(client, 'job', { maxPolls: 2, pollMs: 0 }),
    error => error.code === 'POLL_LIMIT' && error.details.jobId === 'job');
  assert.deepEqual(requests, [['/api/engine/jobs/job', 'GET'], ['/api/engine/jobs/job', 'GET']]);
});

for (const phase of ['headers', 'body', 'fallback']) {
  test(`Ctrl-C interrupts the active polling ${phase} read without cancelling or replaying the job`, async () => {
    const controller = new AbortController(); const requests = [];
    let started;
    const ready = new Promise(resolve => { started = resolve; });
    const hanging = signal => new Promise((resolve, reject) => {
      signal.addEventListener('abort', () => reject(signal.reason), { once: true }); started();
    });
    const client = new CommunityHeroClient({ account: 'likeavto', fetchImpl: async (url, init) => {
      requests.push([url.pathname, init.method]);
      if (phase === 'fallback' && url.pathname.startsWith('/api/engine/jobs/'))
        return { ok: false, status: 404, text: async () => '{}' };
      if (phase === 'body') return { ok: true, status: 200, text: () => hanging(init.signal) };
      return hanging(init.signal);
    } });
    const pending = waitForJob(client, 'job', { signal: controller.signal });
    const assertion = assert.rejects(pending, { code: 'STOPPED' });
    await ready; controller.abort(); await assertion;
    assert.ok(requests.every(([, method]) => method === 'GET'));
    assert.equal(requests.length, phase === 'fallback' ? 2 : 1);
  });
}

test('constructor cancellation applies to reads and sleep cancellation removes its listener', async () => {
  const controller = new AbortController(); controller.abort(); let calls = 0;
  const client = new CommunityHeroClient({ account: 'likeavto', signal: controller.signal,
    fetchImpl: async () => { calls++; return json({}); } });
  await assert.rejects(client.getJob('job'), { code: 'STOPPED' });
  assert.equal(calls, 0);
  const sleepController = new AbortController();
  const pending = assert.rejects(waitForPoll(60_000, sleepController.signal), { code: 'STOPPED' });
  sleepController.abort(); await pending;
  assert.equal(getEventListeners(sleepController.signal, 'abort').length, 0);
});

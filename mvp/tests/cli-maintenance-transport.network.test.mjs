// ROOT-executed synthetic loopback tests. Default aggregate skips every listener.
// No application/native/provider endpoint is used; each server binds a fresh port.
import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { once } from 'node:events';
import { CommunityHeroClient } from '../cli/client.mjs';
import { createMaintenanceFetch } from '../cli/maintenance-transport.mjs';

const enabled = process.env.COMMUNITYHERO_RUN_LOOPBACK_TRANSPORT_TESTS === '1';
const longEnabled = enabled && process.env.COMMUNITYHERO_RUN_LONG_TRANSPORT_TEST === '1';
async function fixture(t, handler) {
  const calls = [], timers = new Set();
  const later = (fn, ms) => { const timer = setTimeout(() => { timers.delete(timer); fn(); }, ms); timers.add(timer); };
  const server = http.createServer((request, response) => {
    calls.push({ method: request.method, path: request.url, csrf: request.headers['x-csrf-token'] });
    request.resume(); handler(request, response, later);
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  t.after(async () => { for (const timer of timers) clearTimeout(timer); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  const client = timeoutMs => new CommunityHeroClient({ account: 'LikeAvto', baseUrl, timeoutMs, fetchImpl: createMaintenanceFetch(baseUrl) });
  return { calls, client };
}
test('ROOT loopback: existing session/CSRF flow and delayed maintenance headers', { skip: !enabled }, async t => {
  const f = await fixture(t, (request, response, later) => {
    if (request.url === '/api/session') return response.end(JSON.stringify({ csrfToken: 'synthetic' }));
    later(() => response.end(JSON.stringify({ synthetic: true })), 40);
  });
  const result = await f.client(2000).request('/api/maintenance/runtime/status', { method: 'POST', body: { owner: {} }, mutation: false, csrf: true });
  assert.deepEqual(result, { synthetic: true }); assert.equal(f.calls.length, 2); assert.equal(f.calls[1].csrf, 'synthetic');
});
test('ROOT loopback: deadline before headers is UNKNOWN with one physical POST', { skip: !enabled }, async t => {
  const f = await fixture(t, (_request, response, later) => later(() => response.end('{}'), 1000));
  const client = f.client(100); client.session = { csrfToken: 'synthetic' };
  await assert.rejects(client.request('/api/maintenance/runtime/begin', { method: 'POST', body: { owner: {} } }),
    error => error.code === 'UNKNOWN_MUTATION_OUTCOME' && error.details.phase === 'request-headers' && error.details.timeoutMs === 100);
  assert.equal(f.calls.length, 1);
});
test('ROOT loopback: deadline while body streams remains response-body UNKNOWN', { skip: !enabled }, async t => {
  const f = await fixture(t, (_request, response, later) => { response.writeHead(200, { 'content-type': 'application/json' }); response.write('{'); later(() => response.end('}'), 1000); });
  const client = f.client(100); client.session = { csrfToken: 'synthetic' };
  await assert.rejects(client.request('/api/maintenance/runtime/checkpoint', { method: 'POST', body: { owner: {} } }),
    error => error.code === 'UNKNOWN_MUTATION_OUTCOME' && error.details.phase === 'response-body');
  assert.equal(f.calls.length, 1);
});
test('ROOT loopback: redirect is never followed', { skip: !enabled }, async t => {
  const f = await fixture(t, (_request, response) => { response.writeHead(302, { location: '/api/session' }); response.end('{}'); });
  const client = f.client(1000); client.session = { csrfToken: 'synthetic' };
  await assert.rejects(client.request('/api/maintenance/runtime/begin', { method: 'POST', body: {} }), error => error.status === 302);
  assert.equal(f.calls.length, 1);
});
test('ROOT opt-in: a real header wait beyond 300 seconds succeeds under 600-second authority', { skip: !longEnabled }, async t => {
  const f = await fixture(t, (_request, response, later) => later(() => response.end('{"observed":true}'), 305000));
  const client = f.client(600000); client.session = { csrfToken: 'synthetic' };
  const started = Date.now();
  assert.deepEqual(await client.request('/api/maintenance/runtime/status', { method: 'POST', body: { owner: {} }, mutation: false, csrf: true }), { observed: true });
  assert.ok(Date.now() - started >= 300000); assert.equal(f.calls.length, 1);
});

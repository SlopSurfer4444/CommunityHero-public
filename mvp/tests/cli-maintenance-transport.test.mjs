// Pure transport seam tests. No network listener or request is created.
import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createMaintenanceFetch } from '../cli/maintenance-transport.mjs';
import { CommunityHeroClient, UnknownMutationError, checkpointError } from '../cli/client.mjs';
import { maintenanceError, maintenanceClient } from '../cli/maintenance.mjs';

const origin = 'http://127.0.0.1:4186';
const route = origin + '/api/maintenance/runtime/begin';
function fakeWire() {
  const calls = [], request = new EventEmitter(), response = new EventEmitter();
  request.destroyed = false; response.destroyed = false; response.complete = false;
  request.end = body => { calls[0].body = body; };
  for (const stream of [request, response]) stream.destroy = error => {
    if (stream.destroyed) return; stream.destroyed = true;
    if (error) stream.emit('error', error); stream.emit('close');
  };
  let callback;
  const requestImpl = (url, options, onResponse) => { calls.push({ url, options }); callback = onResponse; return request; };
  return { calls, request, response, requestImpl,
    headers: (status = 200, headers = {}) => { response.statusCode = status; response.headers = headers; callback(response); },
    body: value => response.emit('data', Buffer.from(value)),
    end: () => { response.complete = true; response.emit('end'); response.emit('close'); }
  };
}
function options(signal = new AbortController().signal) {
  return { method: 'POST', headers: { accept: 'application/json', origin, 'content-type': 'application/json', 'x-csrf-token': 'fixture-csrf' }, body: '{}', signal };
}
test('builtin request options disable agent/idle timeout; no separate 300s header timer', async () => {
  const wire = fakeWire(), fetch = createMaintenanceFetch(origin, wire);
  const pending = fetch(route, options());
  assert.equal(wire.calls[0].options.agent, false); assert.equal(wire.calls[0].options.timeout, 0);
  assert.equal(wire.calls[0].options.maxHeaderSize, 16384); assert.equal(wire.request.listenerCount('timeout'), 0);
  // A socket timeout event itself cannot replace the caller-owned deadline.
  wire.request.emit('timeout'); assert.equal(wire.request.destroyed, false);
  wire.headers(); const response = await pending; wire.body('{"ok":true}'); wire.end();
  assert.equal(await response.text(), '{"ok":true}'); assert.equal(wire.calls.length, 1);
});
test('pre-aborted caller deadline performs zero dispatches', async () => {
  const wire = fakeWire(), controller = new AbortController(); controller.abort(new DOMException('not journalled', 'TimeoutError'));
  await assert.rejects(createMaintenanceFetch(origin, wire)(route, options(controller.signal)), { name: 'TimeoutError' });
  assert.equal(wire.calls.length, 0);
});
test('header wait ends only on caller abort and destroys request once', async () => {
  const wire = fakeWire(), controller = new AbortController();
  const pending = createMaintenanceFetch(origin, wire)(route, options(controller.signal));
  controller.abort(new DOMException('private reason', 'TimeoutError'));
  await assert.rejects(pending, error => error.name === 'TimeoutError' && !error.message.includes('private'));
  assert.equal(wire.request.destroyed, true); assert.equal(wire.calls.length, 1);
});
test('post-header deadline rejects response text and destroys both streams', async () => {
  const wire = fakeWire(), controller = new AbortController();
  const pending = createMaintenanceFetch(origin, wire)(route, options(controller.signal));
  wire.headers(); const response = await pending; wire.body('partial'); controller.abort(new DOMException('private', 'TimeoutError'));
  await assert.rejects(response.text(), { name: 'TimeoutError' }); assert.equal(wire.request.destroyed, true); assert.equal(wire.response.destroyed, true);
});
test('finished body removes abort handler; later abort does not destroy completed request', async () => {
  const wire = fakeWire(), controller = new AbortController();
  const pending = createMaintenanceFetch(origin, wire)(route, options(controller.signal)); wire.headers();
  const response = await pending; wire.body('{}'); wire.end(); await response.text(); controller.abort();
  assert.equal(wire.request.destroyed, false); assert.equal(wire.response.destroyed, false);
});
test('redirect is returned without follow, including external Location', async () => {
  const wire = fakeWire(), pending = createMaintenanceFetch(origin, wire)(route, options());
  wire.headers(302, { location: 'https://foreign.invalid/secrets' }); const response = await pending; wire.end();
  assert.equal(response.status, 302); assert.equal(response.ok, false); assert.equal(await response.text(), ''); assert.equal(wire.calls.length, 1);
});
test('exact origin/path/method and options refusal happens before request', async () => {
  for (const [url, change] of [[origin + '/api/bootstrap', () => {}], ['http://127.0.0.1:9999/api/session', () => {}],
    ['https://127.0.0.1:4186/api/session', () => {}], ['http://user:secret@127.0.0.1:4186/api/session', () => {}],
    [route + '?secret=x', () => {}], [route + '#fragment', () => {}], [route, o => o.method = 'DELETE'],
    [route, o => o.redirect = 'follow'], [route, o => o.headers.host = 'foreign.invalid'],
    [route, o => o.headers.origin = 'https://foreign.invalid'], [route, o => delete o.headers['x-csrf-token']],
    [route, o => o.headers.cookie = 'session=secret\r\nx=y'], [route, o => o.signal = undefined]]) {
    const wire = fakeWire(), input = options(); change(input);
    await assert.rejects(createMaintenanceFetch(origin, wire)(url, input)); assert.equal(wire.calls.length, 0);
  }
});
test('no arbitrary loopback alias or non-http origin is accepted', () => {
  for (const url of ['http://localhost:4186', 'http://127.0.0.2:4186', 'https://127.0.0.1:4186', origin + '/path']) assert.throws(() => createMaintenanceFetch(url));
  assert.equal(typeof createMaintenanceFetch('http://[::1]:4186'), 'function');
});
test('request body bound refuses before dispatch', async () => {
  const wire = fakeWire(), input = options(); input.body = 'x'.repeat(65537);
  await assert.rejects(createMaintenanceFetch(origin, wire)(route, input), { code: 'EMAINTENANCE_REQUEST_SIZE' }); assert.equal(wire.calls.length, 0);
});
test('outbound allowed headers are bounded before any dispatch', async () => {
  const wire = fakeWire(), input = options(); input.headers.cookie = 'session=' + 'x'.repeat(16384);
  await assert.rejects(createMaintenanceFetch(origin, wire)(route, input), { code: 'EMAINTENANCE_HEADERS' }); assert.equal(wire.calls.length, 0);
});
test('oversized advertised response fails without unhandled stream error', async () => {
  const wire = fakeWire(), pending = createMaintenanceFetch(origin, wire)(route, options());
  wire.headers(200, { 'content-length': String(8 * 1024 * 1024 + 1) });
  await assert.rejects(pending, { code: 'EMAINTENANCE_RESPONSE_SIZE' }); assert.equal(wire.response.destroyed, true);
});
test('streamed response bound rejects text after headers', async () => {
  const wire = fakeWire(), pending = createMaintenanceFetch(origin, wire)(route, options()); wire.headers(); const response = await pending;
  wire.body(Buffer.alloc(8 * 1024 * 1024)); wire.body('x');
  await assert.rejects(response.text(), { code: 'EMAINTENANCE_RESPONSE_SIZE' }); assert.equal(wire.request.destroyed, true);
});
test('incomplete end and abrupt response close cannot become valid partial JSON', async () => {
  for (const event of ['end', 'aborted', 'close']) {
    const wire = fakeWire(), pending = createMaintenanceFetch(origin, wire)(route, options()); wire.headers(); const response = await pending;
    wire.body('{}'); wire.response.emit(event);
    await assert.rejects(response.text(), { code: 'EMAINTENANCE_TRUNCATED_BODY' });
  }
});
test('header-side socket error and body-side socket error stay distinct client phases', async () => {
  for (const phase of ['request-headers', 'response-body']) {
    const wire = fakeWire(), client = new CommunityHeroClient({ account: 'LikeAvto', baseUrl: origin,
      timeoutMs: 600000, fetchImpl: createMaintenanceFetch(origin, wire) });
    client.session = { csrfToken: 'fixture' };
    const pending = client.request('/api/maintenance/runtime/begin', { method: 'POST', body: {} });
    if (phase === 'response-body') { wire.headers(); await Promise.resolve(); wire.response.emit('error', Object.assign(new Error('private message'), { code: 'ECONNRESET' })); }
    else wire.request.emit('error', Object.assign(new Error('private message'), { code: 'ECONNREFUSED' }));
    await assert.rejects(pending, error => error.code === 'UNKNOWN_MUTATION_OUTCOME' && error.details.phase === phase);
  }
});
test('safe nested Undici cause survives client, checkpoint and maintenance journal', async () => {
  const cause = new TypeError('fetch failed secret cookie', { cause: Object.assign(new Error('private headers/body'), { code: 'UND_ERR_HEADERS_TIMEOUT', private: 'secret' }) });
  const error = new UnknownMutationError('POST', '/api/maintenance/runtime/begin', cause, { phase: 'request-headers' });
  assert.equal(error.details.cause, 'TypeError'); assert.equal(error.details.nestedCause, 'UND_ERR_HEADERS_TIMEOUT');
  assert.equal(checkpointError(error).details.nestedCause, 'UND_ERR_HEADERS_TIMEOUT');
  assert.equal(maintenanceError(error).details.nestedCause, 'UND_ERR_HEADERS_TIMEOUT');
  assert.equal(JSON.stringify(maintenanceError(error)).includes('secret'), false);
  assert.equal(JSON.stringify(checkpointError(error)).includes('private'), false);
});
test('private unrecognized diagnostic strings never cross safe error serialization', () => {
  const cause = new Error('private', { cause: { code: 'Cookie=private', name: 'private' } });
  const error = new UnknownMutationError('POST', '/api/maintenance/runtime/begin', cause);
  error.details.headers = { cookie: 'private' }; error.details.body = 'private'; error.details.phase = 'private'; error.details.nestedCause = 'Bearer private';
  for (const value of [checkpointError(error), maintenanceError(error)]) assert.equal(JSON.stringify(value).includes('private'), false);
});
test('permanent maintenance client injects scoped transport instead of global fetch', () => {
  let supplied; class Client { constructor(options) { supplied = options; } }
  maintenanceClient(Client, { initialOwner: { account: 'LikeAvto' }, baseUrl: origin });
  assert.equal(supplied.timeoutMs, 600000); assert.equal(typeof supplied.fetchImpl, 'function'); assert.notEqual(supplied.fetchImpl, globalThis.fetch);
});

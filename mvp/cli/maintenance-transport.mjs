// A deliberately narrow fetch seam for the native loopback maintenance client.
// It uses no Undici header/body timer, redirect policy, retry, or connection pool.
import { request as httpRequest } from 'node:http';

const MAX_REQUEST_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 8 * 1024 * 1024;
const MAX_HEADER_BYTES = 16 * 1024;
const POST_PATHS = new Set(['status', 'register-target', 'begin', 'checkpoint']
  .map(action => '/api/maintenance/runtime/' + action));
const HEADER_NAMES = new Set(['accept', 'cookie', 'content-type', 'origin', 'x-csrf-token']);
const SAFE_CODE = /^(?:E[A-Z0-9_]{1,60}|UND_ERR_[A-Z0-9_]{1,50})$/;
const SAFE_NAME = new Set(['TimeoutError', 'AbortError', 'TypeError', 'SyntaxError', 'NetworkError', 'Error']);

function fault(code, cause) {
  const error = new Error('Native maintenance transport failed');
  error.code = code;
  if (cause) error.cause = cause;
  return error;
}
function sanitized(error) {
  const cause = typeof error?.cause?.code === 'string' && SAFE_CODE.test(error.cause.code)
    ? { code: error.cause.code } : undefined;
  const result = fault(typeof error?.code === 'string' && SAFE_CODE.test(error.code) ? error.code : 'EMAINTENANCE_TRANSPORT', cause);
  result.name = SAFE_NAME.has(error?.name) ? error.name : 'Error';
  // Existing admitted clients already retain cause.code. Promote only an
  // allowlisted nested code when the outer error has no usable transport code.
  if (result.code === 'EMAINTENANCE_TRANSPORT' && cause) result.code = cause.code;
  return result;
}
function origin(value) {
  const url = new URL(value);
  if (url.protocol !== 'http:' || !['127.0.0.1', '[::1]'].includes(url.hostname)
    || url.username || url.password || url.href !== url.origin + '/') throw fault('EMAINTENANCE_ORIGIN');
  return url.origin;
}

export function createMaintenanceFetch(baseUrl, { requestImpl = httpRequest } = {}) {
  const allowedOrigin = origin(baseUrl);
  return async function maintenanceFetch(input, options = {}) {
    const url = new URL(input);
    if (url.origin !== allowedOrigin || url.username || url.password || url.search || url.hash)
      throw fault('EMAINTENANCE_ROUTE');
    const method = options.method ?? 'GET';
    if (!((method === 'GET' && url.pathname === '/api/session') || (method === 'POST' && POST_PATHS.has(url.pathname))))
      throw fault('EMAINTENANCE_ROUTE');
    if (Object.keys(options).some(key => !['method', 'headers', 'body', 'signal'].includes(key))) throw fault('EMAINTENANCE_OPTIONS');
    const headers = options.headers ?? {};
    if (!headers || typeof headers !== 'object' || Array.isArray(headers)) throw fault('EMAINTENANCE_HEADERS');
    for (const [name, value] of Object.entries(headers)) {
      if (!HEADER_NAMES.has(name) || typeof value !== 'string' || /[\r\n]/.test(value)) throw fault('EMAINTENANCE_HEADERS');
    }
    // Leave room for the bounded request line and Node's Host/Connection/length.
    if (Object.entries(headers).reduce((size, [name, value]) => size + Buffer.byteLength(name) + Buffer.byteLength(value) + 4, 512) > MAX_HEADER_BYTES)
      throw fault('EMAINTENANCE_HEADERS');
    if (method === 'POST' && (headers.origin !== allowedOrigin || !headers['x-csrf-token'])) throw fault('EMAINTENANCE_CSRF');
    const body = options.body;
    if ((method === 'GET' && body !== undefined) || (method === 'POST' && typeof body !== 'string')
      || (body !== undefined && Buffer.byteLength(body) > MAX_REQUEST_BYTES)) throw fault('EMAINTENANCE_REQUEST_SIZE');
    const signal = options.signal;
    if (!(signal instanceof AbortSignal)) throw fault('EMAINTENANCE_SIGNAL');
    if (signal.aborted) throw sanitized(signal.reason);

    return new Promise((resolveHeaders, rejectHeaders) => {
      let request, response, headersSettled = false, bodySettled = false, failed = false;
      let chunks = [], bytes = 0, resolveBody, rejectBody;
      const bodyPromise = new Promise((resolve, reject) => { resolveBody = resolve; rejectBody = reject; });
      bodyPromise.catch(() => {}); // The client may fail before it can call text().
      const cleanup = () => signal.removeEventListener('abort', abort);
      const fail = error => {
        if (failed || bodySettled) return;
        failed = true; bodySettled = true; chunks = []; cleanup();
        const safe = sanitized(error);
        if (!headersSettled) { headersSettled = true; rejectHeaders(safe); }
        rejectBody(safe);
        request?.destroy(safe); response?.destroy(safe);
      };
      const abort = () => fail(signal.reason);
      signal.addEventListener('abort', abort, { once: true });
      if (signal.aborted) { abort(); return; }
      try {
        request = requestImpl(url, { method, headers, agent: false, timeout: 0, maxHeaderSize: MAX_HEADER_BYTES }, incoming => {
          response = incoming;
          if (failed) { incoming.destroy(); return; }
          incoming.on('error', fail);
          const length = incoming.headers?.['content-length'];
          if (length !== undefined && (!/^(0|[1-9][0-9]*)$/.test(String(length)) || Number(length) > MAX_RESPONSE_BYTES)) {
            fail(fault('EMAINTENANCE_RESPONSE_SIZE')); return;
          }
          const status = incoming.statusCode;
          if (!Number.isInteger(status) || status < 100 || status > 599) { fail(fault('EMAINTENANCE_STATUS')); return; }
          incoming.on('data', chunk => {
            if (bodySettled) return;
            const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
            bytes += buffer.length;
            if (bytes > MAX_RESPONSE_BYTES) { fail(fault('EMAINTENANCE_RESPONSE_SIZE')); return; }
            chunks.push(buffer);
          });
          incoming.once('end', () => {
            if (bodySettled) return;
            if (!incoming.complete) { fail(fault('EMAINTENANCE_TRUNCATED_BODY')); return; }
            bodySettled = true; cleanup();
            const text = Buffer.concat(chunks, bytes).toString('utf8'); chunks = []; resolveBody(text);
          });
          incoming.once('aborted', () => fail(fault('EMAINTENANCE_TRUNCATED_BODY')));
          incoming.once('close', () => { if (!bodySettled) fail(fault('EMAINTENANCE_TRUNCATED_BODY')); });
          headersSettled = true;
          resolveHeaders({ status, ok: status >= 200 && status < 300, text: () => bodyPromise });
        });
        request.on('error', fail);
        request.once('close', () => { if (!headersSettled) fail(fault('EMAINTENANCE_HEADERS_CLOSED')); });
        if (failed) request.destroy(); else request.end(body);
      } catch (error) { fail(error); }
    });
  };
}

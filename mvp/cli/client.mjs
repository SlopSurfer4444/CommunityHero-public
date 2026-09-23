import { readFile } from 'node:fs/promises';

export class CliError extends Error {
  constructor(message, { code = 'CLI_ERROR', status = null, details = null } = {}) {
    super(message);
    this.name = 'CliError';
    this.code = code;
    this.status = status;
    this.details = details;
  }
}

export class UnknownMutationError extends CliError {
  constructor(method, path, cause) {
    super(`The ${method} ${path} outcome is unknown; inspect server state before retrying`, {
      code: 'UNKNOWN_MUTATION_OUTCOME', details: { method, path, cause: cause?.code || cause?.name || 'network' }
    });
  }
}

function normalizeCookie(value) {
  const cookie = String(value || '').trim();
  if (!cookie || /[\r\n]/.test(cookie)) throw new CliError('Session cookie is empty or invalid', { code: 'INVALID_SESSION' });
  return cookie.startsWith('Cookie:') ? cookie.slice(7).trim() : cookie;
}

export async function loadSessionCookie({ sessionFile, env = process.env } = {}) {
  if (sessionFile) {
    const raw = await readFile(sessionFile, 'utf8');
    try {
      const parsed = JSON.parse(raw);
      return normalizeCookie(parsed.cookie || parsed.session);
    } catch (error) {
      if (error instanceof SyntaxError) return normalizeCookie(raw);
      throw error;
    }
  }
  return env.COMMUNITYHERO_SESSION ? normalizeCookie(env.COMMUNITYHERO_SESSION) : '';
}

export class CommunityHeroClient {
  constructor({ baseUrl = 'http://127.0.0.1:4186', account, cookie = '', timeoutMs = 15_000, fetchImpl = fetch }) {
    if (!account) throw new CliError('An explicit --account is required', { code: 'ACCOUNT_REQUIRED' });
    this.baseUrl = new URL(baseUrl).origin;
    this.account = account;
    this.cookie = cookie;
    this.timeoutMs = timeoutMs;
    this.fetchImpl = fetchImpl;
    this.session = null;
  }

  async request(path, { method = 'GET', body, mutation = method !== 'GET' && method !== 'HEAD', timeoutMs = this.timeoutMs } = {}) {
    const headers = { accept: 'application/json' };
    if (this.cookie) headers.cookie = this.cookie;
    if (mutation) {
      if (!this.session?.csrfToken) await this.getSession();
      headers['content-type'] = 'application/json';
      headers.origin = this.baseUrl;
      headers['x-csrf-token'] = this.session.csrfToken;
    }
    let response;
    try {
      response = await this.fetchImpl(new URL(path, this.baseUrl), {
        method, headers, body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(timeoutMs)
      });
    } catch (error) {
      if (mutation) throw new UnknownMutationError(method, path, error);
      throw new CliError(`Cannot reach CommunityHero at ${this.baseUrl}`, { code: 'NETWORK_ERROR', details: { cause: error?.code || error?.name } });
    }
    let text;
    try { text = await response.text(); }
    catch (error) {
      if (mutation) throw new UnknownMutationError(method, path, error);
      throw new CliError(`Cannot read server response for ${path}`, { code: 'INVALID_RESPONSE', status: response.status });
    }
    let payload = null;
    if (mutation && response.ok && !text) throw new UnknownMutationError(method, path, Object.assign(new Error('empty response'), { code: 'EMPTY_RESPONSE' }));
    if (text) {
      try { payload = JSON.parse(text); }
      catch (error) {
        if (mutation && (response.ok || response.status >= 500)) throw new UnknownMutationError(method, path, error);
        throw new CliError(`Server returned non-JSON for ${path}`, { code: 'INVALID_RESPONSE', status: response.status });
      }
    }
    if (mutation && response.status >= 500) throw new UnknownMutationError(method, path, Object.assign(new Error('server error'), { code: `HTTP_${response.status}` }));
    if (!response.ok) throw new CliError(payload?.error || `HTTP ${response.status}`, {
      code: response.status === 409 ? 'STALE_OR_CONFLICT' : 'HTTP_ERROR', status: response.status, details: payload
    });
    return payload;
  }

  async getSession() {
    const session = await this.request('/api/session', { mutation: false });
    if (!session?.csrfToken) throw new CliError('Server session did not include CSRF authority', { code: 'INVALID_SESSION_RESPONSE' });
    this.session = session;
    return session;
  }

  async health() {
    return this.request('/api/health', { mutation: false });
  }

  assertAccount(value) {
    const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
    const actual = value?.account ?? value?.displayAccount;
    if (canonical(actual) !== canonical(this.account)) throw new CliError(`Account mismatch: expected ${this.account}, server returned ${actual || 'none'}`, {
      code: 'WRONG_ACCOUNT', details: { expected: this.account, actual: actual || null }
    });
  }

  async engineStatus() {
    const status = await this.request('/api/engine/status', { mutation: false });
    this.assertAccount(status);
    return status;
  }

  async bootstrap() {
    const snapshot = await this.request('/api/bootstrap', { mutation: false });
    this.assertAccount(snapshot);
    return snapshot;
  }

  async mutate(path, body = {}) {
    await this.bootstrap();
    return this.request(path, { method: 'POST', body, mutation: true });
  }

  sync(body = {}) { return this.mutate('/api/sync', body); }
  importMaterials() { return this.mutate('/api/materials/import', {}); }
  prepareEngine(body) { return this.mutate('/api/engine/prepare', body); }
  createConversation(body) { return this.mutate('/api/conversations', body); }
  sendConversationMessage(id, body) { return this.mutate(`/api/conversations/${encodeURIComponent(id)}/messages`, body); }
  createProposal(body) { return this.mutate('/api/proposals', body); }
  createApproval(refs) { return this.mutate('/api/approvals', { proposals: refs }); }
  execute(approvalId) { return this.mutate(`/api/approvals/${encodeURIComponent(approvalId)}/execute`, {}); }
  reconcile(operationId) { return this.mutate(`/api/operations/${encodeURIComponent(operationId)}/reconcile`, {}); }
  async getJob(jobId) {
    try { return await this.request(`/api/engine/jobs/${encodeURIComponent(jobId)}`, { mutation: false }); }
    catch (error) {
      if (![403, 404].includes(error.status)) throw error;
      const snapshot = await this.bootstrap();
      const job = (snapshot.jobs || []).find(row => row.id === jobId);
      if (!job) throw new CliError(`Job ${jobId} is absent from bounded server history`, { code: 'JOB_NOT_FOUND' });
      return job;
    }
  }
}

export const TERMINAL_JOBS = new Set(['completed', 'failed', 'error', 'cancelled', 'interrupted']);

export async function waitForJob(client, jobId, { pollMs = 1000, maxPolls = 120, signal, onPoll = () => {} } = {}) {
  for (let poll = 0; poll < maxPolls; poll += 1) {
    if (signal?.aborted) throw new CliError('Stopped while polling; the server job may still be running', { code: 'STOPPED' });
    let snapshot = null;
    let job;
    if (typeof client.getJob === 'function') job = await client.getJob(jobId);
    else { snapshot = await client.bootstrap(); job = (snapshot.jobs || []).find(row => row.id === jobId); }
    if (!job) throw new CliError(`Job ${jobId} is absent from bounded server history`, { code: 'JOB_NOT_FOUND' });
    onPoll(job, poll);
    if (TERMINAL_JOBS.has(String(job.status).toLowerCase())) {
      if (job.status !== 'completed') throw new CliError(job.error || `Job ${jobId} ended as ${job.status}`, { code: 'JOB_FAILED', details: job });
      return { job, snapshot: snapshot || await client.bootstrap() };
    }
    if (poll + 1 < maxPolls) await new Promise((resolve, reject) => {
      const timer = setTimeout(resolve, pollMs);
      signal?.addEventListener('abort', () => { clearTimeout(timer); reject(new CliError('Stopped while polling; the server job may still be running', { code: 'STOPPED' })); }, { once: true });
    });
  }
  throw new CliError(`Polling bound reached for job ${jobId}; resume inspection without resending`, { code: 'POLL_LIMIT', details: { jobId, maxPolls } });
}

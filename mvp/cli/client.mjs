import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { familySelectionInput, validateFamilyWindows } from './queue-family.mjs';

// A busy engine may need time to durably admit a batch before returning its ID.
// This bounds one HTTP request, not the lifetime of the admitted background job.
export const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;
export const LOCAL_ADMISSION_TIMEOUT_MS = 300_000;
const MAX_REQUEST_TIMEOUT_MS = 3_600_000;

function validateRequestTimeout(value) {
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_REQUEST_TIMEOUT_MS)
    throw new CliError('Request timeout must be an integer from 1 to 3600000 ms', { code: 'USAGE' });
  return value;
}

function requestTimeout(baseUrl, path, method) {
  const url = new URL(path, baseUrl);
  const loopback = /^(?:localhost|127(?:\.\d{1,3}){3}|\[::1\])$/u.test(url.hostname);
  const admission = ['/api/engine/prepare', '/api/approvals', '/api/proposals/editorial-review'].includes(url.pathname)
    || /^\/api\/approvals\/[^/]+\/execute$/u.test(url.pathname)
    || /^\/api\/engine\/items\/[^/]+\/context-refresh$/u.test(url.pathname);
  return method === 'POST' && loopback && url.origin === baseUrl && admission
    ? LOCAL_ADMISSION_TIMEOUT_MS : DEFAULT_REQUEST_TIMEOUT_MS;
}

const stoppedPolling = () => new CliError('Stopped while observing; the server job may still be running. Resume inspection without resending', { code: 'STOPPED' });

export async function waitForPoll(pollMs, signal) {
  try { await delay(pollMs, undefined, { signal }); }
  catch (error) { if (signal?.aborted) throw stoppedPolling(); throw error; }
}

// Normalize with JSON semantics first (including omitted undefined properties),
// then encode sorted object keys while preserving array order and Unicode text.
export function localAdmissionPayloadHash(body) {
  const { requestId: _requestId, ...payload } = body;
  const normalized = JSON.parse(JSON.stringify(payload));
  const canonical = value => Array.isArray(value) ? `[${value.map(canonical).join(',')}]`
    : value !== null && typeof value === 'object'
      ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`
      : JSON.stringify(value);
  return createHash('sha256').update(canonical(normalized), 'utf8').digest('hex');
}

const safeCauseName = value => ['TimeoutError', 'AbortError', 'TypeError', 'SyntaxError', 'NetworkError', 'Error']
  .includes(value) ? value : null;
const causeCodes = new Set(`ECONNABORTED ECONNREFUSED ECONNRESET EHOSTUNREACH ENETDOWN ENETUNREACH EPIPE ETIMEDOUT
  EAI_AGAIN ENOTFOUND EMPTY_RESPONSE UND_ERR_CONNECT_TIMEOUT UND_ERR_HEADERS_TIMEOUT UND_ERR_BODY_TIMEOUT
  UND_ERR_SOCKET UND_ERR_ABORTED UND_ERR_DESTROYED UND_ERR_RESPONSE_STATUS_CODE UND_ERR_INVALID_ARG
  INVALID_CONTEXT_REFRESH_RESPONSE INVALID_EXECUTION_ADMISSION INVALID_EXECUTION_RESPONSE INVALID_LOCAL_ADMISSION_PAYLOAD
  INVALID_LOCAL_ADMISSION_RESPONSE INVALID_MATERIALS_IMPORT_RESPONSE INVALID_EDITORIAL_ADMISSION`.split(/\s+/u));
const safeCause = value => typeof value === 'string'
  && (/^HTTP_[1-5][0-9]{2}$/u.test(value) || causeCodes.has(value) || safeCauseName(value) !== null) ? value : null;
const safePhase = value => ['request-headers', 'response-body', 'empty-response', 'response-json',
  'response-status', 'response-contract'].includes(value) ? value : null;
const safeErrorCodes = new Set(`CLI_ERROR UNEXPECTED USAGE ACCOUNT_REQUIRED INVALID_SESSION INVALID_SESSION_RESPONSE
  WRONG_ACCOUNT WRONG_SERVER FORBIDDEN UNKNOWN_MUTATION_OUTCOME NETWORK_ERROR READ_TIMEOUT INVALID_RESPONSE HTTP_ERROR
  STALE_OR_CONFLICT STOPPED POLL_LIMIT JOB_NOT_FOUND JOB_FAILED INVALID_CHECKPOINT INVALID_PREPARE_JOB
  INVALID_SCOPE_RESERVATION INVALID_REVIEW_COVERAGE INCOMPLETE_OPERATION_COVERAGE INVALID_EXECUTION_CLOSURE
  INVALID_EDITORIAL_REVIEW INVALID_EXECUTION_ADMISSION INVALID_LOCAL_ADMISSION_RESPONSE LOCAL_ADMISSION_UNCONFIRMED
  MATERIALS_UNAVAILABLE STRICT_GROUPING_REQUIRED ITEM_NOT_FOUND NOT_FOUND AUTONOMOUS_BATCH_LIMIT
  PREPARE_PLAN_SPLIT_REQUIRED INVALID_PREPARE_PLAN INCOMPLETE_WITHOUT_RESUME EXISTING_DRAFT_CHANGED
  INVALID_DEPENDENCY_SCOPE READY_BATCH_STOPPED BULK_STOPPED UNKNOWN_CONVERSATION_HELD CHECKPOINT_LOCKED
  REJECTED_LOCAL_ADMISSION INVALID_LOCAL_REJECTION`.split(/\s+/u));
safeErrorCodes.add('WORKSPACE_GENERATION_MISMATCH');

const generationPattern = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/u;
export function workspaceGeneration(value) {
  if (value === null || generationPattern.test(value || '')) return value;
  throw new CliError('Invalid workspace generation', { code: 'INVALID_CHECKPOINT' });
}

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
  constructor(method, path, cause, { phase = 'response-contract', status = null, timeoutMs = null } = {}) {
    const safeStatus = Number.isInteger(status) && status >= 100 && status <= 599 ? status : null;
    super(`The ${method} ${path} outcome is unknown; inspect server state before retrying`, {
      code: 'UNKNOWN_MUTATION_OUTCOME', status: safeStatus,
      details: { method, path, cause: safeCause(cause?.code) || safeCauseName(cause?.name) || 'network',
        ...(safeCauseName(cause?.name) ? { causeName: safeCauseName(cause.name) } : {}),
        ...(safeCause(cause?.cause?.code) || safeCauseName(cause?.cause?.name)
          ? { nestedCause: safeCause(cause?.cause?.code) || safeCauseName(cause?.cause?.name) } : {}),
        ...(Number.isInteger(timeoutMs) && timeoutMs > 0 ? { timeoutMs } : {}),
        ...(safeStatus !== null ? { status: safeStatus } : {}), phase: safePhase(phase) || 'response-contract' }
    });
  }
}

// Checkpoints retain only bounded transport metadata, never an HTTP body,
// exception message, request payload, cookie, or token.
function checkpointTransportDetails(raw = {}) {
  const details = {};
  if (safeCause(raw.cause) || raw.cause === 'network') details.cause = raw.cause;
  if (safeCauseName(raw.causeName)) details.causeName = raw.causeName;
  if (safeCause(raw.nestedCause)) details.nestedCause = raw.nestedCause;
  if (safePhase(raw.phase)) details.phase = raw.phase;
  if (Number.isInteger(raw.timeoutMs) && raw.timeoutMs > 0 && raw.timeoutMs <= 3_600_000) details.timeoutMs = raw.timeoutMs;
  return details;
}
export function checkpointError(error) {
  if (error?.code !== 'UNKNOWN_MUTATION_OUTCOME') {
    const code = safeErrorCodes.has(error?.code) ? error.code : 'CLI_ERROR';
    const status = Number.isInteger(error?.status) && error.status >= 100 && error.status <= 599 ? error.status : null;
    const value = { code, message: code === 'HTTP_ERROR' ? 'Server read failed' : code === 'STOPPED' ? 'Observation stopped' : code.replaceAll('_', ' ').toLowerCase(),
      ...(status === null ? {} : { status }) };
    if (['NETWORK_ERROR', 'READ_TIMEOUT', 'INVALID_RESPONSE'].includes(code)) {
      const details = checkpointTransportDetails(error?.details || {});
      if (Object.keys(details).length) value.details = details;
    }
    if (code === 'READY_BATCH_STOPPED' && safeErrorCodes.has(error?.details?.causeCode)) {
      const causeCode = error.details.causeCode;
      const causeStatus = Number.isInteger(error.details.causeStatus) && error.details.causeStatus >= 100 && error.details.causeStatus <= 599 ? error.details.causeStatus : null;
      value.message = `Ready batch stopped (${causeCode}${causeStatus === null ? '' : `/${causeStatus}`})`;
      value.details = { causeCode, ...(causeStatus === null ? {} : { causeStatus }) };
    }
    if (code === 'REJECTED_LOCAL_ADMISSION') {
      try {
        const receipt = executeRejection(error.details, error.details);
        value.rejection = Object.fromEntries(['account', 'approvalId', 'requestId', 'payloadHash', 'evaluationId',
          'receiptSha256', 'gateEpoch', 'viewStatus', 'noAttemptProof', 'reevaluationAvailable', 'retryAuthorized'].map(key => [key, receipt[key]]));
      } catch { /* No unvalidated rejection fields may become checkpoint evidence. */ }
    }
    return value;
  }
  const raw = error?.details || {};
  const details = checkpointTransportDetails(raw);
  const status = Number.isInteger(error?.status) && error.status >= 100 && error.status <= 599 ? error.status
    : Number.isInteger(raw.status) && raw.status >= 100 && raw.status <= 599 ? raw.status : null;
  if (status !== null) details.status = status;
  return { code: 'UNKNOWN_MUTATION_OUTCOME', message: 'Mutation outcome unknown; inspect server state before retrying',
    ...(status !== null ? { status } : {}), details };
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
  constructor({ baseUrl = 'http://127.0.0.1:4186', account, cookie = '', timeoutMs, signal, fetchImpl = fetch, workflowGeneration }) {
    if (!account) throw new CliError('An explicit --account is required', { code: 'ACCOUNT_REQUIRED' });
    this.baseUrl = new URL(baseUrl).origin;
    this.account = account;
    this.cookie = cookie;
    this.timeoutMs = timeoutMs === undefined ? undefined : validateRequestTimeout(timeoutMs);
    this.signal = signal;
    this.fetchImpl = fetchImpl;
    this.session = null;
    this.editorialReviewSupportsFresh = true;
    this.workspaceGeneration = undefined;
    if (workflowGeneration !== undefined) this.pinWorkspaceGeneration(workflowGeneration);
  }

  pinWorkspaceGeneration(value) {
    const generation = workspaceGeneration(value);
    if (this.workspaceGeneration !== undefined && this.workspaceGeneration !== generation)
      throw new CliError('Working database changed; the original intent cannot be retargeted', { code: 'WORKSPACE_GENERATION_MISMATCH', status: 409 });
    this.workspaceGeneration = generation;
    return generation;
  }

  async getWorkspaceGeneration() {
    if (this.workspaceGeneration === undefined) await this.getSession();
    return this.workspaceGeneration;
  }

  async request(path, { method = 'GET', body, mutation = method !== 'GET' && method !== 'HEAD', csrf = mutation, timeoutMs = this.timeoutMs, signal = mutation ? undefined : this.signal } = {}) {
    timeoutMs = validateRequestTimeout(timeoutMs === undefined ? requestTimeout(this.baseUrl, path, method) : timeoutMs);
    if (signal?.aborted) throw stoppedPolling();
    const headers = { accept: 'application/json' };
    if (this.cookie) headers.cookie = this.cookie;
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (csrf) {
      if (!this.session?.csrfToken) await this.getSession();
      headers.origin = this.baseUrl;
      headers['x-csrf-token'] = this.session.csrfToken;
    }
    if (this.workspaceGeneration) headers['x-communityhero-workspace-generation'] = this.workspaceGeneration;
    let response;
    try {
      response = await this.fetchImpl(new URL(path, this.baseUrl), {
        method, headers, body: body === undefined ? undefined : JSON.stringify(body),
        signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)]) : AbortSignal.timeout(timeoutMs)
      });
    } catch (error) {
      if (mutation) throw new UnknownMutationError(method, path, error, {
        phase: 'request-headers', ...(error?.name === 'TimeoutError' ? { timeoutMs } : {})
      });
      if (signal?.aborted) throw stoppedPolling();
      const timedOut = error?.name === 'TimeoutError';
      throw new CliError(timedOut
        ? `CommunityHero did not return response headers within ${timeoutMs} ms`
        : `Cannot reach CommunityHero at ${this.baseUrl}`, {
        code: 'NETWORK_ERROR',
        details: { cause: error?.code || error?.name, causeName: error?.name, ...(timedOut ? { timeoutMs } : {}) }
      });
    }
    const responseGeneration = response.headers?.get?.('x-communityhero-workspace-generation');
    if (responseGeneration != null) this.pinWorkspaceGeneration(responseGeneration);
    let text;
    try { text = await response.text(); }
    catch (error) {
      if (mutation) throw new UnknownMutationError(method, path, error, { phase: 'response-body', status: response.status });
      if (signal?.aborted) throw stoppedPolling();
      throw new CliError(`Cannot read server response for ${path}`, { code: 'INVALID_RESPONSE', status: response.status });
    }
    let payload = null;
    if (mutation && response.ok && !text) throw new UnknownMutationError(method, path, { code: 'EMPTY_RESPONSE' }, { phase: 'empty-response', status: response.status });
    if (!mutation && response.ok && !text) throw new CliError('Server returned an empty read response', { code: 'INVALID_RESPONSE', status: response.status });
    if (text) {
      try { payload = JSON.parse(text); }
      catch (error) {
        if (mutation && (response.ok || response.status >= 500)) throw new UnknownMutationError(method, path, error, { phase: 'response-json', status: response.status });
        throw new CliError(`Server returned non-JSON for ${path}`, { code: 'INVALID_RESPONSE', status: response.status });
      }
    }
    if (mutation && response.status >= 500) throw new UnknownMutationError(method, path, { code: `HTTP_${response.status}` }, { phase: 'response-status', status: response.status });
    if (!response.ok) throw new CliError(`HTTP ${response.status}`, {
      code: response.status === 409 ? 'STALE_OR_CONFLICT' : 'HTTP_ERROR', status: response.status, details: payload
    });
    if (['/api/session', '/api/bootstrap', '/api/engine/status'].includes(path)) {
      const generation = Object.hasOwn(payload || {}, 'storageGeneration') ? payload.storageGeneration : responseGeneration ?? null;
      this.pinWorkspaceGeneration(generation);
    }
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

  async requireStrictPreparation() {
    const status = await this.engineStatus();
    if (status.strictGrouping?.version !== 1 || status.strictGrouping.contract !== 'strict_post_family_v1')
      throw new CliError('This engine does not admit strict post/family preparation; no new generation started', { code: 'STRICT_GROUPING_REQUIRED' });
    return status;
  }

  async selectPrepareFamilies(itemIds, batchSize, { maxBatches = 1 } = {}) {
    const body = familySelectionInput(itemIds, batchSize, maxBatches);
    const value = await this.request('/api/engine/prepare/families', {
      method: 'POST', body, mutation: false, csrf: true
    });
    this.assertAccount(value);
    return validateFamilyWindows(value, itemIds, batchSize, maxBatches);
  }

  async bootstrap({ signal } = {}) {
    const snapshot = await this.request('/api/bootstrap', { mutation: false, signal });
    this.assertAccount(snapshot);
    return snapshot;
  }

  async reviewItems(itemIds) {
    if (!Array.isArray(itemIds) || itemIds.length < 1 || itemIds.length > 100
      || new Set(itemIds).size !== itemIds.length
      || itemIds.some(id => typeof id !== 'string' || !id || id.length > 128 || id.includes(','))) {
      throw new CliError('Review requires 1 to 100 distinct exact item IDs', { code: 'USAGE' });
    }
    const selected = itemIds.join(',');
    const review = await this.request(`/api/items/review-bundle?itemIds=${encodeURIComponent(selected)}`, { mutation: false });
    this.assertAccount(review);
    const returned = review?.selectedItemIds;
    const coverage = review?.coverage;
    if (!Array.isArray(returned) || returned.length !== itemIds.length
      || returned.some((id, index) => id !== itemIds[index])
      || !Array.isArray(review.items) || review.items.length !== itemIds.length
      || new Set(review.items.map(row => row?.id)).size !== itemIds.length
      || review.items.some(row => !itemIds.includes(row?.id))
      || !Array.isArray(review.proposals) || !Array.isArray(review.operations)
      || coverage?.itemsReturned !== itemIds.length
      || coverage?.operationsReturned !== review.operations.length
      || typeof coverage?.operationsComplete !== 'boolean'
      || coverage?.historyTruncated !== false) {
      throw new CliError('Selected review has missing or invalid coverage', { code: 'INVALID_REVIEW_COVERAGE' });
    }
    return review;
  }

  // Advisory exact-source partition only. A ready member still needs actual
  // semantic review, native approval and current execution admission.
  async operatorReviewFrontier(proposals) {
    const exactRef = value => value && typeof value === 'object' && !Array.isArray(value)
      && Object.keys(value).length === 2 && typeof value.id === 'string' && value.id.length > 0
      && value.id.length <= 256 && Number.isSafeInteger(value.revision) && value.revision > 0;
    const sameRefs = (actual, expected) => Array.isArray(actual) && actual.length === expected.length
      && actual.every((ref, index) => exactRef(ref) && ref.id === expected[index].id && ref.revision === expected[index].revision);
    if (!Array.isArray(proposals) || proposals.length < 1 || proposals.length > 100
      || proposals.some(ref => !exactRef(ref)) || new Set(proposals.map(ref => ref.id)).size !== proposals.length)
      throw new CliError('Operator review frontier requires 1 to 100 distinct exact proposal revisions', { code: 'USAGE' });
    const frontier = await this.request('/api/proposals/operator-review/frontier', {
      method: 'POST', body: { proposals }, mutation: false, csrf: true
    });
    this.assertAccount(frontier);
    const invalid = () => new CliError('Operator review frontier has invalid exact selection or coverage', { code: 'INVALID_OPERATOR_FRONTIER' });
    const binding = frontier?.connectorBinding;
    const validBinding = binding && typeof binding === 'object' && !Array.isArray(binding)
      && ['id', 'workspaceId', 'accountId', 'connector', 'providerAccountId'].every(key => typeof binding[key] === 'string'
        && binding[key].trim().length > 0 && Buffer.byteLength(binding[key], 'utf8') <= 4096)
      && binding.accountId === frontier.account && Number.isSafeInteger(binding.revision) && binding.revision > 0
      && ['angryspace', 'vk', 'instagram', 'youtube', 'tiktok'].includes(binding.connector);
    if (frontier?.version !== 1 || frontier.contract !== 'communityhero-operator-review-frontier-v1'
      || !validBinding
      || !sameRefs(frontier.requested, proposals) || !Array.isArray(frontier.readyForOperatorReview) || !Array.isArray(frontier.held)) throw invalid();
    const expected = new Map(proposals.map(ref => [ref.id, ref.revision]));
    const seen = new Set();
    const member = ref => {
      if (!exactRef(ref) || expected.get(ref.id) !== ref.revision || seen.has(ref.id)) throw invalid();
      seen.add(ref.id);
    };
    frontier.readyForOperatorReview.forEach(member);
    for (const held of frontier.held) {
      if (!['pending_editorial_review', 'operator_candidate', 'evidence_budget'].includes(held?.stage)
        || typeof held.reason !== 'string' || !held.reason.trim()) throw invalid();
      member(held.reference);
    }
    if (seen.size !== proposals.length) throw invalid();
    const ready = frontier.readyForOperatorReview;
    const preview = frontier.preview;
    if (!ready.length) {
      if (preview !== null) throw invalid();
    } else if (preview?.version !== 1 || preview.contract !== 'communityhero-operator-assisted-editorial-v1'
      || preview.method !== 'assistant_on_operator_authority' || preview.account !== frontier.account
      || localAdmissionPayloadHash({ value: preview.connectorBinding }) !== localAdmissionPayloadHash({ value: frontier.connectorBinding })
      || !sameRefs(preview.proposals, ready) || !Array.isArray(preview.entries) || preview.entries.length !== ready.length
      || preview.entries.some((entry, index) => entry?.candidate?.proposalId !== ready[index].id
        || entry?.candidate?.proposalRevision !== ready[index].revision)
      || typeof preview.previewDigest !== 'string' || !/^[a-f0-9]{64}$/u.test(preview.previewDigest)) throw invalid();
    return frontier;
  }

  async planPrepare(itemIds, instruction) {
    if (!Array.isArray(itemIds) || itemIds.length < 1 || itemIds.length > 100
      || new Set(itemIds).size !== itemIds.length
      || itemIds.some(id => typeof id !== 'string' || !id || id.length > 128)) {
      throw new CliError('Preparation plan requires 1 to 100 distinct item IDs', { code: 'USAGE' });
    }
    const plan = await this.request('/api/engine/prepare/plan', {
      method: 'POST', body: { itemIds, ...(instruction === undefined ? {} : { instruction }) }, mutation: false, csrf: true
    });
    this.assertAccount(plan);
    if (plan.advisory !== true || !Number.isInteger(plan.byteLimit) || plan.byteLimit < 1
      || !Array.isArray(plan.selectedItemIds) || plan.selectedItemIds.length !== itemIds.length
      || plan.selectedItemIds.some((id, index) => id !== itemIds[index])
      || !Array.isArray(plan.batches) || !Array.isArray(plan.held)) {
      throw new CliError('Preparation plan has invalid selection or metadata', { code: 'INVALID_PREPARE_PLAN' });
    }
    const seen = new Set();
    for (const batch of plan.batches) {
      if (!Array.isArray(batch?.itemIds) || !batch.itemIds.length || batch.itemIds.length > 100
        || !Number.isInteger(batch.bytes) || batch.bytes < 1 || batch.bytes > plan.byteLimit)
        throw new CliError('Preparation plan has invalid batch bounds', { code: 'INVALID_PREPARE_PLAN' });
      for (const id of batch.itemIds) {
        if (!itemIds.includes(id) || seen.has(id)) throw new CliError('Preparation plan duplicates or misbinds a batch item', { code: 'INVALID_PREPARE_PLAN' });
        seen.add(id);
      }
    }
    for (const held of plan.held) {
      if (!itemIds.includes(held?.itemId) || seen.has(held.itemId)
        || typeof held.reason !== 'string' || !held.reason.trim())
        throw new CliError('Preparation plan has an invalid held item', { code: 'INVALID_PREPARE_PLAN' });
      seen.add(held.itemId);
    }
    if (seen.size !== itemIds.length) throw new CliError('Preparation plan omitted selected items', { code: 'INVALID_PREPARE_PLAN' });
    return plan;
  }

  async mutate(path, body = {}) {
    await this.engineStatus();
    return this.request(path, { method: 'POST', body, mutation: true });
  }

  sync(body = {}) { return this.mutate('/api/sync', body); }
  async refreshItemContext(itemId) {
    if (typeof itemId !== 'string' || !itemId || itemId.length > 128
      || itemId.trim() !== itemId || /[\u0000-\u001f\u007f]/u.test(itemId))
      throw new CliError('Context refresh requires one exact local item ID', { code: 'USAGE' });
    const path = `/api/engine/items/${encodeURIComponent(itemId)}/context-refresh`;
    await this.engineStatus();
    // The route is intentionally bodyless; even {} is not an accepted request.
    const result = await this.request(path, { method: 'POST', mutation: true });
    if (!result || typeof result.jobId !== 'string' || !result.jobId
      || result.itemId !== itemId || result.status !== 'running'
      || typeof result.deduplicated !== 'boolean')
      throw new UnknownMutationError('POST', path, { code: 'INVALID_CONTEXT_REFRESH_RESPONSE' });
    return result;
  }
  async contextRefreshJob(itemId, jobId) {
    if (typeof itemId !== 'string' || !itemId || itemId.length > 128
      || itemId.trim() !== itemId || /[\u0000-\u001f\u007f]/u.test(itemId)
      || typeof jobId !== 'string' || !jobId || jobId.trim() !== jobId)
      throw new CliError('Context refresh inspection requires exact item and job IDs', { code: 'USAGE' });
    await this.engineStatus();
    const job = await this.getJob(jobId);
    if (job?.id !== jobId || job.kind !== 'target_refresh' || job.refId !== itemId)
      throw new CliError('Job is not the exact context refresh for this local item', { code: 'STALE_OR_CONFLICT' });
    return job;
  }
  async importMaterials() { return materialsImportResult(await this.mutate('/api/materials/import', {})); }
  prepareEngine(body, requestId) { return this.mutate('/api/engine/prepare', { ...body, ...(requestId === undefined ? {} : { requestId }) }); }
  async localAdmission(kind, requestId) {
    if (!['prepare', 'approval', 'execute', 'editorial'].includes(kind) || typeof requestId !== 'string' || !requestId
      || requestId.length > 160 || !/^[A-Za-z0-9_-]+$/u.test(requestId))
      throw new CliError('Local admission inspection requires an exact kind and request ID', { code: 'USAGE' });
    await this.engineStatus();
    return this.request(`/api/local-admissions/${kind}/${encodeURIComponent(requestId)}`, { mutation: false });
  }
  createConversation(body) { return this.mutate('/api/conversations', body); }
  sendConversationMessage(id, body) { return this.mutate(`/api/conversations/${encodeURIComponent(id)}/messages`, body); }
  createProposal(body) { return this.mutate('/api/proposals', body); }
  createApproval(refs, requestId) { return this.mutate('/api/approvals', { proposals: refs, ...(requestId === undefined ? {} : { requestId }) }); }
  editorialReview(proposals, requestId, { fresh = false } = {}) { return this.mutate('/api/proposals/editorial-review', { proposals, requestId, ...(fresh ? { fresh: true } : {}) }); }
  async execute(approvalId, requestId, { reevaluate } = {}) {
    if (reevaluate !== undefined && (!/^[A-Za-z0-9_-]{1,160}$/u.test(requestId || '')
      || !/^[A-Za-z0-9_-]{1,160}$/u.test(reevaluate?.evaluationId || '')
      || !/^[a-f0-9]{64}$/u.test(reevaluate?.receiptSha256 || '') || Object.keys(reevaluate).length !== 2))
      throw new CliError('Re-evaluation requires the exact saved rejection evaluation', { code: 'USAGE' });
    try { return await this.mutate(`/api/approvals/${encodeURIComponent(approvalId)}/execute`, requestId === undefined ? {} : { requestId, ...(reevaluate ? { reevaluate } : {}) }); }
    catch (error) {
      if (error.status !== 409 || requestId === undefined) throw error;
      // HTTP text is never no-attempt proof. Only the exact committed receipt
      // may turn this keyed request into a known local rejection.
      let rejection;
      try {
        const receipt = await this.localAdmission('execute', requestId);
        if (receipt?.status === 'committed') return await recoverExecuteAdmission(this, {
          approvalId, requestId, payloadHash: localAdmissionPayloadHash({ approvalId }) });
        rejection = executeRejection(receipt, {
          approvalId, requestId, payloadHash: localAdmissionPayloadHash({ approvalId }), account: this.account });
        if (reevaluate && (rejection.parentEvaluationId !== reevaluate.evaluationId
          || rejection.parentReceiptSha256 !== reevaluate.receiptSha256)) throw new CliError('Re-evaluation remains unconfirmed', { code: 'INVALID_LOCAL_REJECTION' });
      } catch { throw new UnknownMutationError('POST', executeAdmissionPath(approvalId), { code: 'LOCAL_ADMISSION_UNCONFIRMED' }); }
      throw new CliError('Execution was locally rejected; explicit re-evaluation is required', { code: 'REJECTED_LOCAL_ADMISSION', details: rejection });
    }
  }
  async operatorProgress() {
    const value = await this.engineStatus();
    return value.progress ?? value.continuousPreparation?.progress ?? null;
  }
  reconcile(operationId) { return this.mutate(`/api/operations/${encodeURIComponent(operationId)}/reconcile`, {}); }
  async getJob(jobId, { signal } = {}) {
    try { return await this.request(`/api/engine/jobs/${encodeURIComponent(jobId)}`, { mutation: false, signal }); }
    catch (error) {
      if (error.status !== 404) throw error;
      const snapshot = await this.bootstrap({ signal });
      const job = (snapshot.jobs || []).find(row => row.id === jobId);
      if (!job) throw new CliError(`Job ${jobId} is absent from bounded server history`, { code: 'JOB_NOT_FOUND' });
      return job;
    }
  }
}

export const TERMINAL_JOBS = new Set(['completed', 'failed', 'error', 'cancelled', 'interrupted']);

export const EXECUTE_ADMISSION_PROTOCOL = 'local-admission-v1';
export const executeAdmissionPath = approvalId => `/api/approvals/${encodeURIComponent(approvalId)}/execute`;
export function executeRejection(receipt, { approvalId, requestId, payloadHash, account } = {}) {
  const proof = receipt?.noAttemptProof;
  const canonical = input => String(input || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
  const binding = receipt?.connectionBinding;
  if (receipt?.kind !== 'execute' || receipt.status !== 'rejected_local' || receipt.requestId !== requestId
    || receipt.approvalId !== approvalId || receipt.payloadHash !== payloadHash
    || payloadHash !== localAdmissionPayloadHash({ approvalId }) || receipt.result !== null
    || !['waiting_dependency', 'rejected_local'].includes(receipt.viewStatus)
    || !/^[A-Za-z0-9_-]{1,160}$/u.test(receipt.evaluationId || '')
    || !/^[a-f0-9]{64}$/u.test(receipt.receiptSha256 || '')
    || !/^[a-f0-9]{64}$/u.test(receipt.requestHash || '')
    || !Number.isSafeInteger(receipt.gateEpoch) || receipt.gateEpoch < 1
    || !binding || ['id', 'workspaceId', 'accountId', 'connector', 'providerAccountId'].some(key => typeof binding[key] !== 'string' || !binding[key])
    || !Number.isSafeInteger(binding.revision) || binding.revision < 1
    || typeof receipt.reevaluationAvailable !== 'boolean' || receipt.retryAuthorized !== false
    || !proof || ['executeJobCreated', 'operationCreated', 'approvalConsumed', 'providerDispatchArmed'].some(key => proof[key] !== false)
    || account !== undefined && canonical(receipt.account) !== canonical(account))
    throw new CliError('Exact durable local rejection is unavailable', { code: 'INVALID_LOCAL_REJECTION' });
  return receipt;
}
export function executeAdmissionResult(result, approvalId, requestId) {
  if (!result || typeof result.jobId !== 'string' || !result.jobId || result.jobId.trim() !== result.jobId
    || /[\u0000-\u001f\u007f]/u.test(result.jobId) || result.approvalId !== approvalId
    || result.requestId !== requestId || typeof result.replayed !== 'boolean')
    throw new UnknownMutationError('POST', executeAdmissionPath(approvalId), { code: 'INVALID_EXECUTION_RESPONSE' });
  return result;
}

export async function recoverExecuteAdmission(client, { approvalId, requestId, payloadHash }) {
  const unknown = () => new UnknownMutationError('POST', executeAdmissionPath(approvalId), { code: 'INVALID_EXECUTION_ADMISSION' });
  try {
    if (!approvalId || typeof requestId !== 'string' || !/^[A-Za-z0-9_-]{1,160}$/u.test(requestId)
      || payloadHash !== localAdmissionPayloadHash({ approvalId })) throw unknown();
    // localAdmission verifies the engine account and the server scopes receipts to the actor.
    const receipt = await client.localAdmission('execute', requestId);
    if (receipt?.status === 'rejected_local') {
      const rejection = executeRejection(receipt, { approvalId, requestId, payloadHash, account: client.account });
      throw new CliError('Execution was locally rejected; explicit re-evaluation is required', {
        code: 'REJECTED_LOCAL_ADMISSION', details: rejection });
    }
    if (receipt?.kind !== 'execute' || receipt.requestId !== requestId || receipt.status !== 'committed'
      || receipt.payloadHash !== payloadHash) throw unknown();
    const result = executeAdmissionResult(receipt.result, approvalId, requestId);
    const job = await client.getJob(result.jobId);
    if (job?.id !== result.jobId || job.kind !== 'execute' || job.refId !== approvalId) throw unknown();
    return result;
  } catch (error) {
    // Missing/forbidden receipts and missing jobs do not prove that execution was absent.
    throw ['UNKNOWN_MUTATION_OUTCOME', 'REJECTED_LOCAL_ADMISSION'].includes(error?.code) ? error : unknown();
  }
}

export function materialsImportResult(value) {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    const keys = Object.keys(value).sort().join(',');
    if (keys === 'jobId' && typeof value.jobId === 'string' && value.jobId.trim() === value.jobId
      && value.jobId.length > 0 && !/[\u0000-\u001f\u007f]/.test(value.jobId)) return value;
    if (keys === 'authority,imported,legacyImportSuppressed' && value.imported === 0
      && value.authority === 'communityhero' && value.legacyImportSuppressed === true) return value;
  }
  throw new UnknownMutationError('POST', '/api/materials/import', { code: 'INVALID_MATERIALS_IMPORT_RESPONSE' });
}

export function hasAccountPolicy(snapshot, canonicalOnly = false) {
  const materials = Array.isArray(snapshot?.materials) ? snapshot.materials : [];
  const canonical = value => String(value || '').toLowerCase().replace(/[^a-zа-я0-9]+/giu, '');
  const marker = snapshot?.companyKnowledgeAuthority;
  if (marker?.owner === 'communityhero' && marker.account === snapshot.account
    && canonical(marker.companyKey) === canonical(snapshot.account) && canonical(snapshot.account)) {
    return materials.some(row => row?.companyKnowledge === true && row.account === snapshot.account
      && ['rule', 'policy'].includes(row.kind) && typeof row.text === 'string' && row.text.trim());
  }
  if (canonicalOnly || marker != null) return false;
  return materials.some(row => row?.imported === true && row.kind === 'knowledge');
}

export async function settleMaterialsImport(client, result, options = {}) {
  const admitted = materialsImportResult(result);
  const settled = admitted.jobId ? await waitForJob(client, admitted.jobId, options) : { snapshot: await client.bootstrap(), result: admitted };
  if (!hasAccountPolicy(settled.snapshot, !admitted.jobId)) throw new CliError('Account policy materials are unavailable; generation was not started', { code: 'MATERIALS_UNAVAILABLE' });
  return settled;
}

export async function waitForJob(client, jobId, { pollMs = 1000, maxPolls, signal, onPoll = () => {}, includeSnapshot = true } = {}) {
  if (maxPolls !== undefined && (!Number.isSafeInteger(maxPolls) || maxPolls < 1)
    || !Number.isSafeInteger(pollMs) || pollMs < 0 || pollMs > 2_147_483_647)
    throw new CliError('Invalid polling bounds', { code: 'USAGE' });
  for (let poll = 0; maxPolls === undefined || poll < maxPolls; poll += 1) {
    if (signal?.aborted) throw stoppedPolling();
    let snapshot = null;
    let job;
    if (typeof client.getJob === 'function') job = await client.getJob(jobId, { signal });
    else { snapshot = await client.bootstrap({ signal }); job = (snapshot.jobs || []).find(row => row.id === jobId); }
    if (!job) throw new CliError(`Job ${jobId} is absent from bounded server history`, { code: 'JOB_NOT_FOUND' });
    if (job.id !== jobId) throw new CliError('Polling returned a foreign job', { code: 'JOB_NOT_FOUND' });
    if (!['queued', 'running', ...TERMINAL_JOBS].includes(String(job.status).toLowerCase()))
      throw new CliError('Job status is absent or unsupported', { code: 'INVALID_RESPONSE' });
    await onPoll(job, poll);
    if (signal?.aborted) throw stoppedPolling();
    if (TERMINAL_JOBS.has(String(job.status).toLowerCase())) {
      if (String(job.status).toLowerCase() !== 'completed') throw new CliError(`Job ${jobId} ended as ${job.status}`, { code: 'JOB_FAILED', details: job });
      return { job, snapshot: includeSnapshot ? (snapshot || await client.bootstrap({ signal })) : snapshot };
    }
    if (maxPolls === undefined || poll + 1 < maxPolls) await waitForPoll(pollMs, signal);
  }
  throw new CliError(`Polling bound reached for job ${jobId}; resume inspection without resending`, { code: 'POLL_LIMIT', details: { jobId, maxPolls } });
}

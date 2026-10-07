// A finite adapter for the existing workflow client. The ephemeral capability
// authorizes one Rust grant, never an ordinary owner session or arbitrary URL.
import { CommunityHeroClient, CliError, executeRejection } from './client.mjs';
import { familySelectionInput, validateFamilyWindows } from './queue-family.mjs';
import { connectionWait, validateConnectionBinding, validateConnectionDependency } from './conductor-connection.mjs';

const invalid = () => { throw new CliError('Invalid engine conductor configuration', { code: 'INVALID_CONDUCTOR_CONFIG' }); };
export function validateConfig(config) {
  if (!config || config.version !== 1 || !/^[a-f0-9-]{36}$/iu.test(config.runId || '')
    || !Number.isSafeInteger(config.leaseGeneration) || config.leaseGeneration < 1
    || typeof config.account !== 'string' || !config.account
    || !/^[a-f0-9]{64}$/u.test(config.capability || '') || !['prepare', 'execute'].includes(config.mode)
    || !Array.isArray(config.scopeItemIds) || !config.scopeItemIds.length || config.scopeItemIds.length > 5000
    || new Set(config.scopeItemIds).size !== config.scopeItemIds.length
    || config.scopeItemIds.some(id => typeof id !== 'string' || !id || id.length > 128 || id.trim() !== id || /[,\u0000-\u001f\u007f]/u.test(id))
    || typeof config.checkpointPath !== 'string' || !config.checkpointPath
    || !Number.isInteger(config.batchSize) || config.batchSize < 1 || config.batchSize > 100
    || !Number.isInteger(config.maxRepairRounds) || config.maxRepairRounds < 0 || config.maxRepairRounds > 3
    || !Number.isInteger(config.maxCycles) || config.maxCycles < 1 || config.maxCycles > 50000
    || typeof config.resume !== 'boolean') invalid();
  for (const [field, pathname] of [['baseUrl', '/'], ['rpcUrl', '/rpc']]) {
    let url; try { url = new URL(config[field]); } catch { invalid(); }
    if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port
      || url.username || url.password || url.search || url.hash || url.pathname !== pathname) invalid();
  }
  if (config.cutoffUtc !== null && config.cutoffUtc !== undefined && (typeof config.cutoffUtc !== 'string'
    || !Number.isFinite(Date.parse(config.cutoffUtc)))) invalid();
  validateConnectionBinding(config.connectionBinding, config.account);
  return config;
}

const routeId = value => {
  let id; try { id = decodeURIComponent(value); } catch { invalid(); }
  if (!id || id.length > 256 || /[\/\u0000-\u001f\u007f]/u.test(id)) invalid();
  return id;
};
export function rpcRequest(url, method, body) {
  const path = url.pathname;
  const args = body === undefined ? {} : body;
  let match;
  if (method === 'GET' && !url.search) {
    if (path === '/api/health') return { operation: 'health', args: {} };
    if (path === '/api/engine/status') return { operation: 'engineStatus', args: {} };
    if (path === '/api/bootstrap') return { operation: 'bootstrap', args: {} };
    if ((match = /^\/api\/engine\/jobs\/([^/]+)$/u.exec(path))) return { operation: 'job', args: { jobId: routeId(match[1]) } };
    if ((match = /^\/api\/engine\/posts\/([^/]+)\/media-status$/u.exec(path))) return { operation: 'mediaStatus', args: { postId: routeId(match[1]) } };
    if ((match = /^\/api\/local-admissions\/(prepare|approval|execute|editorial|editorial-repair)\/([^/]+)$/u.exec(path)))
      return { operation: 'localAdmission', args: { kind: match[1], requestId: routeId(match[2]) } };
  }
  if (method === 'GET' && path === '/api/items/review-bundle' && [...url.searchParams.keys()].length === 1 && url.searchParams.has('itemIds'))
    return { operation: 'reviewItems', args: { itemIds: url.searchParams.get('itemIds').split(',') } };
  if (method === 'GET' && path === '/api/conductor/connection-dependency'
    && [...url.searchParams.keys()].length === 2 && ['approvalId', 'requestId'].every(key => url.searchParams.has(key)))
    return { operation: 'connectionDependency', args: { approvalId: routeId(url.searchParams.get('approvalId')), requestId: routeId(url.searchParams.get('requestId')) } };
  if (method === 'POST' && !url.search) {
    const operations = { '/api/engine/prepare/facts/resolve': 'resolvePublicFacts', '/api/engine/prepare/families': 'selectPrepareFamilies', '/api/engine/prepare/plan': 'planPrepare', '/api/sync': 'sync', '/api/materials/import': 'importMaterials',
      '/api/engine/prepare': 'prepare', '/api/proposals/editorial-review': 'editorialReview', '/api/approvals': 'approval' };
    if (operations[path]) return { operation: operations[path], args };
    if ((match = /^\/api\/editorial-reviews\/([^/]+)\/repairs$/u.exec(path)))
      return { operation: 'editorialRepair', args: { ...args, reviewJobId: routeId(match[1]) } };
    if ((match = /^\/api\/approvals\/([^/]+)\/execute$/u.exec(path)))
      return { operation: 'execute', args: { ...args, approvalId: routeId(match[1]) } };
    if ((match = /^\/api\/operations\/([^/]+)\/reconcile$/u.exec(path)))
      return { operation: 'reconcile', args: { ...args, operationId: routeId(match[1]) } };
    if ((match = /^\/api\/engine\/items\/([^/]+)\/context-refresh$/u.exec(path)))
      return { operation: 'refreshContext', args: { itemId: routeId(match[1]) } };
    if (path === '/api/materials/process') return { operation: 'media', args };
  }
  throw new CliError('Conductor operation is outside its finite transport', { code: 'CONDUCTOR_OPERATION_DENIED' });
}

export class ConductorClient extends CommunityHeroClient {
  constructor(config, { fetchImpl = fetch, signal } = {}) {
    validateConfig(config);
    const transport = async (url, init) => {
      const request = rpcRequest(new URL(url), init.method || 'GET', init.body === undefined ? undefined : JSON.parse(init.body));
      const response = await fetchImpl(config.rpcUrl, { method: 'POST', signal: init.signal,
        headers: { accept: 'application/json', 'content-type': 'application/json',
          'x-conductor-capability': config.capability, 'x-conductor-run': config.runId,
          'x-conductor-generation': String(config.leaseGeneration),
          ...(init.headers?.['x-communityhero-workspace-generation'] ? { 'x-communityhero-workspace-generation': init.headers['x-communityhero-workspace-generation'] } : {}) }, body: JSON.stringify(request) });
      if (!response.ok) return response;
      const value = await response.json();
      if (!value || !Number.isInteger(value.status) || value.status < 100 || value.status > 599
        || !Object.hasOwn(value, 'body')) throw new TypeError('Invalid conductor RPC envelope');
      return new Response(JSON.stringify(value.body), { status: value.status, headers: { 'content-type': 'application/json' } });
    };
    super({ baseUrl: config.baseUrl, account: config.account, fetchImpl: transport, signal, workflowGeneration: config.workspaceGeneration ?? null });
    this.editorialReviewSupportsFresh = true;
    this.factScope = new Set(config.scopeItemIds);
    this.conductorConfig = structuredClone(config);
  }
  request(path, options = {}) { return super.request(path, { ...options, csrf: false }); }
  async engineStatus() {
    const value = await super.engineStatus();
    if (!Object.hasOwn(value, 'connectionDependency')) invalid();
    if (value.connectionDependency !== null) validateConnectionDependency(value.connectionDependency, this.conductorConfig, null);
    return value;
  }
  async assertConnectionReady() {
    const value = await this.engineStatus();
    if (value.connectionDependency !== null)
      throw connectionWait(validateConnectionDependency(value.connectionDependency, this.conductorConfig, null));
  }
  async mutate(path, body = {}) {
    const value = await this.engineStatus();
    // Original reconcile-only work keeps its canonical native guards. A closed
    // connection never authorizes a retry or a new ordinary admission.
    if (value.connectionDependency !== null && !/^\/api\/operations\/[^/]+\/reconcile$/u.test(path))
      throw connectionWait(validateConnectionDependency(value.connectionDependency, this.conductorConfig, null), { beforeMutationPath: path });
    return this.request(path, { method: 'POST', body, mutation: true });
  }
  async execute(approvalId, requestId, options) {
    if (options && Object.keys(options).length)
      throw new CliError('Private conductor cannot request execution re-evaluation', { code: 'CONDUCTOR_OPERATION_DENIED' });
    try { return await super.execute(approvalId, requestId, options); }
    catch (error) {
      if (error.code !== 'REJECTED_LOCAL_ADMISSION' || error.details?.viewStatus !== 'waiting_dependency') throw error;
      throw await this.connectionRejection(error);
    }
  }
  async connectionRejection(error) {
    if (error.code !== 'REJECTED_LOCAL_ADMISSION' || error.details?.viewStatus !== 'waiting_dependency') return error;
    const negative = executeRejection(error.details, { ...error.details, account: this.account });
    const { approvalId, requestId } = negative;
    const query = new URLSearchParams({ approvalId, requestId });
    const dependency = await this.request(`/api/conductor/connection-dependency?${query}`, { mutation: false });
    return connectionWait(validateConnectionDependency(dependency, this.conductorConfig, negative), { rejectionError: error });
  }
  async selectPrepareFamilies(itemIds, batchSize, { maxBatches = 1 } = {}) {
    const body = familySelectionInput(itemIds, batchSize, maxBatches);
    const value = await this.request('/api/engine/prepare/families', { method: 'POST', body, mutation: false });
    this.assertAccount(value);
    return validateFamilyWindows(value, itemIds, batchSize, maxBatches);
  }
  editorialReview(proposals, requestId, { fresh = false } = {}) {
    return this.mutate('/api/proposals/editorial-review', { proposals, requestId, ...(fresh ? { fresh: true } : {}) });
  }
  editorialRepair(reviewJobId, expected, requestId) {
    return this.mutate(`/api/editorial-reviews/${encodeURIComponent(reviewJobId)}/repairs`, { requestId, expected });
  }
  media(postId) { return this.mutate('/api/materials/process', { postId }); }
  async resolvePublicFacts(prepareJobId, itemIds) {
    if (typeof prepareJobId !== 'string' || !prepareJobId || prepareJobId.length > 256
      || !Array.isArray(itemIds) || !itemIds.length || itemIds.length > 100
      || new Set(itemIds).size !== itemIds.length || itemIds.some(id => !this.factScope.has(id)))
      throw new CliError('Public facts require exact granted recipients and a parent preparation', { code: 'INVALID_DEPENDENCY_SCOPE' });
    return this.mutate('/api/engine/prepare/facts/resolve', { prepareJobId, itemIds });
  }
  mediaStatus(postId) { return this.request(`/api/engine/posts/${encodeURIComponent(postId)}/media-status`, { mutation: false }); }
  async localAdmission(kind, requestId) {
    if (kind !== 'editorial-repair') return super.localAdmission(kind, requestId);
    if (!/^[A-Za-z0-9_-]{1,160}$/u.test(requestId || '')) invalid();
    await this.engineStatus();
    return this.request(`/api/local-admissions/editorial-repair/${encodeURIComponent(requestId)}`, { mutation: false });
  }
}

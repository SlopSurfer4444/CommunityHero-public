// Native maintenance only. No provider, SQL, process control, or automatic retry.
import fs from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { createMaintenanceFetch } from './maintenance-transport.mjs';

const ROUTE = '/api/maintenance/runtime/';
const ACCOUNTS = { likeavto: 'LikeAvto', 'baw-russia': 'BAW Russia' };
const hash = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const key = value => typeof value === 'string' && /^[a-zA-Z0-9_-]{1,80}$/.test(value);
const exact = (value, fields) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).length === fields.length && fields.every(field => Object.hasOwn(value, field));
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const same = (a, b) => assert.deepEqual(a, b);
const pin = value => exact(value, ['path', 'sha256']) && path.isAbsolute(value.path) && hash(value.sha256);

export function validatePlan(plan, target) {
  assert.ok(exact(plan, ['schemaVersion', 'kind', 'account', 'baseUrl', 'currentCore', 'currentState',
    'initialOwner', 'targetAdmission', 'logicalAttemptId', 'journalDirectory']), 'Exact maintenance plan required');
  assert.equal(plan.schemaVersion, 1);
  assert.equal(plan.kind, 'communityhero-runtime-maintenance-plan');
  assert.ok(Object.hasOwn(ACCOUNTS, plan.account), 'Explicit supported company required');
  const url = new URL(plan.baseUrl);
  assert.ok(url.protocol === 'http:' && ['127.0.0.1', '[::1]'].includes(url.hostname)
    && !url.username && !url.password && url.href === url.origin + '/', 'Explicit loopback origin required');
  assert.ok(pin(plan.currentCore) && pin(plan.currentState) && pin(plan.targetAdmission), 'Artifact pins required');
  const owner = plan.initialOwner;
  assert.ok(exact(owner, ['account', 'runtimeId', 'releaseSha256', 'epoch']), 'Exact owner required');
  assert.equal(owner.account, ACCOUNTS[plan.account]);
  assert.ok(key(owner.runtimeId) && owner.releaseSha256 === plan.currentCore.sha256);
  assert.ok(Number.isSafeInteger(owner.epoch) && owner.epoch > 0 && owner.epoch < Number.MAX_SAFE_INTEGER);
  assert.ok(key(plan.logicalAttemptId) && path.isAbsolute(plan.journalDirectory));
  assert.ok(exact(target, ['schemaVersion', 'kind', 'account', 'installedCore', 'target', 'admissionReceiptSha256']));
  assert.equal(target.schemaVersion, 1);
  assert.equal(target.kind, 'root-reviewed-native-runtime-target-admission');
  assert.equal(target.account, owner.account);
  assert.ok(pin(target.installedCore) && hash(target.admissionReceiptSha256));
  assert.ok(exact(target.target, ['releaseSha256', 'mediaAnalysisGeneration', 'asrDisabled']));
  assert.equal(target.target.releaseSha256, target.installedCore.sha256);
  assert.notEqual(target.target.releaseSha256, plan.currentCore.sha256, 'A distinct admitted target is required');
  assert.ok(Number.isSafeInteger(target.target.mediaAnalysisGeneration) && target.target.mediaAnalysisGeneration >= 0);
  assert.equal(typeof target.target.asrDisabled, 'boolean');
  assert.ok(target.target.mediaAnalysisGeneration !== 0 || target.target.asrDisabled);
}

export function maintenanceRequest(plan, target, { action, invocationId, ownerEpoch }) {
  validatePlan(plan, target);
  assert.ok(['status', 'register-target', 'begin', 'checkpoint'].includes(action), 'Unsupported maintenance action');
  assert.ok(key(invocationId), 'A fresh physical invocation ID is required');
  const initial = plan.initialOwner.epoch;
  const epoch = action === 'checkpoint' ? initial + 1 : action === 'status' ? (ownerEpoch ?? initial) : initial;
  assert.ok(Number.isSafeInteger(epoch) && [initial, initial + 1].includes(epoch));
  assert.ok(ownerEpoch === undefined || (action === 'status' && ownerEpoch === epoch), 'Epoch override is status-only');
  return { plan, target, action, invocationId, owner: { ...plan.initialOwner, epoch } };
}

function targetOf(s) { return { ...s.target.target, attemptId: s.plan.logicalAttemptId }; }
function statusOwner(value, owner) {
  same(value.owner, owner); same(value.lifecycle?.owner, owner);
  assert.equal(value.stopAuthorized, false);
  assert.ok(['running', 'draining', 'drained', 'stopped'].includes(value.lifecycle.phase));
  return value;
}
function maintenanceStatus(value, owner) {
  statusOwner(value, owner);
  assert.equal(value.schemaVersion, 1); assert.equal(value.kind, 'native-runtime-maintenance');
  return value;
}
function settled(value) {
  for (const field of ['applicationTasks', 'nativeActive', 'nativeUnresolved', 'credentialWriters']) assert.equal(value[field], 0);
  const provider = value.provider;
  assert.equal(provider.phase, 'drained'); assert.equal(provider.workerRetired, true); assert.equal(provider.containment, true);
  assert.equal(provider.queued, 0); assert.equal(provider.dispatched, 0); assert.equal(provider.unresolvedStage, null);
}
function drained(s, value) {
  maintenanceStatus(value, s.owner);
  const lifecycle = value.lifecycle;
  assert.ok(exact(lifecycle, ['schemaVersion', 'owner', 'phase', 'history', 'target', 'transfer', 'mediaAnalysisGeneration', 'queuedBacklog']));
  assert.equal(lifecycle.schemaVersion, 1); assert.ok(Array.isArray(lifecycle.history));
  assert.equal(lifecycle.phase, 'drained'); same(lifecycle.target, targetOf(s));
  const transfer = lifecycle.transfer;
  assert.ok(exact(transfer, ['ledgerSha256', 'owner', 'target', 'nativeSettled', 'queuedBacklog']));
  assert.ok(hash(transfer.ledgerSha256)); same(transfer.owner, s.owner); same(transfer.target, lifecycle.target);
  assert.equal(transfer.nativeSettled, true); same(transfer.queuedBacklog, lifecycle.queuedBacklog);
  const backlog = transfer.queuedBacklog;
  assert.ok(exact(backlog, ['version', 'owner', 'jobs'])); assert.equal(backlog.version, 1);
  same(backlog.owner, s.owner); assert.ok(Array.isArray(backlog.jobs));
  const ids = new Set();
  for (const entry of backlog.jobs) {
    assert.ok(exact(entry, ['jobId', 'rowSha256']) && typeof entry.jobId === 'string' && entry.jobId.length > 0 && hash(entry.rowSha256));
    assert.ok(!ids.has(entry.jobId)); ids.add(entry.jobId);
  }
  settled(value);
  return transfer;
}
export function maintenanceError(error) {
  const raw = error?.details ?? {}, details = {};
  const cause = value => typeof value === 'string' && /^(?:HTTP_[1-5][0-9]{2}|E[A-Z0-9_]{1,60}|UND_ERR_[A-Z0-9_]{1,50}|INVALID_[A-Z0-9_]{1,50}|TimeoutError|AbortError|TypeError|SyntaxError|NetworkError|Error|network)$/.test(value);
  for (const field of ['cause', 'causeName', 'nestedCause']) if (cause(raw[field])) details[field] = raw[field];
  if (['request-headers', 'response-body', 'empty-response', 'response-json', 'response-status', 'response-contract'].includes(raw.phase)) details.phase = raw.phase;
  if (Number.isInteger(raw.timeoutMs) && raw.timeoutMs > 0 && raw.timeoutMs <= 1_800_000) details.timeoutMs = raw.timeoutMs;
  return { code: typeof error?.code === 'string' && /^[A-Z0-9_]{1,80}$/.test(error.code) ? error.code : 'MAINTENANCE_REFUSED',
    status: Number.isInteger(error?.status) && error.status >= 100 && error.status <= 599 ? error.status : null,
    ...(Object.keys(details).length ? { details } : {}), noAutomaticRetry: true };
}

// journal writes immutable per-invocation records; reserveDispatch writes one
// fsynced logical action intent. A new invocation never bypasses that intent.
export async function executeMaintenance(s, { client, journal, hasDispatch, reserveDispatch }) {
  const expected = maintenanceRequest(s.plan, s.target, { action: s.action, invocationId: s.invocationId,
    ...(s.action === 'status' ? { ownerEpoch: s.owner.epoch } : {}) });
  same(s.owner, expected.owner);
  if (s.action !== 'status') assert.equal(await hasDispatch(s.action), false, 'Logical action already dispatched; observe and reconcile explicitly');
  const intent = { action: s.action, account: s.plan.account, owner: s.owner, logicalAttemptId: s.plan.logicalAttemptId,
    invocationId: s.invocationId, targetPin: s.plan.targetAdmission, stopAuthorized: false };
  await journal('intent.json', intent);
  const observe = owner => client.request(ROUTE + 'status', { method: 'POST', body: { owner }, mutation: false, csrf: true });
  const before = maintenanceStatus(await observe(s.owner), s.owner);
  await journal('before.json', before);
  if (s.action === 'status') {
    const result = { status: 'observed', value: before, stopAuthorized: false };
    await journal('result.json', result); return result;
  }
  if (s.action === 'checkpoint' && before.lifecycle.phase === 'drained') {
    const result = { status: 'drained-readback', owner: s.owner, transfer: drained(s, before), value: before, postDispatched: false, stopAuthorized: false };
    await journal('result.json', result); return result;
  }
  if (s.action === 'register-target' || s.action === 'begin') {
    assert.equal(before.lifecycle.phase, 'running'); assert.equal(before.lifecycle.target, null); assert.equal(before.lifecycle.transfer, null);
  } else {
    assert.equal(before.lifecycle.phase, 'draining'); same(before.lifecycle.target, targetOf(s));
    try { settled(before); } catch {
      const result = { status: 'held-not-dispatched', owner: s.owner, value: before, noAutomaticRetry: true, stopAuthorized: false };
      await journal('result.json', result); return result;
    }
  }
  const body = s.action === 'register-target' ? { owner: s.owner, admission: s.plan.targetAdmission }
    : s.action === 'begin' ? { owner: s.owner, attemptId: s.plan.logicalAttemptId, releaseSha256: s.target.target.releaseSha256 } : { owner: s.owner };
  const dispatch = { ...intent, route: ROUTE + s.action, body };
  await reserveDispatch(s.action, dispatch);
  await journal('dispatch.json', dispatch);
  try {
    const ack = await client.request(dispatch.route, { method: 'POST', body, mutation: true, csrf: true });
    await journal('ack.json', ack);
    if (s.action === 'register-target') {
      same(ack.owner, s.owner); same(ack.admission, s.plan.targetAdmission);
      same(ack.registeredTarget, { ...s.target.target, admissionReceiptSha256: s.target.admissionReceiptSha256 });
      assert.equal(ack.stopAuthorized, false);
    } else if (s.action === 'begin') {
      statusOwner(ack, { ...s.owner, epoch: s.owner.epoch + 1 });
      assert.equal(ack.lifecycle.phase, 'draining'); same(ack.lifecycle.target, targetOf(s));
    } else {
      const after = maintenanceStatus(await observe(s.owner), s.owner); await journal('after.json', after);
      const result = { status: 'drained-readback', owner: s.owner, transfer: drained(s, after), value: after, acknowledgement: true, stopAuthorized: false };
      await journal('result.json', result); return result;
    }
    const result = { status: 'acknowledged', action: s.action, owner: ack.owner, value: ack, stopAuthorized: false };
    await journal('result.json', result); return result;
  } catch (error) {
    await journal('error.json', maintenanceError(error));
    const owner = s.action === 'begin' ? { ...s.owner, epoch: s.owner.epoch + 1 } : s.owner;
    try {
      const after = maintenanceStatus(await observe(owner), owner); await journal('after-error.json', after);
      // R7 can return HTTP400 after persisting the complete five-field transfer.
      // Only an exact drained native readback can settle checkpoint uncertainty.
      if (s.action === 'checkpoint') {
        const result = { status: 'drained-readback', owner, transfer: drained(s, after), value: after, acknowledgement: false, stopAuthorized: false };
        await journal('result.json', result); return result;
      }
    } catch (readError) { await journal('reconciliation-error.json', maintenanceError(readError)); }
    const result = { status: 'unresolved', noAutomaticRetry: true, stopAuthorized: false };
    await journal('result.json', result); return result;
  }
}

async function noLink(file) {
  assert.ok(path.isAbsolute(file));
  for (let parent = path.resolve(file); ; parent = path.dirname(parent)) {
    try { assert.equal((await fs.lstat(parent)).isSymbolicLink(), false, 'Linked artifact path refused'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (path.dirname(parent) === parent) break;
  }
}
async function readPin(reference) {
  assert.ok(pin(reference), 'Exact file pin required'); await noLink(reference.path);
  const bytes = await fs.readFile(reference.path); assert.equal(digest(bytes), reference.sha256, 'Artifact hash mismatch');
  return JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/, ''));
}
async function writeNew(file, value) {
  await noLink(file);
  const handle = await fs.open(file, 'wx', 0o600);
  try { await handle.writeFile(JSON.stringify(value, null, 2) + '\n'); await handle.sync(); }
  finally { await handle.close(); }
}
async function exists(file) { try { await fs.lstat(file); return true; } catch (error) { if (error.code === 'ENOENT') return false; throw error; } }

export async function loadMaintenancePlan(planPin) {
  const plan = await readPin(planPin);
  const target = await readPin(plan.targetAdmission); validatePlan(plan, target);
  const core = await readPin(plan.currentCore), state = await readPin(plan.currentState);
  assert.equal(core.schemaVersion, 1); assert.equal(core.kind, 'company-independent-immutable-core');
  assert.ok(path.isAbsolute(core.runtimeRoot) && hash(core.binarySha256) && Array.isArray(core.assets));
  assert.equal(state.account, plan.account); assert.equal(state.status, 'running');
  assert.equal(state.coreSha256, plan.currentCore.sha256); assert.equal(state.binarySha256, core.binarySha256);
  const inside = path.relative(core.runtimeRoot, plan.journalDirectory);
  assert.ok(path.isAbsolute(inside) || inside === '..' || inside.startsWith('..' + path.sep), 'Journal cannot modify immutable runtime assets');
  const assets = core.assets.filter(asset => typeof asset.path === 'string' && asset.path.startsWith('cli/'));
  assert.ok(assets.length > 0); const seen = new Set();
  for (const asset of assets) {
    assert.ok(!asset.path.includes('\\') && asset.path.split('/').every(part => part && part !== '.' && part !== '..'));
    assert.ok(hash(asset.sha256) && !seen.has(asset.path.toLowerCase())); seen.add(asset.path.toLowerCase());
    const file = path.join(core.runtimeRoot, asset.path); await noLink(file);
    assert.equal(digest(await fs.readFile(file)), asset.sha256, 'Current CLI asset changed');
  }
  assert.ok(seen.has('cli/client.mjs'), 'Current core must include its admitted client');
  return { plan, target, clientPath: path.join(core.runtimeRoot, 'cli/client.mjs') };
}

// Fresh physical directories coexist under one immutable logical binding. A
// crashed mutation leaves active.lock; recovery is explicitly owned, never TTL.
export async function withMaintenanceJournal(s, planPin, run) {
  const root = s.plan.journalDirectory; await noLink(root); await fs.mkdir(root, { recursive: true });
  const binding = { plan: planPin, account: s.plan.account, logicalAttemptId: s.plan.logicalAttemptId };
  try { await writeNew(path.join(root, 'binding.json'), binding); }
  catch (error) { if (error.code !== 'EEXIST') throw error; same(JSON.parse(await fs.readFile(path.join(root, 'binding.json'), 'utf8')), binding); }
  const directory = path.join(root, 'invocations', s.invocationId);
  await noLink(directory); await fs.mkdir(path.dirname(directory), { recursive: true });
  await fs.mkdir(directory); // Existence refuses reuse, including incomplete invocations.
  const lock = path.join(root, 'active.lock'), token = { invocationId: s.invocationId, nonce: randomUUID() };
  const mutation = s.action !== 'status';
  if (mutation) await writeNew(lock, token);
  try {
    return await run({
      journal: (name, value) => writeNew(path.join(directory, name), value),
      hasDispatch: action => exists(path.join(root, action + '.dispatch.json')),
      reserveDispatch: (action, value) => writeNew(path.join(root, action + '.dispatch.json'), value)
    });
  } finally {
    if (mutation) { same(JSON.parse(await fs.readFile(lock, 'utf8')), token); await fs.unlink(lock); }
  }
}

export function maintenanceClient(Client, plan, { cookie = '', timeoutMs = 600_000 } = {}) {
  assert.ok(Number.isInteger(timeoutMs) && timeoutMs >= 1 && timeoutMs <= 1_800_000, 'Maintenance timeout must be 1..1800000 ms');
  return new Client({ account: plan.initialOwner.account, baseUrl: plan.baseUrl, cookie, timeoutMs,
    fetchImpl: createMaintenanceFetch(plan.baseUrl) });
}

export async function runMaintenance({ planPin, action, invocationId, ownerEpoch, account, baseUrl, cookie = '', timeoutMs }) {
  const loaded = await loadMaintenancePlan(planPin);
  assert.equal(account, loaded.plan.account, 'CLI account must exactly match the plan');
  if (baseUrl !== undefined) assert.equal(new URL(baseUrl).origin, new URL(loaded.plan.baseUrl).origin);
  const request = maintenanceRequest(loaded.plan, loaded.target, { action, invocationId, ownerEpoch });
  const { CommunityHeroClient } = await import(pathToFileURL(loaded.clientPath));
  const client = maintenanceClient(CommunityHeroClient, loaded.plan, { cookie, timeoutMs });
  return withMaintenanceJournal(request, planPin, journal => executeMaintenance(request, { client, ...journal }));
}

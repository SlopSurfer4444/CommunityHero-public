import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { CommunityHeroClient } from '../cli/client.mjs';
import { validatePlan, maintenanceRequest, executeMaintenance, loadMaintenancePlan,
  withMaintenanceJournal, maintenanceClient } from '../cli/maintenance.mjs';

const copy = value => structuredClone(value);
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
function fixture(action = 'checkpoint', account = 'likeavto') {
  const display = account === 'likeavto' ? 'LikeAvto' : 'BAW Russia';
  const plan = { schemaVersion: 1, kind: 'communityhero-runtime-maintenance-plan', account,
    baseUrl: 'http://127.0.0.1:4199', currentCore: { path: path.resolve('current-core.json'), sha256: 'a'.repeat(64) },
    currentState: { path: path.resolve('current-state.json'), sha256: 'b'.repeat(64) },
    initialOwner: { account: display, runtimeId: 'exact_runtime', releaseSha256: 'a'.repeat(64), epoch: 7 },
    targetAdmission: { path: path.resolve('target.json'), sha256: 'c'.repeat(64) },
    logicalAttemptId: 'stable_transfer', journalDirectory: path.resolve('journal') };
  const target = { schemaVersion: 1, kind: 'root-reviewed-native-runtime-target-admission', account: display,
    installedCore: { path: path.resolve('future-core.json'), sha256: 'd'.repeat(64) },
    target: { releaseSha256: 'd'.repeat(64), mediaAnalysisGeneration: 1, asrDisabled: false }, admissionReceiptSha256: 'e'.repeat(64) };
  return maintenanceRequest(plan, target, { action, invocationId: 'physical_01' });
}
function observation(s, phase = 'draining', owner = s.owner) {
  const target = phase === 'running' ? null : { ...s.target.target, attemptId: s.plan.logicalAttemptId };
  const backlog = phase === 'running' ? null : { version: 1, owner, jobs: [{ jobId: 'retained_paid_unknown', rowSha256: 'f'.repeat(64) }] };
  return { schemaVersion: 1, kind: 'native-runtime-maintenance', owner, stopAuthorized: false,
    lifecycle: { schemaVersion: 1, owner, phase, history: [], target, transfer: phase === 'drained'
      ? { ledgerSha256: '1'.repeat(64), owner, target, nativeSettled: true, queuedBacklog: backlog } : null,
    mediaAnalysisGeneration: 1, queuedBacklog: backlog }, applicationTasks: 0, nativeActive: 0, nativeUnresolved: 0,
    credentialWriters: 0, provider: { phase: 'drained', workerRetired: true, containment: true, queued: 0, dispatched: 0, unresolvedStage: null } };
}
function nativeBeginAck(s) {
  const status = observation(s, 'draining', { ...s.owner, epoch: s.owner.epoch + 1 });
  // Actual native begin does not return the status endpoint's schema/kind/counts.
  return { lifecycle: status.lifecycle, provider: status.provider, owner: status.owner, stopAuthorized: false };
}
function harness(observations, effect = () => assert.fail('Unexpected mutation'), sharedDispatch = new Map()) {
  const records = new Map(), calls = [];
  return { records, calls, sharedDispatch,
    journal: async (name, value) => { assert.ok(!records.has(name), 'Immutable journal'); records.set(name, copy(value)); },
    hasDispatch: async action => sharedDispatch.has(action),
    reserveDispatch: async (action, value) => { assert.ok(!sharedDispatch.has(action)); sharedDispatch.set(action, copy(value)); },
    client: { request: async (route, options) => {
      assert.equal(options.method, 'POST'); assert.equal(options.csrf, true); calls.push({ route, ...copy(options) });
      if (!options.mutation) { assert.equal(route, '/api/maintenance/runtime/status'); return copy(observations.shift()); }
      assert.ok(records.has('dispatch.json')); assert.ok(sharedDispatch.has(route.split('/').at(-1)));
      return effect(route, options.body);
    } }
  };
}

test('status uses exact owner and CSRF read-only POST without bootstrap', async () => {
  const s = fixture('status'), h = harness([observation(s, 'running')]);
  assert.equal((await executeMaintenance(s, h)).status, 'observed');
  assert.equal(h.calls.length, 1); assert.equal(h.calls[0].mutation, false); assert.equal(h.sharedDispatch.size, 0);
});
test('registration uses current owner and separate future admission pin', async () => {
  const s = fixture('register-target'), h = harness([observation(s, 'running')], (_, body) => {
    assert.deepEqual(body, { owner: s.owner, admission: s.plan.targetAdmission });
    return { owner: s.owner, admission: s.plan.targetAdmission, registeredTarget: { ...s.target.target, admissionReceiptSha256: s.target.admissionReceiptSha256 }, stopAuthorized: false };
  });
  assert.equal((await executeMaintenance(s, h)).status, 'acknowledged');
});
test('real native begin ACK shape accepts only next epoch and exact target', async () => {
  const s = fixture('begin'), h = harness([observation(s, 'running')], (_, body) => {
    assert.deepEqual(body, { owner: s.owner, attemptId: s.plan.logicalAttemptId, releaseSha256: s.target.target.releaseSha256 });
    return nativeBeginAck(s);
  });
  assert.equal((await executeMaintenance(s, h)).owner.epoch, 8);
});
test('executor refuses forged begin epoch even if caller bypasses request constructor', async () => {
  const s = fixture('begin'); s.owner.epoch++;
  const h = harness([]); await assert.rejects(executeMaintenance(s, h)); assert.equal(h.calls.length, 0);
});
test('wrong company/core/target/owner/epoch and remote origin fail before effects', async () => {
  for (const change of [s => s.plan.account = 'baw-russia', s => s.plan.initialOwner.runtimeId = '../foreign',
    s => s.plan.initialOwner.epoch = Number.MAX_SAFE_INTEGER, s => s.target.account = 'BAW Russia',
    s => s.target.target.releaseSha256 = '9'.repeat(64), s => s.plan.currentCore.sha256 = '9'.repeat(64),
    s => s.plan.baseUrl = 'https://example.com', s => s.plan.baseUrl = 'http://user:secret@127.0.0.1:4199',
    s => s.plan.extra = true, s => s.target.target.mediaAnalysisGeneration = -1]) {
    const s = fixture('begin'); change(s); const h = harness([]);
    await assert.rejects(executeMaintenance(s, h)); assert.equal(h.calls.length, 0); assert.equal(h.records.size, 0);
  }
});
test('status epoch selection is explicit; mutation epoch override is refused', () => {
  const s = fixture();
  assert.equal(maintenanceRequest(s.plan, s.target, { action: 'status', invocationId: 'new', ownerEpoch: 8 }).owner.epoch, 8);
  for (const action of ['begin', 'register-target', 'checkpoint']) assert.throws(() => maintenanceRequest(s.plan, s.target, { action, invocationId: 'new', ownerEpoch: 8 }));
  assert.throws(() => maintenanceRequest(s.plan, s.target, { action: 'status', invocationId: 'new', ownerEpoch: 9 }));
});
test('checkpoint HTTP400 after commit settles only complete durable five-field transfer', async () => {
  const s = fixture(), h = harness([observation(s), observation(s, 'drained')], () => { throw { status: 400, code: 'HTTP_ERROR' }; });
  const result = await executeMaintenance(s, h);
  assert.equal(result.status, 'drained-readback'); assert.equal(result.acknowledgement, false);
  assert.deepEqual(result.transfer.queuedBacklog.jobs, [{ jobId: 'retained_paid_unknown', rowSha256: 'f'.repeat(64) }]);
  assert.equal(h.calls.filter(call => call.mutation).length, 1); assert.equal(result.stopAuthorized, false);
});
test('checkpoint success ACK is followed by exact durable status readback', async () => {
  const s = fixture(), h = harness([observation(s), observation(s, 'drained')], () => ({ owner: s.owner, transfer: {}, stopAuthorized: false }));
  assert.equal((await executeMaintenance(s, h)).acknowledgement, true);
  assert.equal(h.calls.length, 3); assert.ok(h.records.has('after.json'));
});
test('already drained checkpoint observes without reserving another mutation', async () => {
  const s = fixture(), h = harness([observation(s, 'drained')]);
  assert.equal((await executeMaintenance(s, h)).postDispatched, false); assert.equal(h.sharedDispatch.size, 0);
});
test('unready checkpoint does not consume logical mutation slot', async () => {
  const s = fixture(), before = observation(s); before.nativeUnresolved = 1;
  const h = harness([before]); assert.equal((await executeMaintenance(s, h)).status, 'held-not-dispatched');
  assert.equal(h.sharedDispatch.size, 0);
  const second = harness([observation(s), observation(s, 'drained')], () => ({}), h.sharedDispatch);
  assert.equal((await executeMaintenance({ ...s, invocationId: 'physical_02' }, second)).status, 'drained-readback');
  assert.equal(second.calls.filter(call => call.mutation).length, 1);
});
test('different target preflight cannot dispatch checkpoint', async () => {
  const s = fixture(), before = observation(s); before.lifecycle.target.attemptId = 'another';
  const h = harness([before]); await assert.rejects(executeMaintenance(s, h)); assert.equal(h.sharedDispatch.size, 0);
});
test('lost begin ACK remains UNKNOWN; another physical invocation cannot repeat logical POST', async () => {
  const s = fixture('begin'), next = { ...s.owner, epoch: 8 };
  const h = harness([observation(s, 'running'), observation(s, 'draining', next)], () => { throw { code: 'UNKNOWN_MUTATION_OUTCOME' }; });
  assert.equal((await executeMaintenance(s, h)).status, 'unresolved');
  assert.equal(h.calls.filter(call => call.mutation).length, 1); assert.deepEqual(h.calls[2].body, { owner: next });
  const second = harness([], undefined, h.sharedDispatch);
  await assert.rejects(executeMaintenance({ ...s, invocationId: 'physical_02' }, second), /already dispatched/); assert.equal(second.calls.length, 0);
  const status = maintenanceRequest(s.plan, s.target, { action: 'status', invocationId: 'readback_02', ownerEpoch: 8 });
  assert.equal((await executeMaintenance(status, harness([observation(status)], undefined, h.sharedDispatch))).status, 'observed');
});
test('registration timeout cannot be proven by ordinary status and never retries', async () => {
  const s = fixture('register-target'), h = harness([observation(s, 'running'), observation(s, 'running')], () => { throw { code: 'UNKNOWN_MUTATION_OUTCOME' }; });
  assert.equal((await executeMaintenance(s, h)).status, 'unresolved'); assert.equal(h.calls.length, 3);
});
test('malformed successful begin ACK is retained as UNKNOWN and prevents replay', async () => {
  const s = fixture('begin'), h = harness([observation(s, 'running'), observation(s)], () => ({ owner: s.owner, lifecycle: { owner: s.owner, phase: 'draining', target: {} }, stopAuthorized: false }));
  assert.equal((await executeMaintenance(s, h)).status, 'unresolved'); assert.ok(h.records.has('ack.json')); assert.equal(h.sharedDispatch.size, 1);
});
test('corrupt drained proofs never settle an uncertain checkpoint', async () => {
  for (const change of [v => delete v.lifecycle.transfer.queuedBacklog, v => v.owner.epoch++,
    v => v.lifecycle.transfer.target.attemptId = 'foreign', v => v.nativeUnresolved = 1,
    v => v.lifecycle.queuedBacklog.jobs.push(copy(v.lifecycle.queuedBacklog.jobs[0])), v => v.provider.containment = false]) {
    const s = fixture(), after = copy(observation(s, 'drained')); change(after);
    const h = harness([observation(s), after], () => { throw { code: 'UNKNOWN_MUTATION_OUTCOME' }; });
    assert.equal((await executeMaintenance(s, h)).status, 'unresolved'); assert.equal(h.calls.filter(call => call.mutation).length, 1);
  }
});
test('stop/resume and forced flags have no supported action', () => {
  const s = fixture(); for (const action of ['stop', 'resume', 'stop-checkpoint', 'kill']) assert.throws(() => maintenanceRequest(s.plan, s.target, { action, invocationId: 'x' }));
});
test('maintenance deadline defaults to 10 minutes and exact override reaches existing constructor', () => {
  const s = fixture(), options = [];
  class Client { constructor(value) { options.push(value); } }
  maintenanceClient(Client, s.plan); maintenanceClient(Client, s.plan, { timeoutMs: 1_800_000, cookie: 'session=fixture' });
  assert.equal(options[0].timeoutMs, 600_000); assert.equal(options[1].timeoutMs, 1_800_000);
  assert.equal(options[1].account, s.owner.account); assert.equal(options[1].baseUrl, s.plan.baseUrl);
  for (const timeoutMs of [0, 1_800_001, NaN, 1.5]) assert.throws(() => maintenanceClient(Client, s.plan, { timeoutMs }));
});
test('real existing client supplies CSRF and timeout mutation remains UNKNOWN through executor', async () => {
  const s = fixture('begin'), h = harness([]), wire = [];
  let statusCount = 0;
  const client = new CommunityHeroClient({ account: s.owner.account, baseUrl: s.plan.baseUrl, timeoutMs: 600_000,
    fetchImpl: async (url, options) => {
      wire.push({ path: url.pathname, ...options });
      if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'fixture-token' }));
      assert.equal(options.headers['x-csrf-token'], 'fixture-token'); assert.equal(options.headers.origin, s.plan.baseUrl);
      assert.ok(options.signal instanceof AbortSignal);
      if (url.pathname.endsWith('/begin')) throw Object.assign(new Error('not printed private fixture'), { name: 'TimeoutError' });
      const value = statusCount++ === 0 ? observation(s, 'running') : observation(s, 'draining', { ...s.owner, epoch: 8 });
      return new Response(JSON.stringify(value));
    } });
  assert.equal((await executeMaintenance(s, { ...h, client })).status, 'unresolved');
  assert.equal(wire.filter(call => call.path.endsWith('/begin')).length, 1);
  assert.equal(h.records.get('error.json').code, 'UNKNOWN_MUTATION_OUTCOME');
  assert.equal(JSON.stringify([...h.records]).includes('fixture-token'), false);
  assert.equal(wire.some(call => /bootstrap|engine\/status/.test(call.path)), false);
});

async function temporary(t) {
  const directory = await fs.mkdtemp(path.join(tmpdir(), 'communityhero-maintenance-test-'));
  t.after(() => fs.rm(directory, { recursive: true, force: true })); return directory;
}
async function filePin(file, value) {
  const bytes = Buffer.from(JSON.stringify(value)); await fs.mkdir(path.dirname(file), { recursive: true });
  await fs.writeFile(file, bytes, { flag: 'wx' }); return { path: file, sha256: sha(bytes) };
}
async function artifacts(t) {
  const directory = await temporary(t), s = fixture();
  const runtimeRoot = path.join(directory, 'runtime'); await fs.mkdir(path.join(runtimeRoot, 'cli'), { recursive: true });
  const bytes = Buffer.from('export class CommunityHeroClient {}\n');
  await fs.writeFile(path.join(runtimeRoot, 'cli/client.mjs'), bytes);
  await fs.writeFile(path.join(runtimeRoot, 'cli/extra.mjs'), 'retained asset');
  const core = { schemaVersion: 1, kind: 'company-independent-immutable-core', runtimeRoot, binarySha256: '2'.repeat(64),
    assets: [{ path: 'cli/client.mjs', sha256: sha(bytes) }, { path: 'cli/extra.mjs', sha256: sha('retained asset') }] };
  s.plan.currentCore = await filePin(path.join(directory, 'core.json'), core);
  s.plan.initialOwner.releaseSha256 = s.plan.currentCore.sha256;
  s.plan.currentState = await filePin(path.join(directory, 'state.json'), { account: 'likeavto', status: 'running', coreSha256: s.plan.currentCore.sha256, binarySha256: core.binarySha256 });
  s.plan.targetAdmission = await filePin(path.join(directory, 'target.json'), s.target);
  s.plan.journalDirectory = path.join(directory, 'journal');
  return { s, core, directory, planPin: await filePin(path.join(directory, 'plan.json'), s.plan) };
}
test('pinned config loads arbitrary release IDs and verifies all current CLI assets', async t => {
  const f = await artifacts(t); const loaded = await loadMaintenancePlan(f.planPin);
  assert.equal(loaded.plan.currentCore.sha256, f.s.plan.initialOwner.releaseSha256);
  await fs.writeFile(path.join(f.core.runtimeRoot, 'cli/extra.mjs'), 'changed');
  await assert.rejects(loadMaintenancePlan(f.planPin), /Current CLI asset changed/);
});
test('pinned target and current pointer drift refuse before import', async t => {
  const f = await artifacts(t); await fs.appendFile(f.s.plan.currentState.path, ' ');
  await assert.rejects(loadMaintenancePlan(f.planPin), /Artifact hash mismatch/);
});
test('native company pointer mismatch is rejected even with valid file hash', async t => {
  const f = await artifacts(t), state = JSON.parse(await fs.readFile(f.s.plan.currentState.path, 'utf8'));
  state.account = 'baw-russia';
  f.s.plan.currentState = await filePin(path.join(f.directory, 'foreign-state.json'), state);
  const planPin = await filePin(path.join(f.directory, 'foreign-plan.json'), f.s.plan);
  await assert.rejects(loadMaintenancePlan(planPin));
});
test('real journals fence different invocations while allowing read-only status', async t => {
  const directory = await temporary(t), s = fixture('begin'); s.plan.journalDirectory = path.join(directory, 'journal');
  const planPin = { path: path.join(directory, 'plan.json'), sha256: '3'.repeat(64) };
  const h = harness([observation(s, 'running'), observation(s)], () => { throw { code: 'UNKNOWN_MUTATION_OUTCOME' }; });
  await withMaintenanceJournal(s, planPin, journal => executeMaintenance(s, { client: h.client, ...journal }));
  const second = { ...s, invocationId: 'physical_02' }, noCalls = harness([]);
  await assert.rejects(withMaintenanceJournal(second, planPin, journal => executeMaintenance(second, { client: noCalls.client, ...journal })), /already dispatched/);
  assert.equal(noCalls.calls.length, 0);
  const status = maintenanceRequest(s.plan, s.target, { action: 'status', invocationId: 'status_03' });
  const readonly = harness([observation(status, 'running')]);
  assert.equal((await withMaintenanceJournal(status, planPin, journal => executeMaintenance(status, { client: readonly.client, ...journal }))).status, 'observed');
  assert.equal(JSON.parse(await fs.readFile(path.join(s.plan.journalDirectory, 'begin.dispatch.json'), 'utf8')).invocationId, 'physical_01');
  await assert.rejects(withMaintenanceJournal(s, planPin, () => assert.fail()), { code: 'EEXIST' });
});
test('active mutation lock refuses competing mutation but permits same-journal status', async t => {
  const directory = await temporary(t), s = fixture('begin'); s.plan.journalDirectory = path.join(directory, 'journal');
  const planPin = { path: path.join(directory, 'plan.json'), sha256: '3'.repeat(64) };
  let entered, release;
  const barrier = new Promise(resolve => { entered = resolve; }), held = new Promise(resolve => { release = resolve; });
  const running = withMaintenanceJournal(s, planPin, async () => { entered(); await held; }); await barrier;
  try {
    await assert.rejects(withMaintenanceJournal({ ...s, invocationId: 'contender' }, planPin, () => assert.fail()), { code: 'EEXIST' });
    const status = maintenanceRequest(s.plan, s.target, { action: 'status', invocationId: 'observation' });
    const h = harness([observation(status, 'running')]);
    assert.equal((await withMaintenanceJournal(status, planPin, journal => executeMaintenance(status, { client: h.client, ...journal }))).status, 'observed');
  } finally { release(); await running; }
});
test('changed plan cannot retarget existing logical journal', async t => {
  const directory = await temporary(t), s = fixture('status'); s.plan.journalDirectory = path.join(directory, 'journal');
  const planPin = { path: path.join(directory, 'plan.json'), sha256: '3'.repeat(64) };
  await withMaintenanceJournal(s, planPin, async () => {});
  await assert.rejects(withMaintenanceJournal({ ...s, invocationId: 'different' }, { ...planPin, sha256: '4'.repeat(64) }, () => assert.fail()));
});
test('durable dispatch failure refuses network call and preserves reserved ambiguity', async () => {
  const s = fixture('begin'), h = harness([observation(s, 'running')]);
  const write = h.journal;
  h.journal = async (name, value) => { if (name === 'dispatch.json') throw { code: 'ENOSPC' }; return write(name, value); };
  await assert.rejects(executeMaintenance(s, h));
  assert.equal(h.calls.filter(call => call.mutation).length, 0); assert.equal(h.sharedDispatch.size, 1);
  const second = harness([], undefined, h.sharedDispatch);
  await assert.rejects(executeMaintenance({ ...s, invocationId: 'new_physical' }, second), /already dispatched/);
  assert.equal(second.calls.length, 0);
});
test('failed logical dispatch reservation cannot make an HTTP mutation', async () => {
  const s = fixture('begin'), h = harness([observation(s, 'running')]);
  h.reserveDispatch = async () => { throw { code: 'ENOSPC' }; };
  await assert.rejects(executeMaintenance(s, h)); assert.equal(h.calls.filter(call => call.mutation).length, 0);
});
test('BAW uses its own native owner with the same release-neutral command', async () => {
  const s = fixture('begin', 'baw-russia'); s.plan.baseUrl = 'http://127.0.0.1:4208';
  const h = harness([observation(s, 'running')], () => nativeBeginAck(s));
  const result = await executeMaintenance(s, h);
  assert.equal(result.owner.account, 'BAW Russia');
  assert.equal(h.sharedDispatch.get('begin').account, 'baw-russia');
  assert.equal(h.sharedDispatch.get('begin').body.owner.account, 'BAW Russia');
});

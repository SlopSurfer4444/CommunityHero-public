import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { runQueue } from '../cli/queue.mjs';
import { writeCheckpoint } from '../cli/workflow.mjs';
import { CliError, UnknownMutationError } from '../cli/client.mjs';

const suppressed = { imported: 0, authority: 'communityhero', legacyImportSuppressed: true };
const policy = () => ({ account: 'LikeAvto', companyKnowledgeAuthority: {
  owner: 'communityhero', account: 'LikeAvto', companyKey: 'likeavto'
}, materials: [{ account: 'LikeAvto', companyKnowledge: true, kind: 'rule', text: 'Synthetic company rule' }] });
const read = async file => JSON.parse(await readFile(file, 'utf8'));
async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-queue-materials-'));
  t.after(async () => {
    assert.ok(resolve(dir).startsWith(resolve(tmpdir()) + sep));
    assert.ok(dir.includes('communityhero-queue-materials-'));
    await rm(dir, { recursive: true, force: true });
  });
  const file = join(dir, 'queue.json');
  let imports = 0;
  const client = { account: 'likeavto', baseUrl: 'http://127.0.0.1:4186',
    importMaterials: async () => { imports++; return suppressed; }, bootstrap: async () => policy() };
  return { file, client, imports: () => imports,
    start: () => runQueue(client, { checkpointPath: file, maxCycles: 0 }),
    resume: () => runQueue(client, { resumePath: file, maxCycles: 0 }) };
}

test('materials read timeout retains exact safe cause and resumes observation without another import', async t => {
  const f = await fixture(t);
  f.client.bootstrap = async () => { throw new CliError('SECRET server body', { code: 'NETWORK_ERROR',
    details: { cause: 'TimeoutError', causeName: 'TimeoutError', timeoutMs: 120000, body: 'SECRET' } }); };
  await assert.rejects(f.start(), { code: 'NETWORK_ERROR' });
  const saved = await read(f.file);
  assert.equal(saved.phase, 'stopped'); assert.equal(saved.stopReason, 'materials-unavailable');
  assert.deepEqual(saved.materialsImport, suppressed); assert.equal(saved.pendingMaterialsImport, null);
  assert.equal(saved.materialsReady, undefined);
  assert.deepEqual(saved.error.details, { stage: 'policy-verification', cause: 'TimeoutError', causeName: 'TimeoutError', timeoutMs: 120000 });
  assert.equal(saved.error.code, 'NETWORK_ERROR'); assert.ok(!JSON.stringify(saved).includes('SECRET'));
  f.client.bootstrap = async () => policy();
  await f.resume(); assert.equal(f.imports(), 1);
  const recovered = await read(f.file); assert.equal(recovered.materialsReady, true); assert.equal(recovered.error, null);
});

test('materials policy absence stays a policy failure and never becomes ready', async t => {
  const f = await fixture(t);
  f.client.bootstrap = async () => ({ ...policy(), materials: [] });
  await assert.rejects(f.start(), { code: 'MATERIALS_UNAVAILABLE' });
  const saved = await read(f.file);
  assert.equal(saved.error.code, 'MATERIALS_UNAVAILABLE'); assert.equal(saved.error.details.stage, 'policy-verification');
  assert.equal(saved.materialsReady, undefined); assert.equal(f.imports(), 1);
});

test('materials read HTTP rejection preserves status without serializing server error bodies', async t => {
  const f = await fixture(t);
  f.client.bootstrap = async () => { throw new CliError('SECRET', { code: 'HTTP_ERROR', status: 500,
    details: { cause: 'SECRET', causeName: 'SECRET', body: 'SECRET', timeoutMs: 9000000 } }); };
  await assert.rejects(f.start(), { code: 'HTTP_ERROR' });
  const saved = await read(f.file);
  assert.equal(saved.error.status, 500); assert.deepEqual(saved.error.details, { stage: 'policy-verification' });
  assert.ok(!JSON.stringify(saved).includes('SECRET'));
});

test('materials import records intent before POST and crash checkpoint cannot repeat admission', async t => {
  const f = await fixture(t); let inFlight;
  f.client.importMaterials = async () => { inFlight = await read(f.file); return suppressed; };
  await f.start();
  assert.equal(inFlight.phase, 'materials-admitting');
  assert.equal(inFlight.pendingMaterialsImport.path, '/api/materials/import');
  assert.equal(inFlight.pendingMaterialsImport.method, 'POST');
  assert.ok(Number.isFinite(Date.parse(inFlight.pendingMaterialsImport.recordedAt)));
  assert.equal((await read(f.file)).pendingMaterialsImport, null);
  await writeCheckpoint(f.file, inFlight); // Reproduce the last durable bytes at a crash before ACK.
  f.client.importMaterials = async () => { assert.fail('must not repeat an unresolved POST'); };
  f.client.bootstrap = async () => { assert.fail('must not infer an admission from unrelated reads'); };
  await assert.rejects(f.resume(), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const saved = await read(f.file); assert.equal(saved.phase, 'unknown');
  assert.equal(saved.stopReason, 'materials-import-launch-unresolved');
  assert.deepEqual(saved.pendingMaterialsImport, inFlight.pendingMaterialsImport);
});

test('unknown materials admission preserves intent and safe diagnostics without resubmission', async t => {
  const f = await fixture(t); let posts = 0;
  f.client.importMaterials = async () => { posts++; throw new UnknownMutationError('POST', '/api/materials/import',
    { name: 'TimeoutError', message: 'SECRET' }, { phase: 'request-headers', timeoutMs: 120000 }); };
  await assert.rejects(f.start(), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const saved = await read(f.file);
  assert.equal(saved.phase, 'unknown'); assert.ok(saved.pendingMaterialsImport);
  assert.equal(saved.error.details.stage, 'import-admission'); assert.equal(saved.error.details.timeoutMs, 120000);
  assert.ok(!JSON.stringify(saved).includes('SECRET'));
  await assert.rejects(f.resume(), { code: 'UNKNOWN_MUTATION_OUTCOME' }); assert.equal(posts, 1);
});

test('explicit import rejection records terminal rejection and permits ordinary later retry', async t => {
  const f = await fixture(t);
  f.client.importMaterials = async () => { throw new CliError('SECRET', { code: 'HTTP_ERROR', status: 403 }); };
  await assert.rejects(f.start(), { code: 'HTTP_ERROR' });
  const saved = await read(f.file);
  assert.equal(saved.pendingMaterialsImport, null); assert.equal(saved.phase, 'stopped');
  assert.equal(saved.error.status, 403); assert.equal(saved.error.details.stage, 'import-admission');
  f.client.importMaterials = async () => suppressed;
  await f.resume(); assert.equal((await read(f.file)).materialsReady, true);
});

test('accepted asynchronous import retains original job when its observation fails', async t => {
  const f = await fixture(t); let posts = 0; const jobs = [];
  f.client.importMaterials = async () => { posts++; return { jobId: 'original-materials-job' }; };
  f.client.getJob = async id => { jobs.push(id); throw new CliError('SECRET', { code: 'INVALID_RESPONSE', status: 502 }); };
  await assert.rejects(f.start(), { code: 'INVALID_RESPONSE' });
  const saved = await read(f.file);
  assert.equal(saved.materialsJobId, 'original-materials-job'); assert.equal(saved.pendingMaterialsImport, null);
  f.client.getJob = async id => { jobs.push(id); return { id, status: 'completed' }; };
  await f.resume(); assert.equal(posts, 1); assert.deepEqual(jobs, ['original-materials-job', 'original-materials-job']);
});

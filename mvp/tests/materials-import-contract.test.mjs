import test from 'node:test';
import assert from 'node:assert/strict';
import { CommunityHeroClient, hasAccountPolicy, materialsImportResult, settleMaterialsImport } from '../cli/client.mjs';

const suppressed = { imported: 0, authority: 'communityhero', legacyImportSuppressed: true };
const policy = () => ({ account: 'BAW Russia', companyKnowledgeAuthority: { owner: 'communityhero', account: 'BAW Russia', companyKey: 'baw-russia' },
  materials: [{ account: 'BAW Russia', companyKnowledge: true, kind: 'rule', text: 'Synthetic policy' }] });

test('materials import accepts exactly asynchronous job or canonical synchronous suppression', () => {
  for (const value of [{ jobId: 'job-1' }, suppressed]) assert.equal(materialsImportResult(value), value);
  for (const value of [null, [], {}, 'job-1', { jobId: null }, { jobId: 1 }, { jobId: '' }, { jobId: ' ' }, { jobId: ' job' }, { jobId: 'job\n' },
    { jobId: 'job', extra: true }, { ...suppressed, jobId: 'job' }, { ...suppressed, imported: 1 }, { ...suppressed, imported: '0' },
    { ...suppressed, authority: 'provider' }, { ...suppressed, legacyImportSuppressed: 'true' }, { ...suppressed, legacyImportSuppressed: false },
    { imported: 0, authority: 'communityhero' }, { ...suppressed, extra: true }]) {
    assert.throws(() => materialsImportResult(value), error => error.code === 'UNKNOWN_MUTATION_OUTCOME' && error.details.cause === 'INVALID_MATERIALS_IMPORT_RESPONSE');
  }
});

test('canonical policy presence remains account-bound and does not accept facts, blank text or a marker alone', () => {
  assert(hasAccountPolicy(policy(), true));
  const legacy = { account: 'BAW Russia', materials: [{ kind: 'knowledge', imported: true }] };
  assert(hasAccountPolicy(legacy)); assert.equal(hasAccountPolicy(legacy, true), false);
  for (const mutate of [p => { p.materials = []; }, p => { p.materials[0].account = 'LikeAvto'; }, p => { p.materials[0].kind = 'reference'; },
    p => { p.materials[0].text = ' '; }, p => { p.materials[0].companyKnowledge = false; }, p => { p.companyKnowledgeAuthority.owner = 'provider'; },
    p => { p.companyKnowledgeAuthority.account = 'LikeAvto'; }, p => { p.companyKnowledgeAuthority.companyKey = 'likeavto'; }]) {
    const p = policy(); mutate(p); assert.equal(hasAccountPolicy(p, true), false);
  }
});

test('settling synchronous import verifies bootstrap policy and never requests a job', async () => {
  let reads = 0;
  const client = { bootstrap: async () => { reads++; return policy(); }, getJob: () => { throw Error('must not poll'); } };
  await settleMaterialsImport(client, suppressed); assert.equal(reads, 1);
  await assert.rejects(settleMaterialsImport({ ...client, bootstrap: async () => ({ account: 'BAW Russia', materials: [] }) }, suppressed), { code: 'MATERIALS_UNAVAILABLE' });
});

test('malformed successful mutation is rejected once before any job or follow-up request', async () => {
  const calls = [];
  const client = new CommunityHeroClient({ account: 'baw-russia', fetchImpl: async (url, options) => {
    calls.push([url.pathname, options.method]);
    const payload = url.pathname === '/api/engine/status' ? { account: 'baw-russia', authority: 'shared-rust-engine' } : url.pathname === '/api/session' ? { csrfToken: 'synthetic-csrf' } : { imported: 0 };
    return new Response(JSON.stringify(payload), { status: 200 });
  } });
  await assert.rejects(client.importMaterials(), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.deepEqual(calls, [['/api/engine/status', 'GET'], ['/api/session', 'GET'], ['/api/materials/import', 'POST']]);
});

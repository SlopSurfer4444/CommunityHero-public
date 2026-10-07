import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CommunityHeroClient, UnknownMutationError, executeRejection, recoverExecuteAdmission, localAdmissionPayloadHash } from '../cli/client.mjs';
import { runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';

async function fixture(t) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-admission-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  return join(dir, 'checkpoint.json');
}
const refs = [{ id: 'proposal-1', revision: 1, itemId: 'item-1', kind: 'close', text: '' }];
const prepared = { account: 'BAW Russia', baseUrl: 'http://localhost:4186', phase: 'prepared', itemIds: ['item-1'],
  instruction: 'prepare', materialsReady: true, prepareJobId: 'prepare-job', prepareTransport: 'engine', proposals: refs,
  approvedProposals: [{ id: 'proposal-1', revision: 1 }] };
const hashFor = kind => localAdmissionPayloadHash(kind === 'prepare' ? { itemIds: prepared.itemIds, instruction: prepared.instruction }
  : { proposals: prepared.approvedProposals });
function client(overrides = {}) {
  const editorialJobs = new Map();
  return { account: prepared.account, baseUrl: prepared.baseUrl,
    engineStatus: async () => ({ strictGrouping: { version: 1, contract: 'strict_post_family_v1' } }),
    requireStrictPreparation: CommunityHeroClient.prototype.requireStrictPreparation,
    reviewItems: async () => ({ items: [{ id: 'item-1' }], proposals: refs.map(row => ({ ...row, status: 'draft', prepareRunId: 'prepare-job' })),
      operations: [], coverage: { operationsComplete: true } }),
    editorialReview: async (proposals, requestId) => {
      const jobId = `editorial-${requestId}`;
      editorialJobs.set(jobId, { id: jobId, kind: 'editorial_review', refId: requestId, status: 'completed', editorialOutcome: { accepted: proposals, reused: proposals, held: [] } });
      return { jobId, requestId, replayed: false };
    }, ...overrides,
    getJob: async id => editorialJobs.get(id) || (overrides.getJob ? overrides.getJob(id) : { id, status: 'completed' }) };
}

const negative = () => ({ kind: 'execute', status: 'rejected_local', viewStatus: 'waiting_dependency', account: 'BAW Russia',
  approvalId: 'approval-1', requestId: 'exact-execute-key', payloadHash: localAdmissionPayloadHash({ approvalId: 'approval-1' }),
  requestHash: 'c'.repeat(64), evaluationId: 'initial-evaluation', receiptSha256: 'd'.repeat(64),
  connectionBinding: { id: 'binding', workspaceId: 'workspace', accountId: 'baw-russia', connector: 'native-fixture', providerAccountId: 'external-account', revision: 1 },
  gateEpoch: 1, reason: 'waiting_dependency', blockingJobIds: ['original-job'], result: null, reevaluationAvailable: true, retryAuthorized: false,
  noAttemptProof: { executeJobCreated: false, operationCreated: false, approvalConsumed: false, providerDispatchArmed: false } });

test('committed negative execute receipt remains an exact known rejection without any new POST or job lookup', async () => {
  const receipt = negative(); let reads = 0;
  const api = { account: 'BAW Russia', localAdmission: async (kind, key) => { assert.equal(kind, 'execute'); assert.equal(key, receipt.requestId); reads++; return receipt; },
    execute: () => assert.fail('A rejected key does not auto re-evaluate'), getJob: () => assert.fail('No execute job was created') };
  await assert.rejects(recoverExecuteAdmission(api, receipt), error => error.code === 'REJECTED_LOCAL_ADMISSION'
    && error.details.receiptSha256 === receipt.receiptSha256);
  assert.equal(reads, 1);
});

for (const mutate of [row => { row.noAttemptProof.operationCreated = true; }, row => { row.approvalId = 'other'; },
  row => { row.payloadHash = '0'.repeat(64); }, row => { row.status = 'pending_or_unknown'; }, row => { row.retryAuthorized = true; }]) {
  test(`negative receipt cannot authorize re-evaluation with altered proof ${mutate.toString()}`, () => {
    const receipt = negative(); mutate(receipt);
    assert.throws(() => executeRejection(receipt, { approvalId: 'approval-1', requestId: 'exact-execute-key',
      payloadHash: localAdmissionPayloadHash({ approvalId: 'approval-1' }), account: 'BAW Russia' }), { code: 'INVALID_LOCAL_REJECTION' });
  });
}

test('client HTTP409 obtains no-attempt proof from exact durable GET and never reads private text as authority', async () => {
  const paths = []; const receipt = negative();
  const api = new CommunityHeroClient({ account: 'BAW Russia', fetchImpl: async (url, options) => {
    paths.push([url.pathname, options.method]);
    if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'synthetic-csrf' }));
    if (url.pathname === '/api/engine/status') return new Response(JSON.stringify({ account: 'BAW Russia' }));
    if (url.pathname.includes('/local-admissions/')) return new Response(JSON.stringify(receipt));
    return new Response(JSON.stringify({ error: 'private-rejection-body-canary' }), { status: 409 });
  } });
  await assert.rejects(api.execute('approval-1', 'exact-execute-key'), error => error.code === 'REJECTED_LOCAL_ADMISSION'
    && !error.message.includes('private-rejection-body-canary'));
  assert.equal(paths.filter(([path, method]) => path.endsWith('/execute') && method === 'POST').length, 1);
  assert.equal(paths.filter(([path]) => path.includes('/local-admissions/')).length, 1);
});

for (const lineage of ['exact','missing','wrong']) test(`explicit re-evaluation ${lineage} parent proof controls a new durable negative without another POST`,async()=>{
  const initial=negative();const latest={...initial,evaluationId:'second-evaluation',receiptSha256:'e'.repeat(64),
    ...(lineage==='missing'?{}:{parentEvaluationId:lineage==='wrong'?'foreign-evaluation':initial.evaluationId,parentReceiptSha256:initial.receiptSha256})};
  let posts=0;let lookups=0;
  const api=new CommunityHeroClient({account:'BAW Russia',fetchImpl:async(url,options)=>{
    if(url.pathname==='/api/session')return new Response(JSON.stringify({csrfToken:'fixture'}));
    if(url.pathname==='/api/engine/status')return new Response(JSON.stringify({account:'BAW Russia'}));
    if(options.method==='POST'){posts++;assert.deepEqual(JSON.parse(options.body),{requestId:initial.requestId,reevaluate:{evaluationId:initial.evaluationId,receiptSha256:initial.receiptSha256}});return new Response(JSON.stringify({error:'private-canary'}),{status:409});}
    lookups++;return new Response(JSON.stringify(latest));
  }});
  await assert.rejects(api.execute(initial.approvalId,initial.requestId,{reevaluate:{evaluationId:initial.evaluationId,receiptSha256:initial.receiptSha256}}),
    error=>error.code===(lineage==='exact'?'REJECTED_LOCAL_ADMISSION':'UNKNOWN_MUTATION_OUTCOME')
      &&(lineage!=='exact'||error.details.evaluationId===latest.evaluationId));
  assert.equal(posts,1);assert.equal(lookups,1);
});

test('a committed original execute wins a re-evaluation rejection race and only its exact job is observed',async()=>{
  const receipt=negative();let posts=0;const jobs=[];
  const api=new CommunityHeroClient({account:'BAW Russia',fetchImpl:async(url,options)=>{
    if(url.pathname==='/api/session')return new Response(JSON.stringify({csrfToken:'fixture'}));
    if(url.pathname==='/api/engine/status')return new Response(JSON.stringify({account:'BAW Russia'}));
    if(options.method==='POST'){posts++;return new Response('{}',{status:409});}
    if(url.pathname.includes('/local-admissions/'))return new Response(JSON.stringify({...receipt,status:'committed',result:{jobId:'original-job',approvalId:receipt.approvalId,requestId:receipt.requestId,replayed:true}}));
    jobs.push(url.pathname);return new Response(JSON.stringify({id:'original-job',kind:'execute',refId:receipt.approvalId,status:'running'}));
  }});
  const result=await api.execute(receipt.approvalId,receipt.requestId,{reevaluate:{evaluationId:receipt.evaluationId,receiptSha256:receipt.receiptSha256}});
  assert.equal(result.jobId,'original-job');assert.equal(posts,1);assert.deepEqual(jobs,['/api/engine/jobs/original-job']);
});

test('client explicit re-evaluation preserves original key and approval while sending only exact control proof', async () => {
  let body;
  const api = new CommunityHeroClient({ account: 'BAW Russia', fetchImpl: async (url, options) => {
    if (options.method === 'POST') { body = JSON.parse(options.body); return new Response(JSON.stringify({ jobId: 'execute-job', approvalId: 'approval-1', requestId: body.requestId, replayed: false })); }
    return new Response(JSON.stringify(url.pathname === '/api/session' ? { csrfToken: 'synthetic' } : { account: 'BAW Russia' }));
  } });
  const reevaluate = { evaluationId: negative().evaluationId, receiptSha256: negative().receiptSha256 };
  await api.execute('approval-1', 'exact-execute-key', { reevaluate });
  assert.deepEqual(body, { requestId: 'exact-execute-key', reevaluate });
});

test('HTTP409 with an absent rejection receipt remains UNKNOWN and never substitutes a new key', async () => {
  let posts = 0;
  const api = new CommunityHeroClient({ account: 'BAW Russia', fetchImpl: async (url, options) => {
    if (options.method === 'POST') { posts++; return new Response(JSON.stringify({ error: 'rejected' }), { status: 409 }); }
    if (url.pathname.includes('/local-admissions/')) return new Response(JSON.stringify({ status: 'pending_or_unknown' }));
    return new Response(JSON.stringify(url.pathname === '/api/session' ? { csrfToken: 'synthetic' } : { account: 'BAW Russia' }));
  } });
  await assert.rejects(api.execute('approval-1', 'exact-execute-key'), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal(posts, 1);
});

for (const corruption of ['approval', 'recipient', 'missing-binding', 'operation-identity']) {
  test(`workflow terminal outcome rejects ${corruption} without another effect`, async t => {
    const path = await fixture(t);
    const prior = corruption === 'operation-identity' ? [{ id: 'original-op', proposalId: 'proposal-1', status: 'unknown' }] : [];
    await writeCheckpoint(path, { ...prepared, phase: 'executing', approvalId: 'approval-1', executeJobId: 'original-execute', operations: prior });
    const operation = { id: 'observed-op', approvalId: 'approval-1', itemId: 'item-1', proposalId: 'proposal-1', status: 'succeeded' };
    if (corruption === 'approval') operation.approvalId = 'foreign';
    if (corruption === 'recipient') operation.itemId = 'other';
    if (corruption === 'missing-binding') delete operation.approvalId;
    const api = client({ createApproval: () => assert.fail('No approval'), execute: () => assert.fail('No execute retry'),
      getJob: async id => ({ id, kind: 'execute', refId: 'approval-1', status: 'completed' }),
      reviewItems: async () => ({ operations: [operation], coverage: { operationsComplete: true } }) });
    const result = await runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true, pollMs: 0 });
    assert.equal(result.mode, 'needs-reconciliation'); assert.deepEqual(result.checkpoint.operationEvidence.invalidBindingOperationIds, ['observed-op']);
    assert.equal(result.checkpoint.executeJobId, 'original-execute');
  });
}
const unknown = path => new UnknownMutationError('POST', path, { name: 'TimeoutError' });

for (const strictGrouping of [undefined, { version: 2, contract: 'strict_post_family_v1' }, { version: 1, contract: 'unknown' }])
test(`new preparation refuses unverified strict contract ${JSON.stringify(strictGrouping)} before admission`, async t => {
  const path = await fixture(t);
  const api = client({ engineStatus: async () => ({ strictGrouping }),
    reviewItems: () => assert.fail('capability refusal precedes preparation reads'),
    prepareEngine: () => assert.fail('unverified capability must not admit generation'),
    createConversation: () => assert.fail('strict preparation has no legacy fallback') });
  await assert.rejects(runWorkflow(api, ['item-1'], { checkpointPath: path,
    materialsAlreadyRefreshed: true, planAlreadyChecked: true }), { code: 'STRICT_GROUPING_REQUIRED' });
  const saved = JSON.parse(await readFile(path, 'utf8'));
  assert.equal(saved.prepareJobId, undefined);
  assert.equal(saved.pendingLocalAdmission, undefined);
});

test('prepare persists request key before HTTP and recovers committed admission without another POST', async t => {
  const path = await fixture(t); let posts = 0; let key; let payloadHash;
  const api = client({ prepareEngine: async body => {
    posts += 1; const saved = JSON.parse(await readFile(path, 'utf8'));
    key = body.requestId; assert.ok(key); assert.equal(saved.prepareRequestId, key);
    payloadHash = localAdmissionPayloadHash(body);
    assert.deepEqual(saved.pendingLocalAdmission, { kind: 'prepare', requestId: key, payloadHash });
    throw unknown('/api/engine/prepare');
  } });
  await assert.rejects(runWorkflow(api, ['item-1'], { checkpointPath: path, materialsAlreadyRefreshed: true, planAlreadyChecked: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const order = [];
  api.localAdmission = async (kind, requestId) => { order.push('receipt'); assert.equal(kind, 'prepare'); assert.equal(requestId, key);
    return { kind, requestId, payloadHash, status: 'committed', result: { jobId: 'prepare-job', requestId, replayed: true } }; };
  api.reviewItems = async () => { order.push('review'); return { items: [{ id: 'item-1' }], proposals: refs.map(row => ({ ...row, status: 'draft', prepareRunId: 'prepare-job' })) }; };
  const result = await runWorkflow(api, [], { resumePath: path });
  assert.equal(result.checkpoint.phase, 'prepared'); assert.equal(result.checkpoint.prepareJobId, 'prepare-job');
  assert.equal(result.checkpoint.pendingLocalAdmission, null); assert.equal(posts, 1); assert.equal(order[0], 'receipt');
});

test('approval persists key and exact refs before HTTP, then binds committed result without approving again', async t => {
  const path = await fixture(t); await writeCheckpoint(path, prepared); let posts = 0; let key;
  const api = client({ createApproval: async (exact, requestId) => {
    posts += 1; key = requestId; const saved = JSON.parse(await readFile(path, 'utf8'));
    assert.ok(key); assert.equal(saved.approvalRequestId, key); assert.deepEqual(saved.approvedProposals, exact);
    assert.equal(saved.phase, 'approval-admitting'); throw unknown('/api/approvals');
  } });
  await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  api.localAdmission = async (kind, requestId) => ({ kind, requestId, payloadHash: hashFor(kind), status: 'committed', result: { id: 'approval-1', requestId, replayed: true } });
  const result = await runWorkflow(api, [], { resumePath: path, autonomous: true });
  assert.equal(result.mode, 'approved'); assert.equal(result.checkpoint.approvalId, 'approval-1'); assert.equal(posts, 1);
  assert.equal(result.checkpoint.approvalRequestId, key);
});

for (const kind of ['prepare', 'approval']) {
  for (const phase of ['unknown', `${kind}-admitting`]) {
    test(`${kind} ${phase}: pending/absent receipt never permits a POST`, async t => {
      const path = await fixture(t); let reads = 0;
      await writeCheckpoint(path, { ...prepared, phase, ...(kind === 'prepare' ? { prepareJobId: null } : {}),
        [`${kind}RequestId`]: 'request-1', pendingLocalAdmission: { kind, requestId: 'request-1', payloadHash: hashFor(kind) } });
      const api = client({ localAdmission: async () => { reads += 1; return { kind, requestId: 'request-1', status: 'pending_or_unknown', result: null }; },
        prepareEngine: () => assert.fail('prepare must not be repeated'), createApproval: () => assert.fail('approval must not be repeated'),
        execute: () => assert.fail('execute must not start'), reviewItems: () => assert.fail('receipt inspection must happen first') });
      await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
      assert.equal(reads, 1); assert.equal(JSON.parse(await readFile(path, 'utf8')).phase, 'unknown');
    });
  }
}

test('misbound committed receipt and unavailable receipt remain UNKNOWN', async t => {
  const path = await fixture(t);
  for (const localAdmission of [async () => ({ kind: 'approval', requestId: 'other', payloadHash: hashFor('approval'), status: 'committed', result: { id: 'wrong' } }),
    async () => { throw new Error('private response'); },
    async () => ({ kind: 'approval', requestId: 'key', payloadHash: hashFor('approval'), status: 'committed', result: {} })]) {
    await writeCheckpoint(path, { ...prepared, phase: 'unknown', approvalRequestId: 'key', pendingLocalAdmission: { kind: 'approval', requestId: 'key', payloadHash: hashFor('approval') } });
    await assert.rejects(runWorkflow(client({ localAdmission }), [], { resumePath: path, autonomous: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
    assert.doesNotMatch(await readFile(path, 'utf8'), /private response/);
  }
});

test('legacy and external execution UNKNOWN retain hard stop even when admission keys exist', async t => {
  const path = await fixture(t); let calls = 0;
  for (const extra of [{}, { approvalRequestId: 'old', approvalId: 'approval-1' },
    { approvalRequestId: 'key', pendingLocalAdmission: { kind: 'approval', requestId: 'key' }, error: { details: { path: '/api/approvals/approval-1/execute' } } }]) {
    await writeCheckpoint(path, { ...prepared, phase: 'unknown', ...extra });
    await assert.rejects(runWorkflow(client({ localAdmission: async () => { calls += 1; }, execute: () => assert.fail('no execute retry') }), [],
      { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  }
  assert.equal(calls, 0);
});

test('recovered local approval executes once and a missing execute receipt stays UNKNOWN', async t => {
  const path = await fixture(t); let executes = 0; let reads = 0;
  await writeCheckpoint(path, { ...prepared, phase: 'approval-admitting', approvalRequestId: 'key',
    pendingLocalAdmission: { kind: 'approval', requestId: 'key', payloadHash: hashFor('approval') } });
  const api = client({ localAdmission: async (kind, requestId) => { reads += 1;
    return { kind, requestId, payloadHash: hashFor(kind), status: 'committed', result: { id: 'approval-1' } }; },
    createApproval: () => assert.fail('no new approval'),
    execute: async approvalId => { assert.equal(approvalId, 'approval-1'); executes += 1; throw unknown('/api/approvals/approval-1/execute'); } });
  await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const saved = JSON.parse(await readFile(path, 'utf8'));
  assert.equal(saved.pendingLocalAdmission.kind, 'execute'); assert.equal(saved.approvalId, 'approval-1'); assert.equal(saved.phase, 'unknown');
  await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal(reads, 2); assert.equal(executes, 1);
});

test('direct client optional admission keys preserve legacy payloads and use a read-only receipt route', async () => {
  const requests = [];
  const api = new CommunityHeroClient({ account: 'BAW Russia', fetchImpl: async (url, options) => {
    requests.push({ path: url.pathname, method: options.method, body: options.body && JSON.parse(options.body) });
    return new Response(JSON.stringify(url.pathname === '/api/session' ? { csrfToken: 'csrf' } : { account: 'BAW Russia' }), { status: 200 });
  } });
  await api.prepareEngine({ itemIds: ['item-1'] }); await api.prepareEngine({ itemIds: ['item-1'] }, 'prepare-key');
  await api.createApproval([{ id: 'p', revision: 1 }]); await api.createApproval([{ id: 'p', revision: 1 }], 'approval-key');
  await api.execute('approval-1'); await api.execute('approval-1', 'execute-key');
  await api.localAdmission('approval', 'approval-key');
  const posts = requests.filter(row => row.method === 'POST');
  assert.deepEqual(posts.map(row => row.body.requestId), [undefined, 'prepare-key', undefined, 'approval-key', undefined, 'execute-key']);
  assert.deepEqual(posts.at(-1).body, { requestId: 'execute-key' });
  assert.equal(requests.at(-1).method, 'GET'); assert.equal(requests.at(-1).path, '/api/local-admissions/approval/approval-key');
  await api.localAdmission('execute', 'execute-key');
  assert.equal(requests.at(-1).method, 'GET'); assert.equal(requests.at(-1).path, '/api/local-admissions/execute/execute-key');
});

test('workflow saves execute key and approval hash before POST and resumes exact receipt without reposting', async t => {
  const path = await fixture(t); await writeCheckpoint(path, { ...prepared, approvalId: 'approval-1', phase: 'approved' });
  let posts = 0; let receipt;
  const api = client({ execute: async (approvalId, requestId) => {
    posts += 1;
    const state = JSON.parse(await readFile(path, 'utf8'));
    assert.equal(state.executeRequestId, requestId);
    assert.deepEqual(state.pendingLocalAdmission, { kind: 'execute', requestId, payloadHash: localAdmissionPayloadHash({ approvalId }) });
    receipt = { ...state.pendingLocalAdmission, status: 'committed', result: { jobId: 'execute-job', approvalId, requestId, replayed: false } };
    throw unknown('/api/approvals/approval-1/execute');
  }, localAdmission: async (kind, key) => { assert.equal(kind, 'execute'); assert.equal(key, receipt.requestId); return receipt; },
  getJob: async id => ({ id, status: 'completed', kind: 'execute', refId: 'approval-1' }) });
  await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  const result = await runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true });
  assert.equal(result.checkpoint.executeJobId, 'execute-job'); assert.equal(result.checkpoint.pendingLocalAdmission, null);
  assert.equal(posts, 1); assert.equal(result.mode, 'needs-reconciliation'); // Receipt cannot invent operation outcomes.
});

for (const corruption of ['missing', 'kind', 'key', 'hash', 'result-key', 'result-approval', 'job-kind', 'job-ref', 'job-id', 'missing-job', 'checkpoint-approval']) {
  test(`execute recovery rejects ${corruption} without another POST or unrelated job polling`, async t => {
    const path = await fixture(t); let reads = 0; let jobs = 0;
    const payloadHash = localAdmissionPayloadHash({ approvalId: 'approval-1' });
    await writeCheckpoint(path, { ...prepared, phase: 'unknown', approvalId: corruption === 'checkpoint-approval' ? 'other' : 'approval-1',
      executeRequestId: 'execute-key', pendingLocalAdmission: { kind: 'execute', requestId: 'execute-key', payloadHash } });
    const receipt = { kind: 'execute', requestId: 'execute-key', payloadHash, status: 'committed',
      result: { jobId: 'execute-job', approvalId: 'approval-1', requestId: 'execute-key', replayed: false } };
    if (corruption === 'kind') receipt.kind = 'approval';
    if (corruption === 'key') receipt.requestId = 'other-key';
    if (corruption === 'hash') receipt.payloadHash = '0'.repeat(64);
    if (corruption === 'result-key') receipt.result.requestId = 'other-key';
    if (corruption === 'result-approval') receipt.result.approvalId = 'other';
    const api = client({ execute: () => assert.fail('no repeat execute'), reviewItems: () => assert.fail('no review before exact receipt'),
      localAdmission: async () => { reads += 1; return corruption === 'missing' ? { status: 'pending_or_unknown' } : receipt; },
      getJob: async () => { jobs += 1; return corruption === 'missing-job' ? null : { id: corruption === 'job-id' ? 'other-job' : 'execute-job',
        kind: corruption === 'job-kind' ? 'prepare' : 'execute', refId: corruption === 'job-ref' ? 'other' : 'approval-1', status: 'completed' }; } });
    await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
    assert.equal(reads, corruption === 'checkpoint-approval' ? 0 : 1);
    assert.equal(jobs, ['job-kind', 'job-ref', 'job-id', 'missing-job'].includes(corruption) ? 1 : 0);
    assert.equal(JSON.parse(await readFile(path, 'utf8')).phase, 'unknown');
  });
}

test('canonical payload hash sorts nested keys, preserves Unicode and array order, and omits undefined fields', () => {
  const value = { z: undefined, instruction: 'Привет 🚗', itemIds: ['b', 'a'], nested: { z: 1, a: [undefined, { y: 2, b: 'текст' }] }, requestId: 'ignored' };
  const canonical = '{"instruction":"Привет 🚗","itemIds":["b","a"],"nested":{"a":[null,{"b":"текст","y":2}],"z":1}}';
  assert.equal(localAdmissionPayloadHash(value), createHash('sha256').update(canonical).digest('hex'));
  assert.equal(localAdmissionPayloadHash(value), localAdmissionPayloadHash({ nested: value.nested, itemIds: value.itemIds, instruction: value.instruction }));
  assert.notEqual(localAdmissionPayloadHash(value), localAdmissionPayloadHash({ ...value, itemIds: ['a', 'b'] }));
});

test('receipt hash, saved hash and current checkpoint payload must all match', async t => {
  const path = await fixture(t);
  for (const kind of ['prepare', 'approval']) {
    for (const corruption of ['receipt', 'saved', 'checkpoint', 'missing']) {
      let reads = 0;
      const state = { ...prepared, phase: 'unknown', [`${kind}RequestId`]: 'key',
        pendingLocalAdmission: { kind, requestId: 'key', payloadHash: hashFor(kind) } };
      if (corruption === 'saved') state.pendingLocalAdmission.payloadHash = '0'.repeat(64);
      if (corruption === 'missing') delete state.pendingLocalAdmission.payloadHash;
      if (corruption === 'checkpoint') {
        if (kind === 'prepare') state.itemIds = ['other-item'];
        else state.approvedProposals = [{ id: 'other-proposal', revision: 1 }];
      }
      await writeCheckpoint(path, state);
      const api = client({ localAdmission: async () => { reads += 1;
        return { kind, requestId: 'key', payloadHash: corruption === 'receipt' ? '0'.repeat(64) : hashFor(kind),
          status: 'committed', result: kind === 'prepare' ? { jobId: 'prepare-job' } : { id: 'approval-1' } }; },
        prepareEngine: () => assert.fail('no repeat'), createApproval: () => assert.fail('no repeat'), execute: () => assert.fail('no execute') });
      await assert.rejects(runWorkflow(api, [], { resumePath: path, autonomous: true, execute: true }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
      assert.equal(reads, corruption === 'receipt' ? 1 : 0);
    }
  }
});

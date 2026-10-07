import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { CommunityHeroClient, UnknownMutationError, checkpointError, localAdmissionPayloadHash, waitForJob } from '../cli/client.mjs';
import { freshSync, queueCoverage, summarizeQueue } from '../cli/queue.mjs';
import { runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { resultExitCode } from '../cli/read-observer.mjs';

const execFileAsync = promisify(execFile);
const cli = fileURLToPath(new URL('../cli/communityhero.mjs', import.meta.url));

function workspace(state) {
  const frontierDone = state.syncCalls >= state.frontierCompleteAfter;
  const open = { id: 'frontier', cursor: frontierDone ? null : `cursor-${state.syncCalls}`, done: frontierDone, pages: state.syncCalls, skipped: 0, unknownDates: 0, coverageComplete: frontierDone };
  const openEvidence = state.canonicalCoverage
    ? { openCoverage: { ...open, scanId: 'canonical-scan', scope: 'all-open' } }
    : { openFrontier: open };
  return {
    account: state.account, workspaceVersion: `v${state.version}`, operator: { id: 'operator', role: 'owner' },
    settings: { externalWritesEnabled: true }, sync: { status: frontierDone ? 'complete' : 'partial', ...openEvidence, scan: { closed: { done: false } } }, items: state.items,
    proposals: state.proposals, approvals: state.approvals, operations: state.operations, jobs: state.jobs, materials: state.materials,
    ...(state.companyKnowledgeAuthority ? { companyKnowledgeAuthority: state.companyKnowledgeAuthority } : {})
  };
}

async function fakeServer(overrides = {}) {
  const state = {
    account: 'LikeAvto', version: 1,
    items: [{ id: 'item-1', revision: 4, draft: 'Prepared reply', workflow: 'attention' }],
    proposals: [{ id: 'old-unrelated', revision: 1, itemId: 'other', kind: 'close', text: '', status: 'draft' }],
    admissions: {}, approvals: [], operations: [], jobs: [], materials: [], requests: [], executeOutcome: 'succeeded', scanCalls: 0, syncCalls: 0, prepareCalls: 0, frontierCompleteAfter: 1, enginePrepare: true, ...overrides
  };
  const server = http.createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : null;
    state.requests.push({ method: request.method, path: request.url, body, csrf: request.headers['x-csrf-token'], cookie: request.headers.cookie,
      contentType: request.headers['content-type'] });
    const send = (status, value) => { response.writeHead(status, { 'content-type': 'application/json' }); response.end(JSON.stringify(value)); };
    if (request.url === '/api/health') return send(200, { status: 'ok', account: state.account });
    if (request.url === '/api/session') return send(200, { id: 'operator', role: 'owner', csrfToken: 'csrf-secret' });
    if (request.url === '/api/engine/status') return send(200, { account: state.account, displayAccount: state.account,
      strictGrouping: { version: 1, contract: 'strict_post_family_v1' } });
    if (request.url === '/api/bootstrap') return send(200, workspace(state));
    if (/^\/api\/local-admissions\/(execute|editorial)\//u.test(request.url)) return send(200, state.admissions[request.url.split('/').at(-1)] || { status: 'pending_or_unknown' });
    if (request.url === '/api/proposals/editorial-review' && request.method === 'POST') {
      const result = { jobId: `editorial-${body.requestId}`, requestId: body.requestId, replayed: false };
      state.jobs.push({ id: result.jobId, kind: 'editorial_review', refId: body.requestId, status: 'completed',
        editorialOutcome: state.editorialHold
          ? { accepted: [], reused: [], held: body.proposals.map(reference => ({ reference, decision: 'revise', reason: 'Review the exact text' })) }
          : { accepted: body.proposals, reused: body.proposals, held: [] } });
      state.admissions[body.requestId] = { kind: 'editorial', requestId: body.requestId, status: 'committed', payloadHash: localAdmissionPayloadHash(body), result };
      if (state.loseEditorialResponse) { request.socket.destroy(); return; }
      return send(200, result);
    }
    if (request.url.startsWith('/api/items/review-bundle?')) {
      const ids = new URL(request.url, 'http://localhost').searchParams.get('itemIds')?.split(',') || [];
      if (!ids.length || ids.length > 100 || ids.some(id => !state.items.some(row => row.id === id))) return send(404, { error: 'Selected item missing' });
      const operations = state.operations.filter(row => ids.includes(row.itemId)
        || ids.includes(state.proposals.find(proposal => proposal.id === row.proposalId)?.itemId));
      return send(200, { account: state.account, selectedItemIds: ids, items: state.items.filter(row => ids.includes(row.id)),
        posts: [], branches: [], proposals: state.proposals.filter(row => ids.includes(row.itemId)), operations,
        coverage: { itemsReturned: ids.length, operationsReturned: operations.length,
          operationsComplete: state.reviewOperationsComplete !== false, historyTruncated: false } });
    }
    if (/^\/api\/engine\/jobs\//.test(request.url)) {
      const job = state.jobs.find(row => request.url.endsWith(encodeURIComponent(row.id)));
      return job ? send(200, job) : send(404, { error: 'job not found' });
    }
    if (request.url === '/api/conversations' && request.method === 'POST') return send(200, { id: 'headless-chat' });
    if (request.url === '/api/sync' && request.method === 'POST') {
      state.syncCalls += 1; if (state.onSync) state.onSync(state);
      const job = { id: `sync-job-${state.syncCalls}`, kind: 'sync', status: 'completed', result: { partial: state.syncCalls < state.frontierCompleteAfter, openFrontier: workspace(state).sync.openFrontier } }; state.jobs.push(job);
      return send(200, { jobId: job.id });
    }
    if (/^\/api\/engine\/items\/[^/]+\/context-refresh$/.test(request.url) && request.method === 'POST') {
      if (body !== null) return send(400, { error: 'Body must be empty' });
      const itemId = decodeURIComponent(request.url.split('/')[4]);
      if (!state.items.some(row => row.id === itemId)) return send(404, { error: 'Item not found' });
      if (state.refreshResponse) return send(200, state.refreshResponse);
      const job = { id: `refresh-${itemId}`, kind: 'target_refresh', refId: itemId,
        status: state.refreshJobStatus || 'completed',
        result: { itemId, refresh: 'admitted', itemRevision: state.items.find(row => row.id === itemId).revision,
          providerStatus: state.items.find(row => row.id === itemId).providerStatus || 'new', contextObservedAt: '2026-09-24T12:00:00Z' } };
      state.jobs.push(job);
      return send(200, { jobId: job.id, itemId, status: 'running', deduplicated: state.refreshDeduplicated === true });
    }
    if (request.url === '/api/materials/import' && request.method === 'POST') {
      if (Object.hasOwn(state, 'materialsResponse')) return send(200, state.materialsResponse);
      state.materials.push({ id: 'import-policy', kind: 'knowledge', imported: true });
      const job = { id: 'materials-job', kind: 'materials', status: 'completed', result: { imported: 1 } }; state.jobs.push(job);
      return send(200, { jobId: job.id });
    }
    if (request.url === '/api/engine/prepare/families' && request.method === 'POST') {
      const windows=[];
      for(let i=0;i<body.itemIds.length&&windows.length<body.maxBatches;i+=body.batchSize)
        windows.push(body.itemIds.slice(i,i+body.batchSize));
      return send(200,{account:state.account,advisory:true,selectedItemIds:body.itemIds,windows});
    }
    if (request.url === '/api/engine/prepare/plan' && request.method === 'POST') {
      const batches = state.planGroups || [body.itemIds];
      const held = state.planHeld || [];
      const plan = { account: state.account, byteLimit: 550000, advisory: true, selectedItemIds: body.itemIds,
        batches: batches.map(itemIds => ({ itemIds, bytes: itemIds.length * 1000 })), held };
      return send(200, state.planResponse ? state.planResponse(plan, state) : plan);
    }
    if (request.url === '/api/engine/prepare' && request.method === 'POST' && state.prepareFailureStatus)
      return send(state.prepareFailureStatus, { error: 'PRIVATE_HTTP_BODY' });
    if (request.url === '/api/engine/prepare' && request.method === 'POST' && state.enginePrepare) {
      if (body.itemIds.some(id => state.prepareConflictItemIds?.includes(id)))
        return send(409, { error: 'Revision changed; review current content' });
      state.prepareCalls += 1; const job = { id: `engine-prepare-${state.prepareCalls}`, kind: 'engine-prepare', status: 'completed', result: { prepared: [], held: [], admission: {} } };
      for (const itemId of body.itemIds) {
        const item = state.items.find(row => row.id === itemId); item.workflow = 'prepared'; item.revision += 1;
        const proposal = { id: `generated-${itemId}`, revision: 1, itemId, itemRevision: item.revision, kind: 'reply_and_close', text: 'Generated answer', status: 'draft', prepareRunId: job.id };
        state.proposals.push(proposal); job.result.prepared.push({ itemId, proposalId: proposal.id, proposalRevision: 1, kind: proposal.kind });
      }
      state.jobs.push(job); state.version += 1; return send(200, { jobId: job.id });
    }
    if (request.url === '/api/conversations/headless-chat/messages' && request.method === 'POST') {
      state.prepareCalls += 1; const job = { id: `prepare-job-${state.prepareCalls}`, kind: 'assistant', status: 'completed', prepareOutcome: { status: 'review' } };
      state.jobs.push(job);
      for (const itemId of body.itemIds) {
        const item = state.items.find(row => row.id === itemId); item.workflow = 'prepared'; item.revision += 1;
        state.proposals.push({ id: `generated-${itemId}`, revision: 1, itemId, itemRevision: item.revision, kind: 'reply_and_close', text: 'Generated answer', status: 'draft', prepareRunId: job.id });
      }
      state.version += 1; return send(200, { jobId: job.id });
    }
    if (request.url === '/api/engine/scan' && request.method === 'POST') {
      state.scanCalls += 1; const complete = body.resume === 'opaque-next';
      const result = complete
        ? { account: 'likeavto', items: [{ id: 'new' }], scannedCount: 1, pagesRead: 1, hasMore: false, stopReason: 'exhausted', coverage: [{ objectId: 'one', status: 'new', state: 'exhausted' }], failures: [], nextResume: null }
        : { account: 'likeavto', items: [{ id: 'old' }], scannedCount: 1, pagesRead: 2, hasMore: true, stopReason: 'max_pages', coverage: [{ objectId: 'one', status: 'new', state: 'pending' }], failures: [], nextResume: 'opaque-next' };
      const job = { id: `scan-job-${state.scanCalls}`, kind: 'provider-scan', status: 'completed', result }; state.jobs.push(job);
      return send(200, { jobId: job.id });
    }
    if (request.url === '/api/proposals' && request.method === 'POST') {
      const item = state.items.find(row => row.id === body.itemId);
      if (!item || item.revision !== body.expectedRevision) return send(409, { error: 'Revision changed; review current content' });
      const proposal = { id: `new-${state.proposals.length}`, revision: 1, itemId: body.itemId, kind: body.kind, text: body.text, status: 'draft' };
      state.proposals.push(proposal); state.version += 1; return send(200, proposal);
    }
    if (request.url === '/api/approvals' && request.method === 'POST') {
      const current = body.proposals.every(ref => state.proposals.some(row => row.id === ref.id && row.revision === ref.revision && row.status === 'draft'));
      if (!current) return send(409, { error: 'Proposal changed' });
      const approval = { id: `approval-${state.approvals.length}`, proposals: body.proposals, status: 'approved' };
      state.approvals.push(approval); for (const ref of body.proposals) state.proposals.find(row => row.id === ref.id).status = 'approved';
      state.version += 1; return send(200, approval);
    }
    if (/^\/api\/approvals\/[^/]+\/execute$/.test(request.url) && request.method === 'POST') {
      const approval = state.approvals.find(row => request.url.includes(row.id));
      if (!approval || approval.status !== 'approved') return send(409, { error: 'Approval already consumed' });
      approval.status = 'consumed'; const job = { id: `execute-job-${approval.id}`, kind: 'execute', refId: approval.id, status: 'completed' }; state.jobs.push(job);
      if (!state.omitExecuteOperations) for (const ref of approval.proposals) state.operations.push({ id: `op-${ref.id}`, proposalId: ref.id, approvalId: approval.id,
        itemId: state.proposals.find(row => row.id === ref.id)?.itemId, status: state.executeOutcome });
      if (state.coverageAfterExecuteIncomplete) state.reviewOperationsComplete = false;
      const result = { jobId: job.id, ...(body.requestId ? { approvalId: approval.id, requestId: body.requestId, replayed: false } : {}) };
      if (body.requestId) state.admissions[body.requestId] = { kind: 'execute', requestId: body.requestId,
        status: 'committed', payloadHash: localAdmissionPayloadHash({ approvalId: approval.id }), result };
      state.version += 1;
      if (state.loseExecuteResponse) { request.socket.destroy(); return; }
      return send(200, result);
    }
    if (/^\/api\/operations\/[^/]+\/reconcile$/.test(request.url) && request.method === 'POST') {
      const operation = state.operations.find(row => request.url.includes(row.id));
      if (!operation || operation.status !== 'unknown') return send(409, { error: 'Only unknown operations need reconciliation' });
      operation.status = 'succeeded'; const job = { id: `reconcile-${operation.id}`, kind: 'reconcile', status: 'completed' }; state.jobs.push(job);
      state.version += 1; return send(200, { jobId: job.id });
    }
    return send(404, { error: 'not found' });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  return { state, url: `http://127.0.0.1:${address.port}`, close: () => new Promise(resolve => server.close(resolve)) };
}

async function run(args, options = {}) {
  try {
    const result = await execFileAsync(process.execPath, [cli, ...args], { timeout: 10_000, ...options });
    return { code: 0, stdout: result.stdout, stderr: result.stderr };
  } catch (error) {
    return { code: error.code, stdout: error.stdout || '', stderr: error.stderr || '' };
  }
}

const suppressedImport = { imported: 0, authority: 'communityhero', legacyImportSuppressed: true };
test('inspect accepts each single selector and rejects ambiguous or empty selectors without requests', async t => {
  const fake = await fakeServer({ approvals: [{ id: 'approval-1' }], operations: [{ id: 'operation-1' }] }); t.after(fake.close);
  for (const [kind, id] of [['item', 'item-1'], ['proposal', 'old-unrelated'], ['approval', 'approval-1'], ['operation', 'operation-1']]) {
    const result = await run(['inspect', '--account', 'likeavto', '--base-url', fake.url, `--${kind}`, id]);
    assert.equal(result.code, 0, result.stderr);
    assert.equal(JSON.parse(result.stdout)[kind].id, id);
  }
  const before = fake.state.requests.length;
  for (const selectors of [[], ['--item', 'item-1', '--proposal', 'old-unrelated'], ['--item', 'item-1', '--item', 'other'], ['--proposal', 'a', '--proposal', 'b'], ['--item='], ['--approval', '   ']]) {
    const result = await run(['inspect', '--account', 'likeavto', '--base-url', fake.url, ...selectors]);
    assert.notEqual(result.code, 0);
    assert.match(result.stderr, /USAGE/);
  }
  assert.equal(fake.state.requests.length, before);
  assert.ok(fake.state.requests.every(request => request.method === 'GET'));
});

const canonicalPolicy = {
  account: 'BAW Russia', enginePrepare: true, materialsResponse: suppressedImport,
  companyKnowledgeAuthority: { owner: 'communityhero', account: 'BAW Russia', companyKey: 'baw-russia' },
  materials: [{ id: 'canonical-policy', kind: 'rule', companyKnowledge: true, account: 'BAW Russia', text: 'Synthetic policy' }]
};

test('materials synchronous suppression returns without job polling, with and without wait', async t => {
  const fake = await fakeServer(canonicalPolicy); t.after(fake.close);
  for (const wait of [[], ['--wait']]) {
    const result = await run(['materials', '--base-url', fake.url, '--account', 'baw-russia', ...wait]);
    assert.equal(result.code, 0, result.stderr); assert.deepEqual(JSON.parse(result.stdout), suppressedImport);
  }
  assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 2);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/engine/jobs/')).length, 0);
});

test('context-refresh uses one bodyless encoded local target POST and polls only its durable job', async t => {
  const id = 'local/item?one';
  const fake = await fakeServer({ items: [{ id, revision: 7, workflow: 'attention', providerStatus: 'new' }],
    refreshDeduplicated: true }); t.after(fake.close);
  const result = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', id, '--wait']);
  assert.equal(result.code, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.itemId, id); assert.equal(output.deduplicated, true);
  assert.equal(output.status, 'completed'); assert.equal(output.result.refresh, 'admitted');
  const posts = fake.state.requests.filter(row => row.method === 'POST');
  assert.equal(posts.length, 1);
  assert.equal(posts[0].path, '/api/engine/items/local%2Fitem%3Fone/context-refresh');
  assert.equal(posts[0].body, null); assert.equal(posts[0].contentType, undefined);
  assert.equal(posts[0].csrf, 'csrf-secret');
  assert(fake.state.requests.some(row => row.path === '/api/engine/jobs/refresh-local%2Fitem%3Fone'));
  assert(!fake.state.requests.some(row => row.path === '/api/sync' || /\/execute$/.test(row.path)));
});

test('context-refresh poll-only resume verifies job binding and never posts again', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const first = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1']);
  assert.equal(first.code, 0, first.stderr);
  const jobId = JSON.parse(first.stdout).jobId;
  const resumed = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--job', jobId, '--wait']);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).status, 'completed');
  assert.equal(fake.state.requests.filter(row => row.path.endsWith('/context-refresh')).length, 1);
  const mismatched = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'other', '--job', jobId]);
  assert.equal(mismatched.code, 1); assert.match(mismatched.stderr, /STALE_OR_CONFLICT/);
  assert.equal(fake.state.requests.filter(row => row.path.endsWith('/context-refresh')).length, 1);
});

test('context-refresh polling bound resumes by saved job ID without a second mutation', async t => {
  const fake = await fakeServer({ refreshJobStatus: 'running' }); t.after(fake.close);
  const first = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--wait', '--max-polls', '1']);
  assert.equal(first.code, 4); assert.match(first.stderr, /POLL_LIMIT/);
  const job = fake.state.jobs.find(row => row.kind === 'target_refresh');
  assert(job); job.status = 'completed';
  const records = first.stderr.trim().split(/\r?\n/u).map(line => JSON.parse(line));
  assert.equal(records.at(-1).error.code, 'POLL_LIMIT');
  const started = records.filter(row => row.event === 'context-refresh.started');
  assert.equal(started.length, 1); assert.equal(started[0].itemId, 'item-1');
  const savedJobId = started[0].jobId;
  assert.equal(savedJobId, job.id);
  const resumed = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--job', savedJobId, '--wait']);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).jobId, savedJobId);
  assert.equal(JSON.parse(resumed.stdout).result.itemId, 'item-1');
  assert.equal(fake.state.requests.filter(row => row.path.endsWith('/context-refresh')).length, 1);
});

test('context-refresh rejects multiple targets and wrong company before mutation', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const invalid = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--item', 'other']);
  assert.equal(invalid.code, 2); assert.match(invalid.stderr, /USAGE/);
  const foreign = await run(['context-refresh', '--base-url', fake.url, '--account', 'BAW Russia', '--item', 'item-1']);
  assert.equal(foreign.code, 1); assert.match(foreign.stderr, /WRONG_ACCOUNT/);
  assert.equal(fake.state.requests.filter(row => row.method === 'POST').length, 0);
});

test('unrecognized context-refresh success is unknown and is never automatically repeated', async t => {
  const fake = await fakeServer({ refreshResponse: { jobId: 'maybe' } }); t.after(fake.close);
  const result = await run(['context-refresh', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1']);
  assert.equal(result.code, 4); assert.match(result.stderr, /UNKNOWN_MUTATION_OUTCOME/);
  assert.equal(fake.state.requests.filter(row => row.path.endsWith('/context-refresh')).length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/sync').length, 0);
});

test('prepare and queue accept canonical account policy after synchronous suppression', async t => {
  for (const command of ['prepare', 'queue']) {
    const fake = await fakeServer({ ...canonicalPolicy, materials: structuredClone(canonicalPolicy.materials) }); t.after(fake.close);
    const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-'));
    const result = await run([command, '--base-url', fake.url, '--account', 'baw-russia', '--checkpoint', join(dir, 'state.json'),
      ...(command === 'prepare' ? ['--item', 'item-1'] : ['--max-cycles', '2'])]);
    assert.equal(result.code, 0, result.stderr); assert.equal(fake.state.prepareCalls, 1);
    assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 1);
    assert(!fake.state.requests.some(row => /jobs\/(undefined|null|materials-job)$/.test(row.path)));
  }
});

test('synchronous suppression without canonical policy stops before preparation and resume does not reimport', async t => {
  const fake = await fakeServer({ ...canonicalPolicy, materials: [] }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')), checkpoint = join(dir, 'state.json');
  for (const resume of [false, true]) {
    const result = await run(['queue', '--base-url', fake.url, '--account', 'baw-russia', ...(resume ? ['--resume', checkpoint] : ['--checkpoint', checkpoint])]);
    assert.equal(result.code, 1); assert.match(result.stderr, /MATERIALS_UNAVAILABLE/);
  }
  assert.equal(fake.state.prepareCalls, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/engine/jobs/')).length, 0);
});

test('malformed materials success is unknown and queue resume never retries it', async t => {
  const fake = await fakeServer({ materialsResponse: { imported: 0 } }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')), checkpoint = join(dir, 'state.json');
  for (const resume of [false, true]) {
    const result = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', ...(resume ? ['--resume', checkpoint] : ['--checkpoint', checkpoint])]);
    assert.equal(result.code, 4); assert.match(result.stderr, /UNKNOWN_MUTATION_OUTCOME/);
  }
  assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/engine/jobs/')).length, 0);
  assert.equal(fake.state.prepareCalls, 0);
});

test('prepare-only run binds account, uses assistant generation, and persists no session secret', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  const result = await run(['run', '--base-url', fake.url, '--account', 'likeavto', '--item', 'item-1', '--checkpoint', checkpoint], { env: { ...process.env, COMMUNITYHERO_SESSION: 'session=very-secret' } });
  assert.equal(result.code, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).mode, 'prepare-only');
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/conversations')).length, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/proposals').length, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);
  assert.doesNotMatch(await readFile(checkpoint, 'utf8'), /very-secret|csrf-secret/);
  const mutation = fake.state.requests.find(row => row.path === '/api/engine/prepare');
  assert.deepEqual(mutation.body.itemIds, ['item-1']); assert.equal(mutation.csrf, 'csrf-secret');
});

test('prepare prefers the exact engine endpoint and does not fall through to legacy conversation', async t => {
  const fake = await fakeServer({ enginePrepare: true, materials: [{ id: 'old-fact', kind: 'knowledge', imported: true }] }); t.after(fake.close);
  const result = await run(['prepare', '--base-url', fake.url, '--account', 'likeavto', '--item', 'item-1']);
  assert.equal(result.code, 0, result.stderr);
  const request = fake.state.requests.find(row => row.path === '/api/engine/prepare');
  assert.deepEqual(request.body.itemIds, ['item-1']); assert.equal(typeof request.body.instruction, 'string');
  assert.equal(fake.state.requests.filter(row => row.path === '/api/conversations').length, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 1);
});

test('direct prepare reports a split plan before starting a model job', async t => {
  const items = ['one', 'two'].map(id => ({ id, revision: 1, workflow: 'attention' }));
  const fake = await fakeServer({ items, enginePrepare: true, planGroups: [['one'], ['two']] }); t.after(fake.close);
  const result = await run(['prepare', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'one', '--item', 'two']);
  assert.equal(result.code, 1); assert.match(result.stderr, /PREPARE_PLAN_SPLIT_REQUIRED/);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 0);
});

test('queue uses every planned subset and records explicit holds without omitted recipients', async t => {
  const items = ['one', 'two', 'three', 'four', 'five'].map(id => ({ id, revision: 1, workflow: 'attention', providerStatus: 'new' }));
  const fake = await fakeServer({ items, enginePrepare: true, planGroups: [['one', 'two'], ['three', 'four']],
    planHeld: [{ itemId: 'five', reason: 'evidence_too_large' }] }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const result = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--checkpoint', checkpoint, '--batch-size', '5']);
  assert.equal(result.code, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.counts.prepared, 4); assert.equal(output.counts.held, 1);
  assert.deepEqual(fake.state.requests.filter(row => row.path === '/api/engine/prepare').map(row => row.body.itemIds), [['one', 'two'], ['three', 'four']]);
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.deepEqual(new Set(saved.attemptedItemIds), new Set(items.map(row => row.id)));
  assert.equal(saved.slices.find(row => row.status === 'plan-held')?.planHoldReason, 'evidence_too_large');
});

test('queue resume consumes saved planned subsets without re-planning or dropping the tail', async t => {
  const items = ['one', 'two', 'three'].map(id => ({ id, revision: 1, workflow: 'attention', providerStatus: 'new' }));
  const fake = await fakeServer({ items, enginePrepare: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const pendingSlices = [['one', 'two'], ['three']].map((itemIds, index) => ({
    id: `cycle-1-slice-${index + 1}`, itemIds, plannedBytes: itemIds.length * 1000,
    childPath: `${checkpoint}.slices/cycle-1-slice-${index + 1}.json`
  }));
  await writeCheckpoint(checkpoint, { kind: 'communityhero-queue', account: 'LikeAvto', baseUrl: fake.url,
    phase: 'running', materialsReady: true, cycle: 1, syncCycles: 0, attemptedItemIds: [], slices: [], pendingSlices });
  const result = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint]);
  assert.equal(result.code, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).mode, 'complete');
  assert.deepEqual(fake.state.requests.filter(row => row.path === '/api/engine/prepare').map(row => row.body.itemIds), [['one', 'two'], ['three']]);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare/plan').length, 0);
  assert.equal(fake.state.syncCalls, 1, 'one fresh full sync follows all saved slices');
  const requests = fake.state.requests.map(row => row.path);
  assert(requests.lastIndexOf('/api/engine/prepare') < requests.indexOf('/api/sync'));
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.pendingSlices.length, 0);
  assert.deepEqual(new Set(saved.attemptedItemIds), new Set(items.map(row => row.id)));
});

test('resumed pending slices preserve open coverage continuation without a sync between slices', async t => {
  const items = ['one', 'two'].map(id => ({ id, revision: 1, workflow: 'attention', providerStatus: 'new' }));
  const fake = await fakeServer({ items, enginePrepare: true, canonicalCoverage: true, frontierCompleteAfter: 3 }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const pendingSlices = items.map((item, index) => ({
    id: `cycle-1-slice-${index + 1}`, itemIds: [item.id], plannedBytes: 1000,
    childPath: `${checkpoint}.slices/cycle-1-slice-${index + 1}.json`
  }));
  await writeCheckpoint(checkpoint, { kind: 'communityhero-queue', account: 'LikeAvto', baseUrl: fake.url,
    phase: 'running', materialsReady: true, cycle: 1, syncCycles: 0, attemptedItemIds: [], slices: [], pendingSlices });
  const result = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint]);
  assert.equal(result.code, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.mode, 'complete'); assert.equal(output.coverage.complete, true);
  assert.equal(output.coverage.source, 'openCoverage');
  assert.equal(fake.state.syncCalls, 3, 'only frontier continuation requires additional syncs');
  assert.deepEqual(fake.state.requests.filter(row => row.path === '/api/engine/prepare').map(row => row.body.itemIds), [['one'], ['two']]);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare/plan').length, 0);
});

test('a rejected pending slice preserves successful proposals and cannot claim queue completion', async t => {
  const items = ['one', 'two'].map(id => ({ id, revision: 1, workflow: 'attention', providerStatus: 'new' }));
  const fake = await fakeServer({ items, enginePrepare: true, prepareConflictItemIds: ['two'] }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const pendingSlices = items.map((item, index) => ({
    id: `cycle-1-slice-${index + 1}`, itemIds: [item.id], plannedBytes: 1000,
    childPath: `${checkpoint}.slices/cycle-1-slice-${index + 1}.json`
  }));
  await writeCheckpoint(checkpoint, { kind: 'communityhero-queue', account: 'LikeAvto', baseUrl: fake.url,
    phase: 'running', materialsReady: true, cycle: 1, syncCycles: 0, attemptedItemIds: [], slices: [], pendingSlices });
  const result = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint]);
  assert.equal(result.code, 4, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.mode, 'stopped'); assert.equal(output.stopReason, 'slice-outcomes-unresolved');
  assert.equal(output.counts.prepared, 1); assert.equal(output.counts.sliceFailures, 1);
  assert.equal(fake.state.proposals.filter(row => row.itemId === 'one' && row.status === 'draft').length, 1);
  assert.equal(fake.state.proposals.filter(row => row.itemId === 'two').length, 0);
  assert.equal(fake.state.syncCalls, 1);
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.slices.find(row => row.itemIds[0] === 'one').status, 'prepare-only');
  assert.equal(saved.slices.find(row => row.itemIds[0] === 'two').error.code, 'STALE_OR_CONFLICT');
});

test('max-cycle stop after a slice does not present the pre-slice scan as fresh coverage', async t => {
  const fake = await fakeServer({ enginePrepare: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const first = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--checkpoint', checkpoint, '--max-cycles', '1']);
  assert.equal(first.code, 4, first.stderr);
  const bounded = JSON.parse(first.stdout);
  assert.equal(bounded.mode, 'stopped'); assert.equal(bounded.stopReason, 'max-cycles');
  assert.equal(bounded.coverage.complete, false); assert.equal(bounded.coverage.reason, 'post-slice-sync-required');
  assert.equal(fake.state.syncCalls, 1); assert.equal(fake.state.prepareCalls, 1);
  const resumed = await run(['queue', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint, '--max-cycles', '2']);
  assert.equal(resumed.code, 0, resumed.stderr);
  const completed = JSON.parse(resumed.stdout);
  assert.equal(completed.mode, 'complete'); assert.equal(completed.coverage.complete, true);
  assert.equal(fake.state.syncCalls, 2); assert.equal(fake.state.prepareCalls, 1);
});

test('preparation plan rejects duplicate, omitted, and cross-account selections', async t => {
  const fake = await fakeServer({ items: [{ id: 'one' }, { id: 'two' }] }); t.after(fake.close);
  const client = new CommunityHeroClient({ baseUrl: fake.url, account: 'LikeAvto' });
  for (const override of [
    plan => ({ ...plan, batches: [{ itemIds: ['one', 'one'], bytes: 1000 }] }),
    plan => ({ ...plan, batches: [{ itemIds: ['one'], bytes: 1000 }] }),
    plan => ({ ...plan, account: 'BAW Russia' }),
    plan => ({ ...plan, selectedItemIds: ['two', 'one'] }),
    plan => ({ ...plan, batches: [{ itemIds: ['one', 'two'], bytes: 550001 }] })
  ]) {
    fake.state.planResponse = override;
    await assert.rejects(client.planPrepare(['one', 'two']), error => ['INVALID_PREPARE_PLAN', 'WRONG_ACCOUNT'].includes(error.code));
  }
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 0);
});

test('lost read-only plan response is a read error and is not retried', async () => {
  let calls = 0;
  const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async (url, options) => {
    if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 });
    if (url.pathname === '/api/engine/prepare/plan') {
      calls += 1; assert.equal(options.headers['x-csrf-token'], 'csrf');
      assert.equal(options.headers['content-type'], 'application/json');
      throw new Error('read interrupted');
    }
    throw new Error('unexpected request');
  } });
  await assert.rejects(client.planPrepare(['one']), { code: 'NETWORK_ERROR' });
  assert.equal(calls, 1);
});

test('resume approves only the proposal created by its checkpoint and does not resend prepare', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  assert.equal((await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--checkpoint', checkpoint])).code, 0);
  const resumed = await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint, '--autonomous']);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).mode, 'approved');
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
  const approvalRequest = fake.state.requests.find(row => row.path === '/api/approvals');
  assert.deepEqual(approvalRequest.body.proposals, [{ id: 'generated-item-1', revision: 1 }]);
  assert.ok(!approvalRequest.body.proposals.some(row => row.id === 'old-unrelated'));
});

test('stale proposal on resume stops before approval', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--checkpoint', checkpoint]);
  const proposal = fake.state.proposals.find(row => row.id === 'generated-item-1'); proposal.revision = 2; proposal.text = 'changed';
  const resumed = await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint, '--autonomous']);
  assert.equal(resumed.code, 1); assert.match(resumed.stderr, /STALE_OR_CONFLICT/);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);
});

test('incomplete selected operation coverage prevents approval', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  assert.equal((await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--checkpoint', checkpoint])).code, 0);
  fake.state.reviewOperationsComplete = false;
  const resumed = await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint, '--autonomous']);
  assert.equal(resumed.code, 4); assert.match(resumed.stderr, /INCOMPLETE_OPERATION_COVERAGE/);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);
});

test('drain executes its exact approval and reconciles an unknown outcome without resending execute', async t => {
  const fake = await fakeServer({ executeOutcome: 'unknown' }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  const result = await run(['drain', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--autonomous', '--execute', '--checkpoint', checkpoint]);
  assert.equal(result.code, 0, result.stderr); assert.equal(JSON.parse(result.stdout).mode, 'complete');
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
  assert.equal(fake.state.requests.filter(row => /\/reconcile$/.test(row.path)).length, 1);
  assert.equal(fake.state.operations[0].status, 'succeeded');
  const readbackIndex = fake.state.requests.findIndex(row => /\/reconcile$/.test(row.path));
  assert.equal(fake.state.requests.slice(readbackIndex + 1).filter(row => row.path === '/api/bootstrap').length, 0);
});

test('standalone reconcile wait returns its job without fetching another workspace snapshot', async t => {
  const fake = await fakeServer({ operations: [{ id: 'op-unknown', itemId: 'item-1', status: 'unknown' }] }); t.after(fake.close);
  const result = await run(['reconcile', '--base-url', fake.url, '--account', 'LikeAvto', '--operation', 'op-unknown', '--wait']);
  assert.equal(result.code, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.job.id, 'reconcile-op-unknown'); assert.equal(output.snapshot, null);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/bootstrap').length, 1);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 0);
  assert.equal(fake.state.operations[0].status, 'succeeded');
});

for (const [name, overrides, expectedMode, expectedCode] of [
  ['missing operation', { omitExecuteOperations: true }, 'needs-reconciliation', 4],
  ['dispatching operation', { executeOutcome: 'dispatching' }, 'needs-reconciliation', 4],
  ['failed operation', { executeOutcome: 'failed' }, 'completed-with-failures', 1],
  ['incomplete operation coverage', { coverageAfterExecuteIncomplete: true }, 'needs-reconciliation', 4]
]) {
  test(`drain reports ${name} without a false success or duplicate execution`, async t => {
    const fake = await fakeServer(overrides); t.after(fake.close);
    const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
    const result = await run(['drain', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--autonomous', '--execute', '--checkpoint', checkpoint]);
    assert.equal(result.code, expectedCode, result.stderr);
    assert.equal(JSON.parse(result.stdout).mode, expectedMode);
    assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
    assert.equal(fake.state.requests.filter(row => /\/reconcile$/.test(row.path)).length, 0);
    const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
    assert.equal(saved.phase, expectedMode);
    assert.deepEqual(saved.operationEvidence.invalidBindingOperationIds, []);
    if (name === 'missing operation') assert.deepEqual(saved.operationEvidence.missingProposalIds, ['generated-item-1']);
    if (name === 'dispatching operation') assert.deepEqual(saved.operationEvidence.pendingOperationIds, ['op-generated-item-1']);
    if (name === 'failed operation') assert.equal(saved.operations[0].status, 'failed');
    if (name === 'incomplete operation coverage') assert.equal(saved.operationEvidence.operationsComplete, false);
  });
}

test('scan is bounded, persists incomplete coverage, and resumes only from its explicit token', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'scan.json');
  const first = await run(['scan', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--page-size', '25', '--max-pages', '2', '--max-items', '40', '--max-elapsed-ms', '5000']);
  assert.equal(first.code, 0, first.stderr); assert.equal(JSON.parse(first.stdout).mode, 'incomplete');
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.coverageComplete, false); assert.equal(saved.nextResume, 'opaque-next'); assert.equal(saved.incompleteReason, 'provider_max_pages');
  const scanRequest = fake.state.requests.find(row => row.path === '/api/engine/scan').body;
  assert.deepEqual(scanRequest, { pageSize: 25, maxPages: 2, maxItems: 40, maxElapsedMs: 5000 });
  const second = await run(['scan', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint, '--checkpoint', checkpoint]);
  assert.equal(second.code, 0, second.stderr); assert.equal(JSON.parse(second.stdout).mode, 'complete');
  const resumedRequest = fake.state.requests.filter(row => row.path === '/api/engine/scan').at(-1).body;
  assert.equal(resumedRequest.resume, 'opaque-next');
  assert.equal(fake.state.scanCalls, 2);
});

test('queue imports policy once, walks oldest eligible items in bounded slices, and only claims empty with complete coverage', async t => {
  const items = [
    { id: 'newest', revision: 1, workflow: 'attention', providerStatus: 'new', createdAt: '2026-01-03T00:00:00Z' },
    { id: 'oldest', revision: 1, workflow: 'attention', providerStatus: 'new', createdAt: '2026-01-01T00:00:00Z' },
    { id: 'middle', revision: 1, workflow: 'attention', providerStatus: 'inprogress', createdAt: '2026-01-02T00:00:00Z' }
  ];
  const fake = await fakeServer({ items }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const result = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--batch-size', '2', '--max-cycles', '5']);
  assert.equal(result.code, 0, result.stderr); const parsed = JSON.parse(result.stdout);
  assert.equal(parsed.mode, 'complete'); assert.equal(parsed.stopReason, 'known-complete-no-eligible');
  assert.equal(parsed.counts.prepared, 3); assert.equal(parsed.counts.held, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/materials/import').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);
  assert.deepEqual(fake.state.requests.filter(row => row.path === '/api/engine/prepare').map(row => row.body.itemIds), [['oldest', 'middle'], ['newest']]);
});

test('autonomous queue approves one exact 60-proposal engine batch and no unrelated draft', async t => {
  const items = Array.from({ length: 60 }, (_, index) => ({ id: `item-${String(index).padStart(2, '0')}`, revision: 1, workflow: 'attention', providerStatus: 'new', createdAt: `2026-01-01T00:${String(index).padStart(2, '0')}:00Z` }));
  const fake = await fakeServer({ items, enginePrepare: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const result = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--batch-size', '60', '--autonomous']);
  assert.equal(result.code, 0, result.stderr); assert.equal(JSON.parse(result.stdout).mode, 'complete');
  const approvals = fake.state.requests.filter(row => row.path === '/api/approvals'); assert.equal(approvals.length, 1);
  assert.equal(approvals[0].body.proposals.length, 60); assert.ok(!approvals[0].body.proposals.some(row => row.id === 'old-unrelated'));
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
});

test('executed queue keeps missing operation unresolved and resumes from readback without repeating dispatch', async t => {
  const fake = await fakeServer({ enginePrepare: true, omitExecuteOperations: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const first = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--autonomous', '--execute']);
  assert.equal(first.code, 4, first.stderr);
  assert.equal(JSON.parse(first.stdout).mode, 'stopped');
  assert.equal(JSON.parse(first.stdout).stopReason, 'operation-outcomes-unresolved');
  assert.equal(JSON.parse(first.stdout).coverage.complete, true);
  const originalApprovalId = fake.state.approvals[0].id;
  fake.state.operations.push({ id: 'op-generated-item-1', proposalId: 'generated-item-1', approvalId: originalApprovalId, itemId: 'item-1', status: 'succeeded' });
  const resumed = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint, '--autonomous', '--execute']);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).mode, 'complete');
  assert.equal(JSON.parse(resumed.stdout).checkpoint.slices[0].child.operations[0].approvalId, originalApprovalId);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
});

test('executed queue exposes failed operation even when provider traversal is exhausted', async t => {
  const fake = await fakeServer({ enginePrepare: true, executeOutcome: 'failed' }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const result = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--autonomous', '--execute']);
  assert.equal(result.code, 1, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.mode, 'stopped'); assert.equal(output.stopReason, 'operation-failures');
  assert.equal(output.coverage.complete, true); assert.equal(output.counts.failed, 1);
});

test('queue recovers lost execute receipt across max-cycle and temporary recovery stops without another POST', async t => {
  const fake = await fakeServer({ enginePrepare: true, loseExecuteResponse: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const args = ['queue', '--base-url', fake.url, '--account', 'likeavto', '--autonomous', '--execute', '--max-cycles', '1', '--max-polls', '1'];
  const first = await run([...args, '--checkpoint', checkpoint]); assert.equal(first.code, 4, first.stderr);
  assert.equal(JSON.parse(first.stdout).stopReason, 'max-cycles');
  const admissions = fake.state.admissions; fake.state.admissions = {};
  const unconfirmed = await run([...args, '--resume', checkpoint]); assert.equal(unconfirmed.code, 4, unconfirmed.stderr);
  assert.equal(JSON.parse(unconfirmed.stdout).stopReason, 'stage-upgrade-incomplete');
  fake.state.admissions = admissions;
  const job = fake.state.jobs.find(row => row.kind === 'execute'); job.status = 'running';
  const polling = await run([...args, '--resume', checkpoint]); assert.equal(polling.code, 4, polling.stderr);
  const pollingState = JSON.parse(polling.stdout);
  assert.equal(pollingState.stopReason, 'stage-upgrade-incomplete');
  assert.equal(pollingState.checkpoint.slices[0].child.phase, 'executing');
  assert.equal(pollingState.checkpoint.slices[0].child.pendingLocalAdmission, null);
  job.status = 'completed';
  const resumed = await run([...args, '--resume', checkpoint]); assert.equal(resumed.code, 4, resumed.stderr);
  const result = JSON.parse(resumed.stdout);
  assert.equal(result.mode, 'stopped'); assert.equal(result.stopReason, 'max-cycles');
  assert.equal(result.coverage.complete, false); assert.equal(result.coverage.reason, 'post-slice-sync-required');
  assert.equal(result.checkpoint.slices[0].child.phase, 'complete');
  assert.equal(result.checkpoint.slices[0].child.pendingLocalAdmission, null);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/local-admissions/execute/')).length, 2);
  assert.equal(fake.state.requests.filter(row => /\/reconcile$/.test(row.path)).length, 0);
});

test('standalone editorial-review command persists and resumes exact refs without approving or editing', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'editorial.json');
  const first = await run(['editorial-review', '--base-url', fake.url, '--account', 'likeavto', '--proposal', 'old-unrelated@1', '--checkpoint', checkpoint]);
  assert.equal(first.code, 0, first.stderr); assert.equal(JSON.parse(first.stdout).mode, 'editorial-reviewed');
  const resumed = await run(['editorial-review', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint]);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/proposals/editorial-review').length, 1);
  assert.equal(fake.state.requests.filter(row => row.method === 'POST' && row.path !== '/api/proposals/editorial-review').length, 0);
});

test('queue resumes lost editorial admission and running review across cycle/poll bounds before one exact approval', async t => {
  const fake = await fakeServer({ enginePrepare: true, loseEditorialResponse: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const args = ['queue', '--base-url', fake.url, '--account', 'likeavto', '--autonomous', '--max-cycles', '1', '--max-polls', '1'];
  const first = await run([...args, '--checkpoint', checkpoint]); assert.equal(first.code, 4, first.stderr);
  assert.equal(JSON.parse(first.stdout).stopReason, 'max-cycles');
  assert.equal(fake.state.approvals.length, 0);
  const job = fake.state.jobs.find(row => row.kind === 'editorial_review'); job.status = 'running';
  const polling = await run([...args, '--resume', checkpoint]); assert.equal(polling.code, 4, polling.stderr);
  assert.equal(JSON.parse(polling.stdout).stopReason, 'stage-upgrade-incomplete');
  assert.equal(fake.state.approvals.length, 0); job.status = 'completed';
  const resumed = await run([...args, '--resume', checkpoint]); assert.equal(resumed.code, 4, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).stopReason, 'max-cycles');
  assert.equal(JSON.parse(resumed.stdout).checkpoint.slices[0].child.phase, 'approved');
  assert.equal(fake.state.approvals.length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/proposals/editorial-review').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path.startsWith('/api/local-admissions/editorial/')).length, 1);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 0);
});

test('queue editorial hold is explicit and never counted as a successful completed queue', async t => {
  const fake = await fakeServer({ enginePrepare: true, editorialHold: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const args = ['queue', '--base-url', fake.url, '--account', 'likeavto', '--autonomous', '--execute'];
  const first = await run([...args, '--checkpoint', checkpoint]); assert.equal(first.code, 4, first.stderr);
  const result = JSON.parse(first.stdout);
  assert.equal(result.mode, 'stopped'); assert.equal(result.stopReason, 'editorial-review-held'); assert.equal(result.counts.held, 1);
  const resumed = await run([...args, '--resume', checkpoint]); assert.equal(resumed.code, 4, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).stopReason, 'editorial-review-held');
  assert.equal(fake.state.approvals.length, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/proposals/editorial-review').length, 1);
});

test('completed prepare-only queue upgrades its owned drafts once and repeated resume is a no-op', async t => {
  const fake = await fakeServer({ enginePrepare: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const prepared = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint]);
  assert.equal(prepared.code, 0, prepared.stderr); assert.equal(JSON.parse(prepared.stdout).mode, 'complete');
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);

  const upgraded = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint, '--autonomous', '--execute']);
  assert.equal(upgraded.code, 0, upgraded.stderr); const upgradedResult = JSON.parse(upgraded.stdout);
  assert.equal(upgradedResult.mode, 'complete'); assert.equal(upgradedResult.counts.verifiedReplies, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 1);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
  let saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.slices.length, 1); assert.equal(saved.slices[0].child.phase, 'complete');

  const repeated = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint, '--autonomous', '--execute']);
  assert.equal(repeated.code, 0, repeated.stderr); assert.equal(JSON.parse(repeated.stdout).mode, 'complete');
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 1);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
  saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.slices.length, 1);
});

test('queue stage upgrade walks a new comment observed by its refresh before reclaiming empty', async t => {
  const fake = await fakeServer({ enginePrepare: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const prepared = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint]);
  assert.equal(prepared.code, 0, prepared.stderr); assert.equal(JSON.parse(prepared.stdout).stopReason, 'known-complete-no-eligible');
  fake.state.onSync = state => {
    if (!state.items.some(row => row.id === 'arrived-during-upgrade')) state.items.push({
      id: 'arrived-during-upgrade', revision: 1, workflow: 'attention', providerStatus: 'new', createdAt: '2026-01-02T00:00:00Z'
    });
  };

  const upgraded = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--resume', checkpoint, '--autonomous', '--execute']);
  assert.equal(upgraded.code, 0, upgraded.stderr); const result = JSON.parse(upgraded.stdout);
  assert.equal(result.mode, 'complete'); assert.equal(result.stopReason, 'known-complete-no-eligible');
  assert.equal(result.counts.verifiedReplies, 2);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 2);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 2);
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 2);
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.slices.length, 2); assert.ok(saved.attemptedItemIds.includes('arrived-during-upgrade'));
});

test('queue continues canonical open-coverage pagination while closed history is pending and stops at its bound', async t => {
  const fake = await fakeServer({ frontierCompleteAfter: 10, canonicalCoverage: true }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'queue.json');
  const result = await run(['queue', '--base-url', fake.url, '--account', 'likeavto', '--checkpoint', checkpoint, '--max-cycles', '20']);
  assert.equal(result.code, 0, result.stderr); const parsed = JSON.parse(result.stdout);
  assert.equal(parsed.mode, 'complete'); assert.equal(parsed.stopReason, 'known-complete-no-eligible'); assert.equal(parsed.coverage.complete, true);
  assert.equal(parsed.coverage.source, 'openCoverage'); assert.equal(parsed.coverage.closedPending, true); assert.ok(fake.state.syncCalls >= 10);
});

test('queue coverage prefers canonical all-open evidence and retains legacy fallback only when absent', () => {
  const complete = queueCoverage({ sync: { openCoverage: { id: 'coverage', scanId: 'scan-1', scope: 'all-open', cursor: null, done: true, pages: 3, skipped: 0, unknownDates: 0, coverageComplete: true } } });
  assert.equal(complete.source, 'openCoverage'); assert.equal(complete.complete, true); assert.equal(complete.pending, false);

  const pendingTwo = queueCoverage({ sync: { openCoverage: { id: 'coverage', scanId: 'scan-2', scope: 'all-open', cursor: 'page-2', done: false, pages: 2, skipped: 0, unknownDates: 0, coverageComplete: false } } });
  const pendingThree = queueCoverage({ sync: { openCoverage: { id: 'coverage', scanId: 'scan-2', scope: 'all-open', cursor: 'page-3', done: false, pages: 3, skipped: 0, unknownDates: 0, coverageComplete: false } } });
  assert.equal(pendingTwo.pending, true); assert.equal(pendingThree.pending, true); assert.notEqual(pendingTwo.signature, pendingThree.signature);

  const canonicalIncomplete = queueCoverage({ sync: {
    openCoverage: { id: 'coverage', scanId: 'scan-current', scope: 'all-open', cursor: null, done: true, pages: 1, skipped: 1, unknownDates: 0, coverageComplete: false },
    openFrontier: { id: 'old-frontier', cursor: null, done: true, pages: 99, skipped: 0, unknownDates: 0, coverageComplete: true }
  } });
  assert.equal(canonicalIncomplete.source, 'openCoverage'); assert.equal(canonicalIncomplete.complete, false); assert.equal(canonicalIncomplete.reason, 'open-frontier-incomplete-evidence');

  const legacy = queueCoverage({ sync: { openFrontier: { id: 'legacy', cursor: null, done: true, pages: 1, skipped: 0, unknownDates: 0, coverageComplete: true } } });
  assert.equal(legacy.source, 'openFrontier'); assert.equal(legacy.complete, true);
});

test('queue summary keeps operation outcomes and unresolved slice items disjoint', () => {
  const counts = summarizeQueue({ slices: [
    {
      itemIds: ['reply', 'close', 'failed', 'unknown', 'stale', 'held'], status: 'complete', error: null,
      child: {
        proposals: [
          { id: 'p-reply', itemId: 'reply', kind: 'reply_and_close' },
          { id: 'p-close', itemId: 'close', kind: 'close' },
          { id: 'p-failed', itemId: 'failed', kind: 'close' },
          { id: 'p-unknown', itemId: 'unknown', kind: 'reply_and_close' },
          { id: 'p-stale', itemId: 'stale', kind: 'reply_and_close' }
        ],
        operations: [
          { id: 'o-reply', proposalId: 'p-reply', status: 'succeeded' },
          { id: 'o-close', proposalId: 'p-close', status: 'succeeded' },
          { id: 'o-failed', proposalId: 'p-failed', status: 'failed' },
          { id: 'o-unknown', proposalId: 'p-unknown', status: 'unknown' },
          { id: 'o-unknown', proposalId: 'p-unknown', status: 'unknown' },
          { id: 'o-stale', proposalId: 'p-stale', status: 'stale' }
        ]
      }
    },
    { itemIds: ['transport'], status: 'unknown', error: { code: 'UNKNOWN_MUTATION_OUTCOME' }, child: { proposals: [], operations: [] } },
    { itemIds: ['slice-failure'], status: 'held', error: { code: 'STALE_OR_CONFLICT' }, child: { proposals: [], operations: [] } },
    { itemIds: ['missing-operation'], status: 'complete', error: null, child: { executeJobId: 'execute-job', proposals: [{ id: 'p-missing', itemId: 'missing-operation', kind: 'close' }], operations: [] } }
  ] });
  assert.deepEqual(counts, {
    verifiedReplies: 1, verifiedNoReply: 1, failed: 1, unknown: 1, stale: 1, held: 1,
    unresolvedTransport: 1, sliceFailures: 1, unresolvedItems: 1, prepared: 6, slices: 4
  });
});

// These cases observe a synthetic, already acknowledged execute job through
// in-memory reads. Any mutation, network transport or child CLI is an error.
for (const [name, change, expectedMode, expectedCode, evidence] of [
  ['complete original operation', row => [row], 'complete', 0, { invalidBindingOperationIds: [] }],
  ['known failed original operation', row => [{ ...row, status: 'failed' }], 'completed-with-failures', 1, { invalidBindingOperationIds: [] }],
  ['missing approval binding', row => { const { approvalId, ...missing } = row; return [missing]; }, 'needs-reconciliation', 4, { invalidBindingOperationIds: ['op-original'] }],
  ['foreign approval binding', row => [{ ...row, approvalId: 'approval-foreign' }], 'needs-reconciliation', 4, { invalidBindingOperationIds: ['op-original'] }],
  ['foreign recipient binding', row => [{ ...row, itemId: 'item-foreign' }], 'needs-reconciliation', 4, { invalidBindingOperationIds: ['op-original'] }],
  ['replacement operation identity', row => [{ ...row, id: 'op-replacement' }], 'needs-reconciliation', 4, { invalidBindingOperationIds: ['op-replacement'] }],
  ['missing original operation', () => [], 'needs-reconciliation', 4, { missingProposalIds: ['proposal-original'] }],
  ['duplicate original proposal operations', row => [row, { ...row, id: 'op-duplicate' }], 'needs-reconciliation', 4, { duplicateProposalIds: ['proposal-original'] }],
  ['dispatch still in progress', row => [{ ...row, status: 'dispatching' }], 'needs-reconciliation', 4, { pendingOperationIds: ['op-original'] }],
  ['unknown external outcome', row => [{ ...row, status: 'unknown' }], 'needs-reconciliation', 4, { pendingOperationIds: ['op-original'] }],
  ['incomplete operation coverage', row => [row], 'needs-reconciliation', 4, { operationsComplete: false }]
]) {
  test(`headless repair pure: ${name} cannot change the original dispatch`, async () => {
    const proposal = { id: 'proposal-original', revision: 1, itemId: 'item-original', kind: 'reply_and_close' };
    const original = { id: 'op-original', proposalId: proposal.id, itemId: proposal.itemId, approvalId: 'approval-original', status: 'succeeded' };
    const mutations = []; const reads = [];
    const client = {
      account: 'LikeAvto', baseUrl: 'memory://headless-original-dispatch',
      getJob: async id => { reads.push(['job', id]); return { id, kind: 'execute', refId: original.approvalId, status: 'completed' }; },
      reviewItems: async ids => { reads.push(['review', ids]); return {
        operations: change(original), coverage: { operationsComplete: name !== 'incomplete operation coverage' }
      }; }
    };
    for (const method of ['prepareEngine', 'editorialReview', 'createApproval', 'execute', 'reconcile'])
      client[method] = async () => { mutations.push(method); throw new Error(`Unexpected mutation ${method}`); };
    const dir = await mkdtemp(join(tmpdir(), 'communityhero-headless-pure-')); const checkpoint = join(dir, 'original.json');
    await writeCheckpoint(checkpoint, { kind: 'communityhero-workflow', account: client.account, baseUrl: client.baseUrl,
      phase: 'executing', workflowId: 'workflow-original', itemIds: [proposal.itemId], proposals: [proposal],
      approvedProposals: [{ id: proposal.id, revision: proposal.revision }], approvalId: original.approvalId,
      executeRequestId: 'request-original', executeJobId: 'job-original', operations: [original] });
    const result = await runWorkflow(client, [], { resumePath: checkpoint, autonomous: true, execute: true,
      streamingChild: true, pollMs: 0, maxPolls: 1 });
    assert.equal(result.mode, expectedMode); assert.equal(resultExitCode(result), expectedCode);
    for (const [field, value] of Object.entries(evidence)) assert.deepEqual(result.checkpoint.operationEvidence[field], value);
    const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
    assert.equal(saved.approvalId, original.approvalId); assert.equal(saved.executeJobId, 'job-original');
    assert.equal(saved.executeRequestId, 'request-original'); assert.equal(saved.phase, expectedMode);
    assert.deepEqual(reads, [['job', 'job-original'], ['review', ['item-original']]]);
    assert.deepEqual(mutations, []);
  });
}

test('wrong account is rejected before any mutation', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const result = await run(['prepare', '--base-url', fake.url, '--account', 'BAW Russia', '--item', 'item-1']);
  assert.equal(result.code, 1); assert.match(result.stderr, /WRONG_ACCOUNT/);
  assert.equal(fake.state.requests.filter(row => row.method === 'POST').length, 0);
});

test('mutation checks compact status account before POST', async () => {
  const requests = [];
  const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async url => {
    requests.push(url.pathname);
    if (url.pathname === '/api/engine/status') return new Response(JSON.stringify({ account: 'baw-russia', displayAccount: 'BAW Russia' }), { status: 200 });
    throw new Error('unexpected request');
  } });
  await assert.rejects(client.createApproval([{ id: 'proposal', revision: 1 }]), { code: 'WRONG_ACCOUNT' });
  assert.deepEqual(requests, ['/api/engine/status']);
});

test('read deadlines remain distinguishable from connection failures without replaying mutations', async () => {
  let calls = 0;
  const client = new CommunityHeroClient({ account: 'BAW Russia', timeoutMs: 15_000,
    fetchImpl: async () => { calls += 1; throw new DOMException('deadline', 'TimeoutError'); } });
  await assert.rejects(client.request('/api/health'), error => {
    assert.equal(error.code, 'NETWORK_ERROR');
    assert.equal(error.details.causeName, 'TimeoutError');
    assert.equal(error.details.timeoutMs, 15_000);
    assert.doesNotMatch(error.message, /Cannot reach/);
    return true;
  });
  await assert.rejects(client.request('/api/test', { method: 'POST', csrf: false }), UnknownMutationError);
  assert.equal(calls, 2);
});

test('a network failure on mutation is unknown and is never reissued by the client', async () => {
  let mutationCalls = 0;
  const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async url => {
    if (url.pathname === '/api/engine/status') return new Response(JSON.stringify({ account: 'LikeAvto' }), { status: 200 });
    if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 });
    mutationCalls += 1; throw Object.assign(new Error('socket closed'), { code: 'ECONNRESET' });
  } });
  await assert.rejects(client.createProposal({ itemId: 'x', expectedRevision: 1, kind: 'close', text: '' }), UnknownMutationError);
  assert.equal(mutationCalls, 1);
});

test('ambiguous mutation responses are unknown for invalid success JSON, HTTP 500, and body read failure', async () => {
  const cases = [
    () => new Response('not-json', { status: 200 }),
    () => new Response(JSON.stringify({ error: 'late failure' }), { status: 500 }),
    () => ({ ok: true, status: 200, text: async () => { throw Object.assign(new Error('body reset'), { code: 'ECONNRESET' }); } })
  ];
  for (const mutationResponse of cases) {
    let mutations = 0;
    const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async url => {
      if (url.pathname === '/api/engine/status') return new Response(JSON.stringify({ account: 'LikeAvto' }), { status: 200 });
      if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 });
      mutations += 1; return mutationResponse();
    } });
    await assert.rejects(client.createProposal({ itemId: 'x', expectedRevision: 1, kind: 'close', text: '' }), UnknownMutationError);
    assert.equal(mutations, 1);
  }
});

test('UNKNOWN prepare checkpoints retain safe timeout, HTTP status, and body-read phase without retry', async () => {
  const cases = [
    { name: 'AbortTimeout', response: async () => { throw new DOMException('PRIVATE_TIMEOUT', 'TimeoutError'); },
      expected: { cause: 'TimeoutError', causeName: 'TimeoutError', timeoutMs: 17_000, phase: 'request-headers' } },
    { name: 'HTTP500', response: async () => new Response(JSON.stringify({ error: 'PRIVATE_HTTP_BODY' }), { status: 500 }),
      expected: { cause: 'HTTP_500', status: 500, phase: 'response-status' } },
    { name: 'BodyFailure', response: async () => ({ ok: true, status: 200,
      text: async () => { throw Object.assign(new Error('PRIVATE_BODY_ERROR'), { code: 'ECONNRESET' }); } }),
      expected: { cause: 'ECONNRESET', causeName: 'Error', status: 200, phase: 'response-body' } }
  ];
  for (const scenario of cases) {
    const dir = await mkdtemp(join(tmpdir(), 'communityhero-unknown-'));
    const checkpoint = join(dir, `${scenario.name}.json`);
    let mutationCalls = 0;
    const transport = new CommunityHeroClient({ account: 'BAW Russia', timeoutMs: 17_000,
      fetchImpl: async () => { mutationCalls += 1; return scenario.response(); } });
    const client = { account: 'BAW Russia', baseUrl: transport.baseUrl,
      requireStrictPreparation: async () => ({ version: 1, contract: 'strict_post_family_v1' }),
      reviewItems: async () => ({ items: [{ id: 'item-1' }] }),
      prepareEngine: body => transport.request('/api/engine/prepare', { method: 'POST', body, csrf: false }) };
    await assert.rejects(runWorkflow(client, ['item-1'], { checkpointPath: checkpoint,
      materialsAlreadyRefreshed: true, planAlreadyChecked: true }), UnknownMutationError);
    const savedText = await readFile(checkpoint, 'utf8'); const saved = JSON.parse(savedText);
    assert.equal(saved.phase, 'unknown', scenario.name);
    assert.equal(saved.error.code, 'UNKNOWN_MUTATION_OUTCOME');
    assert.deepEqual(saved.error.details, scenario.expected);
    assert.equal(saved.error.status ?? null, scenario.expected.status ?? null);
    assert.doesNotMatch(savedText, /PRIVATE_TIMEOUT|PRIVATE_HTTP_BODY|PRIVATE_BODY_ERROR/);
    await assert.rejects(runWorkflow(client, [], { resumePath: checkpoint }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
    assert.equal(mutationCalls, 1, `${scenario.name} must not be resent`);
  }
});

test('CLI stderr and checkpoint expose HTTP 500 diagnostics without the response body or replay', async t => {
  const fake = await fakeServer({ ...canonicalPolicy, prepareFailureStatus: 500 }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-unknown-'));
  const checkpoint = join(dir, 'prepare.json');
  const first = await run(['run', '--base-url', fake.url, '--account', 'baw-russia', '--item', 'item-1', '--checkpoint', checkpoint]);
  assert.equal(first.code, 4, first.stderr);
  const diagnostic = JSON.parse(first.stderr.trim().split(/\r?\n/u).at(-1)).error;
  assert.equal(diagnostic.code, 'UNKNOWN_MUTATION_OUTCOME');
  assert.equal(diagnostic.status, 500);
  assert.equal(diagnostic.details.status, 500);
  assert.equal(diagnostic.details.phase, 'response-status');
  assert.equal(diagnostic.details.cause, 'HTTP_500');
  assert.doesNotMatch(first.stderr, /PRIVATE_HTTP_BODY/);
  const saved = JSON.parse(await readFile(checkpoint, 'utf8'));
  assert.equal(saved.phase, 'unknown');
  assert.deepEqual(saved.error.details, { cause: 'HTTP_500', phase: 'response-status', status: 500 });
  const resumed = await run(['run', '--base-url', fake.url, '--account', 'baw-russia', '--resume', checkpoint]);
  assert.equal(resumed.code, 4);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/engine/prepare').length, 1);
});

test('queue UNKNOWN sync checkpoint retains the bounded transport cause', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-queue-unknown-'));
  const checkpoint = join(dir, 'queue.json');
  const cause = new UnknownMutationError('POST', '/api/sync', new DOMException('PRIVATE_TIMEOUT', 'TimeoutError'),
    { phase: 'request-headers', timeoutMs: 15_000 });
  await assert.rejects(freshSync({ account: 'BAW Russia', sync: async () => { throw cause; } },
    { account: 'BAW Russia', baseUrl: 'http://127.0.0.1:4186' }, checkpoint, {}), error => error === cause);
  const savedText = await readFile(checkpoint, 'utf8'); const saved = JSON.parse(savedText);
  assert.equal(saved.phase, 'unknown');
  assert.deepEqual(saved.error.details, { cause: 'TimeoutError', causeName: 'TimeoutError',
    timeoutMs: 15_000, phase: 'request-headers' });
  assert.doesNotMatch(savedText, /PRIVATE_TIMEOUT/);
});

test('UNKNOWN diagnostic serializer drops unrecognized exception fields', () => {
  const error = new UnknownMutationError('POST', '/api/engine/prepare',
    { code: 'PRIVATE_SECRET', name: 'PRIVATE_EXCEPTION', message: 'PRIVATE_MESSAGE' });
  const saved = checkpointError(error);
  assert.deepEqual(saved.details, { cause: 'network', phase: 'response-contract' });
  assert.doesNotMatch(JSON.stringify(saved), /PRIVATE_/u);
});

test('bounded polling honors stop without cancelling or retrying the server job', async () => {
  let reads = 0; const controller = new AbortController();
  const client = { bootstrap: async () => { reads += 1; if (reads === 1) controller.abort(); return { jobs: [{ id: 'job', status: 'running' }] }; } };
  await assert.rejects(waitForJob(client, 'job', { pollMs: 1, maxPolls: 10, signal: controller.signal }), { code: 'STOPPED' });
  assert.equal(reads, 1);
});

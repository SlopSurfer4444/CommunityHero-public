import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { execFile } from 'node:child_process';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { CommunityHeroClient, UnknownMutationError, waitForJob } from '../cli/client.mjs';
import { queueCoverage, summarizeQueue } from '../cli/queue.mjs';

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
    approvals: [], operations: [], jobs: [], materials: [], requests: [], executeOutcome: 'succeeded', scanCalls: 0, syncCalls: 0, prepareCalls: 0, frontierCompleteAfter: 1, enginePrepare: false, ...overrides
  };
  const server = http.createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : null;
    state.requests.push({ method: request.method, path: request.url, body, csrf: request.headers['x-csrf-token'], cookie: request.headers.cookie });
    const send = (status, value) => { response.writeHead(status, { 'content-type': 'application/json' }); response.end(JSON.stringify(value)); };
    if (request.url === '/api/health') return send(200, { status: 'ok', account: state.account });
    if (request.url === '/api/session') return send(200, { id: 'operator', role: 'owner', csrfToken: 'csrf-secret' });
    if (request.url === '/api/bootstrap') return send(200, workspace(state));
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
    if (request.url === '/api/materials/import' && request.method === 'POST') {
      if (Object.hasOwn(state, 'materialsResponse')) return send(200, state.materialsResponse);
      state.materials.push({ id: 'import-policy', kind: 'knowledge', imported: true });
      const job = { id: 'materials-job', kind: 'materials', status: 'completed', result: { imported: 1 } }; state.jobs.push(job);
      return send(200, { jobId: job.id });
    }
    if (request.url === '/api/engine/prepare' && request.method === 'POST' && state.enginePrepare) {
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
      approval.status = 'consumed'; const job = { id: 'execute-job', kind: 'execute', status: 'completed' }; state.jobs.push(job);
      for (const ref of approval.proposals) state.operations.push({ id: `op-${ref.id}`, proposalId: ref.id, status: state.executeOutcome });
      state.version += 1; return send(200, { jobId: job.id });
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
  assert.equal(fake.state.requests.filter(row => row.path === '/api/conversations/headless-chat/messages').length, 1);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/proposals').length, 0);
  assert.equal(fake.state.requests.filter(row => row.path === '/api/approvals').length, 0);
  assert.doesNotMatch(await readFile(checkpoint, 'utf8'), /very-secret|csrf-secret/);
  const mutation = fake.state.requests.find(row => row.path.endsWith('/messages'));
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

test('resume approves only the proposal created by its checkpoint and does not resend prepare', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  assert.equal((await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--checkpoint', checkpoint])).code, 0);
  const resumed = await run(['run', '--base-url', fake.url, '--account', 'LikeAvto', '--resume', checkpoint, '--autonomous']);
  assert.equal(resumed.code, 0, resumed.stderr);
  assert.equal(JSON.parse(resumed.stdout).mode, 'approved');
  assert.equal(fake.state.requests.filter(row => row.path.endsWith('/messages')).length, 1);
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

test('drain executes its exact approval and reconciles an unknown outcome without resending execute', async t => {
  const fake = await fakeServer({ executeOutcome: 'unknown' }); t.after(fake.close);
  const dir = await mkdtemp(join(tmpdir(), 'communityhero-cli-')); const checkpoint = join(dir, 'run.json');
  const result = await run(['drain', '--base-url', fake.url, '--account', 'LikeAvto', '--item', 'item-1', '--autonomous', '--execute', '--checkpoint', checkpoint]);
  assert.equal(result.code, 0, result.stderr); assert.equal(JSON.parse(result.stdout).mode, 'complete');
  assert.equal(fake.state.requests.filter(row => /\/execute$/.test(row.path)).length, 1);
  assert.equal(fake.state.requests.filter(row => /\/reconcile$/.test(row.path)).length, 1);
  assert.equal(fake.state.operations[0].status, 'succeeded');
});

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
  assert.deepEqual(fake.state.requests.filter(row => row.path.endsWith('/messages')).map(row => row.body.itemIds), [['oldest', 'middle'], ['newest']]);
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

test('wrong account is rejected before any mutation', async t => {
  const fake = await fakeServer(); t.after(fake.close);
  const result = await run(['prepare', '--base-url', fake.url, '--account', 'BAW Russia', '--item', 'item-1']);
  assert.equal(result.code, 1); assert.match(result.stderr, /WRONG_ACCOUNT/);
  assert.equal(fake.state.requests.filter(row => row.method === 'POST').length, 0);
});

test('a network failure on mutation is unknown and is never reissued by the client', async () => {
  let mutationCalls = 0;
  const client = new CommunityHeroClient({ account: 'LikeAvto', fetchImpl: async url => {
    if (url.pathname === '/api/bootstrap') return new Response(JSON.stringify({ account: 'LikeAvto' }), { status: 200 });
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
      if (url.pathname === '/api/bootstrap') return new Response(JSON.stringify({ account: 'LikeAvto' }), { status: 200 });
      if (url.pathname === '/api/session') return new Response(JSON.stringify({ csrfToken: 'csrf' }), { status: 200 });
      mutations += 1; return mutationResponse();
    } });
    await assert.rejects(client.createProposal({ itemId: 'x', expectedRevision: 1, kind: 'close', text: '' }), UnknownMutationError);
    assert.equal(mutations, 1);
  }
});

test('bounded polling honors stop without cancelling or retrying the server job', async () => {
  let reads = 0; const controller = new AbortController();
  const client = { bootstrap: async () => { reads += 1; if (reads === 1) controller.abort(); return { jobs: [{ id: 'job', status: 'running' }] }; } };
  await assert.rejects(waitForJob(client, 'job', { pollMs: 1, maxPolls: 10, signal: controller.signal }), { code: 'STOPPED' });
  assert.equal(reads, 1);
});

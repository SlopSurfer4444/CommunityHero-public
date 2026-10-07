import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { CliError, CommunityHeroClient, localAdmissionPayloadHash } from '../cli/client.mjs';
import { generateProposals, runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { PREPARE_REVIEW_ONLY, READY_FOR_OWNER_APPROVAL, nativeProgress } from '../cli/read-observer.mjs';

const running = { account: 'LikeAvto', baseUrl: 'http://localhost:4186', phase: 'assistant-running',
  itemIds: ['i1'], instruction: 'Synthetic preparation', proposals: [], materialsReady: true,
  prepareJobId: 'prepare-job', prepareRequestId: 'prepare-key', prepareTransport: 'engine', pendingLocalAdmission: null };
async function fixture(t, state = running) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-prepare-checkpoint-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'checkpoint.json'); await writeCheckpoint(path, state); return path;
}
const saved = async path => JSON.parse(await readFile(path, 'utf8'));
function api(getJob) {
  return { account: running.account, baseUrl: running.baseUrl, getJob,
    reviewItems: async () => ({ items: [{ id: 'i1' }], proposals: [{ id: 'p1', itemId: 'i1', revision: 1,
      status: 'draft', prepareRunId: 'prepare-job', kind: 'reply_and_close', text: 'Synthetic text' }] }),
    prepareEngine: () => assert.fail('an existing attempt must not be posted again'),
    createConversation: () => assert.fail('no fallback preparation'),
    sendConversationMessage: () => assert.fail('no legacy model call'),
    createApproval: () => assert.fail('no approval after preparation failure'),
    execute: () => assert.fail('no execution after preparation failure') };
}

test('named prepare-review-only exposes settled native units before the other unit, then retains exact owner-ready refs without approval', async t => {
  const path = await fixture(t, { ...running, workflowMode: PREPARE_REVIEW_ONLY, itemIds: ['i1', 'i2'] });
  const proposals = [1, 2].map(n => ({ id: `p${n}`, revision: 1, itemId: `i${n}`, kind: 'reply_and_close', text: `Synthetic ${n}`, status: 'draft', prepareRunId: running.prepareJobId }));
  const ready = { id: 'p1', revision: 1, itemId: 'i1', textSha256: 'a'.repeat(64), receiptSha256: 'b'.repeat(64) };
  const held = [{ reference: { id: 'p2', revision: 1 }, decision: 'hold', reason: 'Mandatory material remains held' }];
  let editorialKey; let admissions = 0; let polls = 0; let current = [ready]; const events = [];
  const client = { account: running.account, baseUrl: running.baseUrl, editorialReviewSupportsFresh: true,
    reviewItems: async () => ({ items: [{ id: 'i1' }, { id: 'i2' }], proposals, operations: [], coverage: { operationsComplete: true }, readyForOwnerApproval: current,
      progress: { version: 1, counters: { backlog: null, knownReady: 1 }, source: { coverageComplete: false } } }),
    prepareEngine: () => assert.fail('Original paid preparation cannot be replayed'),
    createApproval: () => assert.fail('Prepare-only never creates approval'), execute: () => assert.fail('Prepare-only never executes'),
    editorialReview: async (refs, requestId, { fresh }) => {
      assert.equal(fresh, true); assert.deepEqual(refs, proposals.map(({ id, revision }) => ({ id, revision })));
      assert.equal((await saved(path)).workflowMode, PREPARE_REVIEW_ONLY); editorialKey = requestId; admissions++;
      return { jobId: 'editorial-native', requestId, replayed: false };
    },
    getJob: async id => {
      if (id === running.prepareJobId) return { id, status: 'completed', workflowMode: PREPARE_REVIEW_ONLY };
      assert.equal(id, 'editorial-native'); polls++;
      const complete = polls > 1;
      return { id, kind: 'editorial_review', refId: editorialKey, status: complete ? 'completed' : 'running',
        editorialProgress: { version: 1, contract: 'communityhero-editorial-progress-v1', accepted: [{ id: 'p1', revision: 1 }], reused: [],
          held: complete ? held : [], pending: complete ? [] : [{ id: 'p2', revision: 1 }], readyForOwnerApproval: [ready],
          completedUnits: complete ? 2 : 1, totalUnits: 2, complete, approvalRequired: true, dispatchAuthorized: false, retryAllowed: false },
        ...(complete ? { editorialOutcome: { accepted: [{ id: 'p1', revision: 1 }], reused: [], held } } : {}) };
    } };
  const result = await runWorkflow(client, [], { resumePath: path, pollMs: 0, onProgress: event => {
    events.push(event);
    if (event.event === 'editorial.ready' && event.pending.length) { assert.equal(polls, 1); assert.deepEqual(event.references, [ready]); }
  } });
  assert.equal(result.mode, READY_FOR_OWNER_APPROVAL); assert.deepEqual(result.readyReferences, [ready]); assert.equal(result.held.length, 1);
  assert.equal(result.checkpoint.progress.counters.backlog, null); assert.equal(admissions, 1);
  assert.ok(events.some(event => event.event === 'editorial.ready' && event.pending.length === 1));
  await runWorkflow(client, [], { resumePath: path, pollMs: 0 }); assert.equal(admissions, 1); assert.equal(polls, 2);
  await assert.rejects(runWorkflow(client, [], { resumePath: path, execute: true, autonomous: true }), { code: 'USAGE' });
  current = [];
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'INVALID_REVIEW_COVERAGE' });
  assert.equal((await saved(path)).phase, 'editorial-held'); assert.equal((await saved(path)).readyCurrent, false);
  assert.equal(admissions, 1, 'stale readiness does not re-pay editorial automatically');
});

test('legacy checkpoint cannot be promoted to a new prepare-review-only workflow mode', async t => {
  const path = await fixture(t);
  await assert.rejects(runWorkflow(api(() => assert.fail('No reads before mode validation')), [], {
    resumePath: path, workflowMode: PREPARE_REVIEW_ONLY }), { code: 'INVALID_CHECKPOINT' });
});

test('new prepare-review-only persists its mode and workflow identity before the first native admission', async t => {
  const path = `${await fixture(t)}.new`; let prepareCalls = 0; let requestId; let editorialKey;
  const proposal = { id: 'p1', revision: 1, itemId: 'i1', kind: 'reply_and_close', text: 'Synthetic', status: 'draft', prepareRunId: 'new-prepare' };
  const ready = { id: 'p1', revision: 1, itemId: 'i1', textSha256: 'a'.repeat(64), receiptSha256: 'b'.repeat(64) };
  const client = { account: running.account, baseUrl: running.baseUrl, editorialReviewSupportsFresh: true,
    requireStrictPreparation: async () => ({}),
    reviewItems: async () => ({ items: [{ id: 'i1' }], proposals: [proposal], operations: [], coverage: { operationsComplete: true }, readyForOwnerApproval: [ready] }),
    prepareEngine: async body => {
      const state = await saved(path); assert.equal(state.workflowMode, PREPARE_REVIEW_ONLY);
      assert.match(state.workflowId, /^[a-f0-9-]{36}$/u); assert.equal(state.pendingLocalAdmission.payloadHash, localAdmissionPayloadHash(body));
      assert.equal(body.workflowMode, PREPARE_REVIEW_ONLY); requestId = body.requestId; prepareCalls++;
      return { jobId: 'new-prepare', requestId };
    },
    editorialReview: async (_refs, key, options) => { assert.equal(options.fresh, true); editorialKey = key; return { jobId: 'new-editorial', requestId: key, replayed: false }; },
    getJob: async id => id === 'new-prepare' ? { id, status: 'completed', workflowMode: PREPARE_REVIEW_ONLY }
      : { id, kind: 'editorial_review', refId: editorialKey, status: 'completed', editorialOutcome: { accepted: [{ id: 'p1', revision: 1 }], reused: [], held: [] } },
    createApproval: () => assert.fail('No approval'), execute: () => assert.fail('No execution') };
  const result = await runWorkflow(client, ['i1'], { workflowMode: PREPARE_REVIEW_ONLY, checkpointPath: path,
    materialsAlreadyRefreshed: true, planAlreadyChecked: true, pollMs: 0 });
  assert.equal(result.mode, READY_FOR_OWNER_APPROVAL); assert.equal(prepareCalls, 1); assert.equal(result.checkpoint.prepareRequestId, requestId);
});

test('prepare-review-only rejects a foreign native mode and preserves original admission without a replay', async t => {
  const path = await fixture(t, { ...running, workflowMode: PREPARE_REVIEW_ONLY }); let polls = 0;
  const client = api(async id => { polls++; return { id, status: 'completed', workflowMode: 'execute' }; });
  await assert.rejects(runWorkflow(client, [], { resumePath: path, pollMs: 0 }), { code: 'INVALID_PREPARE_JOB' });
  assert.equal(polls, 1); assert.equal((await saved(path)).prepareJobId, running.prepareJobId);
});

test('native progress preserves unknown coverage and unavailable counters as null', () => {
  const progress = nativeProgress({ version: 1, counters: { missing: undefined, unknown: null, knownEmpty: 0 }, coverageComplete: false });
  assert.deepEqual(progress.counters, { missing: null, unknown: null, knownEmpty: 0 });
  assert.equal(progress.coverageComplete, false); assert.equal(nativeProgress(null).counters, null);
});

for (const code of ['NETWORK_ERROR', 'READ_TIMEOUT']) {
  test(`acknowledged preparation recovers two transient ${code} reads without a paid replay`, async t => {
    const path = await fixture(t); let reads = 0;
    const client = api(async id => {
      assert.equal(id, running.prepareJobId);
      if (++reads <= 2) throw new CliError('Transient read loss', { code });
      return { id, status: 'completed' };
    });
    const result = await generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
      recoverPreparationReads: true, pollMs: 0, maxPolls: 1 });
    assert.equal(result.phase, 'prepared'); assert.equal(reads, 3);
    assert.equal(result.prepareJobId, running.prepareJobId); assert.equal(result.prepareRequestId, running.prepareRequestId);
    assert.equal(result.proposals.length, 1);
  });
}

test('preparation read budget is shared across exact job and durable group discovery', async t => {
  const path = await fixture(t); let polls = 0; let reviews = 0;
  const client = api(async id => {
    if (++polls === 1) throw new CliError('Transient job loss', { code: 'NETWORK_ERROR' });
    return { id, kind: 'assistant', purpose: 'engine_prepare', status: 'completed',
      preparationStages: { groupAdmission: [{ itemIds: ['i1'], status: 'admitted',
        admission: { candidates: [{ itemId: 'i1', status: 'review', proposalId: 'p1' }] } }] } };
  });
  const initialReview = client.reviewItems;
  client.reviewItems = async ids => {
    if (++reviews === 1) return initialReview(ids);
    throw new CliError('PRIVATE_READ_DETAIL', { code: 'HTTP_ERROR', status: 503 });
  };
  await assert.rejects(generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
    recoverPreparationReads: true, pollMs: 0 }), { code: 'HTTP_ERROR' });
  assert.equal(polls, 2); assert.equal(reviews, 3);
  const state = await saved(path); assert.equal(state.phase, 'stopped'); assert.equal(state.prepareJobId, running.prepareJobId);
  assert.doesNotMatch(await readFile(path, 'utf8'), /PRIVATE_READ_DETAIL/);
});

test('preparation read recovery does not replay a ready-child callback', async t => {
  const path = await fixture(t); let polls = 0; let callbacks = 0;
  const client = api(async id => { polls++; return { id, kind: 'assistant', purpose: 'engine_prepare', status: 'completed',
    preparationStages: { groupAdmission: [{ itemIds: ['i1'], status: 'admitted',
      admission: { candidates: [{ itemId: 'i1', status: 'review', proposalId: 'p1' }] } }] } }; });
  const review = client.reviewItems; client.reviewItems = async ids => ({ ...await review(ids), operations: [], coverage: { operationsComplete: true } });
  await assert.rejects(generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
    recoverPreparationReads: true, pollMs: 0,
    onReady: async () => { callbacks++; throw new CliError('Child read/effect uncertain', { code: 'NETWORK_ERROR' }); } }), { code: 'READY_BATCH_STOPPED' });
  assert.equal(polls, 1); assert.equal(callbacks, 1);
  assert.equal((await saved(path)).proposals.length, 1);
});

test('preparation read recovery never retries authority denial or a different job', async t => {
  for (const wrongJob of [false, true]) {
    const path = await fixture(t); let reads = 0;
    const client = api(async () => {
      reads++;
      if (wrongJob) return { id: 'other-job', status: 'completed' };
      throw new CliError('Denied', { code: 'HTTP_ERROR', status: 403 });
    });
    await assert.rejects(generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
      recoverPreparationReads: true, pollMs: 0 }), { code: wrongJob ? 'JOB_NOT_FOUND' : 'HTTP_ERROR' });
    assert.equal(reads, 1);
  }
});

test('acknowledged initial and final preparation reads use one budget and persist bounded exhaustion', async t => {
  const path = await fixture(t); let reviews = 0; let polls = 0;
  const client = api(async id => { polls++; return { id, status: 'completed' }; });
  const review = client.reviewItems;
  client.reviewItems = async ids => {
    reviews++;
    if (reviews === 2) return review(ids);
    throw new CliError('PRIVATE_INITIAL_OR_FINAL', { code: 'HTTP_ERROR', status: 503, details: { secret: 'PRIVATE_TOKEN' } });
  };
  await assert.rejects(generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
    recoverPreparationReads: true, pollMs: 0 }), { code: 'HTTP_ERROR', status: 503 });
  assert.equal(reviews, 4); assert.equal(polls, 1);
  const state = await saved(path); assert.equal(state.phase, 'stopped'); assert.equal(state.prepareJobId, running.prepareJobId);
  assert.doesNotMatch(await readFile(path, 'utf8'), /PRIVATE_/);
});

test('cancellation during preparation retry wait stops before another exact-job read', async t => {
  const path = await fixture(t); const controller = new AbortController(); let reads = 0;
  const client = api(async () => {
    reads++; setTimeout(() => controller.abort(), 5);
    throw new CliError('Transient', { code: 'NETWORK_ERROR' });
  });
  await assert.rejects(generateProposals(client, [], { checkpoint: await saved(path), checkpointPath: path,
    recoverPreparationReads: true, pollMs: 1000, signal: controller.signal }), { code: 'STOPPED' });
  assert.equal(reads, 1); assert.equal((await saved(path)).phase, 'stopped');
});

for (const status of ['failed', 'error', 'cancelled', 'interrupted']) {
  test(`terminal ${status} preparation is durably reported once; resume is a local hard stop`, async t => {
    const path = await fixture(t); const events = []; let polls = 0;
    const client = api(async id => { polls += 1; return { id, kind: 'assistant', purpose: 'engine_prepare', status,
      finishedAt: '2026-09-27T08:00:00.123Z', error: 'PRIVATE_RAW_MODEL_ERROR', prepareBundle: { secret: 'PRIVATE_BUNDLE_TEXT' } }; });
    const failure = await runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true,
      onProgress: event => events.push(event) }).then(() => assert.fail('failure expected'), error => error);
    assert.equal(failure.code, 'JOB_FAILED'); assert.equal(failure.details.status, status);
    const checkpoint = await saved(path);
    assert.equal(checkpoint.schemaVersion, 1); assert.equal(checkpoint.phase, 'prepare-failed');
    assert.equal(checkpoint.stopReason, 'prepare-job-failed'); assert.equal(checkpoint.prepareJobId, running.prepareJobId);
    assert.equal(checkpoint.prepareRequestId, running.prepareRequestId); assert.equal(checkpoint.pendingLocalAdmission, null);
    assert.deepEqual(checkpoint.lastPrepareJob, { id: 'prepare-job', status, finishedAt: '2026-09-27T08:00:00.123Z' });
    assert.deepEqual(events.map(event => event.event), ['job.poll', 'prepare.failed']);
    const beforeResume = await readFile(path, 'utf8');
    assert.doesNotMatch(beforeResume + JSON.stringify(failure), /PRIVATE_|prepareBundle/);
    client.getJob = () => assert.fail('a recorded terminal failure does not need new polling');
    client.reviewItems = () => assert.fail('a recorded terminal failure must stop before other reads');
    await assert.rejects(runWorkflow(client, [], { resumePath: path, autonomous: true, execute: true }), { code: 'JOB_FAILED' });
    assert.equal(await readFile(path, 'utf8'), beforeResume); assert.equal(polls, 1);
  });
}

test('fresh admitted preparation records terminal failure without replaying its saved request', async t => {
  const path = `${await fixture(t)}.fresh`; let posts = 0;
  const client = api(async id => ({ id, status: 'failed' }));
  client.requireStrictPreparation = async () => ({ version: 1, contract: 'strict_post_family_v1' });
  client.prepareEngine = async body => {
    posts += 1; const checkpoint = await saved(path);
    assert.equal(checkpoint.pendingLocalAdmission.requestId, body.requestId);
    return { jobId: 'prepare-job', requestId: body.requestId, replayed: false };
  };
  await assert.rejects(generateProposals(client, ['i1'], { checkpointPath: path, materialsAlreadyRefreshed: true,
    planAlreadyChecked: true }), { code: 'JOB_FAILED' });
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'JOB_FAILED' });
  assert.equal(posts, 1); assert.equal((await saved(path)).phase, 'prepare-failed');
});

test('recovered UNKNOWN admission adopts only its original failed job and never repeats preparation', async t => {
  const payloadHash = localAdmissionPayloadHash({ itemIds: running.itemIds, instruction: running.instruction });
  const path = await fixture(t, { ...running, phase: 'unknown', prepareJobId: null,
    pendingLocalAdmission: { kind: 'prepare', requestId: running.prepareRequestId, payloadHash } });
  const client = api(async id => ({ id, status: 'failed' }));
  client.localAdmission = async (kind, requestId) => ({ kind, requestId, payloadHash, status: 'committed',
    result: { jobId: 'prepare-job', requestId, replayed: true } });
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'JOB_FAILED' });
  assert.equal((await saved(path)).phase, 'prepare-failed'); assert.equal((await saved(path)).prepareJobId, 'prepare-job');
});

for (const code of ['POLL_LIMIT', 'NETWORK_ERROR', 'READ_TIMEOUT', 'STOPPED']) {
  test(`${code} stops polling without declaring terminal failure, and resume polls the same admitted job`, async t => {
    const path = await fixture(t); const polls = [];
    const client = api(async id => {
      polls.push(id);
      if (code !== 'POLL_LIMIT') throw new CliError('Synthetic read interruption', { code });
      return { id, status: 'running' };
    });
    await assert.rejects(runWorkflow(client, [], { resumePath: path, maxPolls: 1, pollMs: 0 }), { code });
    const checkpoint = await saved(path); assert.equal(checkpoint.phase, 'stopped'); assert.equal(checkpoint.lastPrepareJob, undefined);
    assert.equal(checkpoint.prepareJobId, 'prepare-job'); assert.equal(checkpoint.prepareRequestId, 'prepare-key');
    client.getJob = async id => { polls.push(id); return { id, status: 'completed', prepareOutcome: { proposalsCreated: 1 } }; };
    const result = await runWorkflow(client, [], { resumePath: path, maxPolls: 1, pollMs: 0 });
    assert.equal(result.checkpoint.phase, 'prepared'); assert.equal(result.checkpoint.error, null);
    assert.equal(result.checkpoint.stopReason, null); assert.equal(result.checkpoint.proposals.length, 1);
    assert.deepEqual(polls, ['prepare-job', 'prepare-job']);
  });
}

test('wrong-job failure is not accepted as terminal evidence for the saved preparation', async t => {
  const path = await fixture(t); const client = api(async () => ({ id: 'other-job', status: 'failed' }));
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'JOB_NOT_FOUND' });
  assert.equal((await saved(path)).phase, 'stopped'); assert.equal((await saved(path)).lastPrepareJob, undefined);
});

test('HTTP500 polling response cannot persist or rethrow private server text and resume never posts', async t => {
  const path = await fixture(t); const calls = []; let failing = true;
  const client = new CommunityHeroClient({ account: running.account, baseUrl: running.baseUrl, fetchImpl: async (url, options) => {
    calls.push({ path: url.pathname, method: options.method });
    assert.equal(url.pathname, '/api/engine/jobs/prepare-job'); assert.equal(options.method, 'GET');
    return new Response(JSON.stringify(failing
      ? { error: 'PRIVATE_HTTP500_TEXT', details: { token: 'PRIVATE_HTTP500_SECRET' } }
      : { id: 'prepare-job', status: 'completed' }), { status: failing ? 500 : 200 });
  } });
  client.reviewItems = api().reviewItems;
  const failure = await runWorkflow(client, [], { resumePath: path }).then(() => assert.fail('failure expected'), error => error);
  assert.equal(failure.code, 'HTTP_ERROR'); assert.equal(failure.status, 500);
  assert.deepEqual(failure.details, { jobId: 'prepare-job' });
  const checkpoint = await saved(path); assert.equal(checkpoint.phase, 'stopped'); assert.equal(checkpoint.error.status, 500);
  assert.doesNotMatch((await readFile(path, 'utf8')) + JSON.stringify(failure) + failure.message, /PRIVATE_HTTP500/);
  failing = false;
  const resumed = await runWorkflow(client, [], { resumePath: path }); assert.equal(resumed.checkpoint.phase, 'prepared');
  assert.deepEqual(calls, [{ path: '/api/engine/jobs/prepare-job', method: 'GET' }, { path: '/api/engine/jobs/prepare-job', method: 'GET' }]);
});

test('uppercase FAILED terminal status is normalized without raw job error persistence or another POST', async t => {
  const path = await fixture(t); let polls = 0;
  const client = api(async id => { polls += 1; return { id, status: 'FAILED', error: 'PRIVATE_UPPERCASE_MODEL_ERROR', prepareBundle: { text: 'PRIVATE_UPPERCASE_BUNDLE' } }; });
  const failure = await runWorkflow(client, [], { resumePath: path }).then(() => assert.fail('failure expected'), error => error);
  const checkpoint = await saved(path); assert.equal(checkpoint.phase, 'prepare-failed'); assert.equal(checkpoint.lastPrepareJob.status, 'failed');
  assert.equal(failure.code, 'JOB_FAILED'); assert.equal(failure.details.status, 'failed');
  assert.doesNotMatch((await readFile(path, 'utf8')) + JSON.stringify(failure) + failure.message, /PRIVATE_UPPERCASE/);
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'JOB_FAILED' });
  assert.equal(polls, 1);
});

test('unrecognized polling error codes and attached data remain outside the checkpoint', async t => {
  const path = await fixture(t); const client = api(async () => { throw Object.assign(new Error('PRIVATE_ARBITRARY_MESSAGE'), {
    code: 'PRIVATE_ARBITRARY_CODE', status: 'PRIVATE_STATUS', details: { text: 'PRIVATE_DETAIL' }
  }); });
  const failure = await runWorkflow(client, [], { resumePath: path }).then(() => assert.fail('failure expected'), error => error);
  assert.equal(failure.code, 'PREPARE_POLL_FAILED'); assert.equal(failure.status, null);
  assert.doesNotMatch((await readFile(path, 'utf8')) + JSON.stringify(failure) + failure.message, /PRIVATE_/);
  assert.equal((await saved(path)).phase, 'stopped');
});

test('legacy UNKNOWN without a recoverable receipt remains unchanged and is never retried', async t => {
  const path = await fixture(t, { ...running, phase: 'unknown', prepareJobId: null });
  const before = await readFile(path, 'utf8'); const client = api(() => assert.fail('unknown must not poll a guessed job'));
  client.reviewItems = () => assert.fail('unknown must stop before reads');
  await assert.rejects(runWorkflow(client, [], { resumePath: path }), { code: 'UNKNOWN_MUTATION_OUTCOME' });
  assert.equal(await readFile(path, 'utf8'), before);
});

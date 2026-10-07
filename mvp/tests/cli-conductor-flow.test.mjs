import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runQueue } from '../cli/queue.mjs';
import { runWorkflow, writeCheckpoint } from '../cli/workflow.mjs';
import { CliError, CommunityHeroClient, localAdmissionPayloadHash, UnknownMutationError } from '../cli/client.mjs';

const read = async path => JSON.parse(await readFile(path, 'utf8'));
const digest = 'a'.repeat(64);
const ref = row => ({ id: row.id, revision: row.revision });

async function fixture(t, { loseRepair = false, unconfirmedRepair = false, repeat = false, changingProof = false, unknown = false } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-conductor-flow-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'queue.json'); const jobs = new Map(); const receipts = new Map(); const approvals = new Map();
  const calls = []; const proposals = []; const operations = [];
  const items = ['repair', 'private', 'ready', 'outside', 'late'].map((id, index) => ({ id, workflow: 'attention', providerStatus: 'new',
    createdAt: id === 'late' ? '2026-10-02T00:00:00Z' : `2026-09-30T00:00:0${index}Z`, conversationKey: `branch:${id}` }));
  const snapshot = () => ({ account: 'BAW', items, proposals, operations, jobs: [...jobs.values()], materials: [{ kind: 'knowledge', imported: true }],
    sync: { openCoverage: { done: true, coverageComplete: true } } });
  const receipt = (kind, requestId, payload, result) => receipts.set(`${kind}:${requestId}`,
    { kind, requestId, status: 'committed', payloadHash: localAdmissionPayloadHash(payload), result });
  const client = { account: 'BAW', baseUrl: 'http://conductor.fixture.invalid', editorialReviewSupportsFresh: true,
    engineStatus: async () => ({ strictGrouping: { version: 1, contract: 'strict_post_family_v1' }, prepareScopeReservations: { version: 1 } }),
    requireStrictPreparation: CommunityHeroClient.prototype.requireStrictPreparation,
    bootstrap: async () => snapshot(), importMaterials: async () => ({ jobId: 'materials' }), sync: async () => ({ jobId: 'sync' }),
    planPrepare: async ids => ({ batches: [{ itemIds: ids, bytes: 1000 }], held: [] }),
    reviewItems: async ids => ({ items: items.filter(row => ids.includes(row.id)), proposals: proposals.filter(row => ids.includes(row.itemId)),
      operations: operations.filter(row => ids.includes(row.itemId)), coverage: { operationsComplete: true } }),
    prepareEngine: async payload => {
      calls.push(['prepare', payload.itemIds]); const jobId = `prepare-${payload.itemIds.join('-')}`;
      const scopeReservation = { version: 1, ownerJobId: jobId, keysDigest: digest };
      for (const itemId of payload.itemIds) {
        proposals.push({ id: `p-${itemId}`, itemId, revision: 1, text: `Synthetic ${itemId}`, kind: 'reply_and_close', status: 'draft', prepareRunId: jobId });
        items.find(row => row.id === itemId).workflow = 'prepared';
      }
      jobs.set(jobId, { id: jobId, kind: 'assistant', purpose: 'engine_prepare', status: 'completed', scopeReservation,
        preparationStages: { groupAdmission: payload.itemIds.map(itemId => ({ key: itemId, itemIds: [itemId], status: 'admitted',
          admission: { candidates: [{ itemId, status: 'review', proposalId: `p-${itemId}` }] } })) } });
      const result = { jobId, requestId: payload.requestId, replayed: false, scopeReservation };
      receipt('prepare', payload.requestId, { itemIds: payload.itemIds, instruction: payload.instruction }, result); return result;
    },
    getJob: async id => ['materials', 'sync'].includes(id) ? { id, status: 'completed' } : jobs.get(id),
    editorialReview: async (refs, requestId, options) => {
      assert.equal(options.fresh, true); calls.push(['editorial', refs]);
      const held = refs.flatMap(reference => {
        if (reference.id === 'p-private') return [{ reference, decision: 'hold', reason: 'Private order data needs a human' }];
        if (reference.id === 'p-repair' && (reference.revision === 1 || repeat)) return [{ reference, decision: 'revise', reason: 'Tone needs revision',
          repairExpected: { proposalId: reference.id, proposalRevision: reference.revision, textSha256: changingProof ? String(reference.revision).repeat(64) : digest, contextDigest: digest,
            rulesDigest: digest, receiptSha256: digest } }];
        return [];
      });
      const accepted = refs.filter(row => !held.some(hold => hold.reference.id === row.id));
      const jobId = `editorial-${requestId}`; const result = { jobId, requestId, replayed: false };
      jobs.set(jobId, { id: jobId, kind: 'editorial_review', refId: requestId, status: 'completed', editorialOutcome: { accepted, reused: [], held } });
      receipt('editorial', requestId, { proposals: refs, fresh: true }, result); return result;
    },
    editorialRepair: async (parentReviewJobId, expected, requestId) => {
      calls.push(['repair', expected]);
      for (const value of expected) {
        const row = proposals.find(row => row.id === value.proposalId);
        assert.equal(row.revision, value.proposalRevision); row.revision++; row.text = 'Canonical repaired text';
      }
      const result = { status: 'repaired', repairId: `repair-${requestId}`, parentReviewJobId, requestId, replayed: false,
        oldRefs: expected.map(row => ({ id: row.proposalId, revision: row.proposalRevision })),
        newRefs: expected.map(row => ({ id: row.proposalId, revision: row.proposalRevision + 1 })) };
      if (!unconfirmedRepair) receipt('editorial-repair', requestId, { reviewJobId: parentReviewJobId, expected }, result);
      if (loseRepair || unconfirmedRepair) throw new UnknownMutationError('POST', `/api/editorial-reviews/${parentReviewJobId}/repairs`, { code: 'TIMEOUT' });
      return result;
    },
    localAdmission: async (kind, requestId) => { calls.push(['receipt', kind]); return receipts.get(`${kind}:${requestId}`); },
    createApproval: async (refs, requestId) => {
      calls.push(['approve', refs]); const result = { id: `approval-${requestId}`, requestId, replayed: false };
      approvals.set(result.id, refs); receipt('approval', requestId, { proposals: refs }, result); return result;
    },
    execute: async (approvalId, requestId) => {
      const refs = approvals.get(approvalId); calls.push(['execute', refs]);
      for (const reference of refs) {
        const proposal = proposals.find(row => row.id === reference.id);
        operations.push({ id: `op-${reference.id}`, approvalId, proposalId: reference.id, itemId: proposal.itemId, status: unknown && proposal.itemId === 'repair' ? 'unknown' : 'succeeded' });
      }
      const jobId = `execute-${approvalId}`; jobs.set(jobId, { id: jobId, kind: 'execute', refId: approvalId, status: 'completed' });
      const result = { jobId, approvalId, requestId, replayed: false }; receipt('execute', requestId, { approvalId }, result); return result;
    },
    reconcile: async operationId => { const id = `readback-${operationId}`; jobs.set(id, { id, status: 'completed' }); return { jobId: id }; }
  };
  const options = { checkpointPath: path, autonomous: true, execute: true, scopeItemIds: ['repair', 'private', 'ready', 'late'],
    cutoffUtc: '2026-10-01T00:00:00Z', freshEditorial: true, maxRepairRounds: 2, continueHeld: true, batchSize: 3, maxCycles: 3, pollMs: 0 };
  return { path, client, calls, jobs, receipts, proposals, operations, items, options };
}

for (const readStage of ['job', 'result']) test(`acknowledged execute recovers transient ${readStage} observation and finishes independent scope without another effect`, async t => {
  const { client, calls, options } = await fixture(t);
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1; options.maxCycles = 10;
  let failures = 0; let terminalObserved = false;
  const getJob = client.getJob;
  client.getJob = async id => {
    if (id.startsWith('execute-') && readStage === 'job' && failures < 2) {
      failures++; throw new CliError('Read response lost after canonical execution', { code: 'NETWORK_ERROR' });
    }
    const job = await getJob(id);
    if (job?.kind === 'execute') terminalObserved = true;
    return job;
  };
  const review = client.reviewItems;
  client.reviewItems = async ids => {
    if (readStage === 'result' && terminalObserved && failures < 2) {
      failures++; throw new CliError('Temporary read unavailable', { code: 'HTTP_ERROR', status: 503 });
    }
    return review(ids);
  };
  const result = await runQueue(client, options);
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 2); assert.equal(failures, 2);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair', 'p-ready']);
  const parent = await read(result.checkpoint.slices[0].childPath);
  const files = await readdir(`${result.checkpoint.slices[0].childPath}.ready`);
  const child = await read(join(`${result.checkpoint.slices[0].childPath}.ready`, files[0]));
  assert.equal(child.phase, 'complete'); assert.equal(child.pendingLocalAdmission, null);
  assert.equal(child.executeJobId, `execute-${child.approvalId}`);
  assert.equal(child.operations[0].id, 'op-p-repair');
});

test('persistent execute observation failure exhausts three reads and preserves original admission with bounded diagnostic', async t => {
  const { client, calls, options } = await fixture(t);
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1;
  let reads = 0; const getJob = client.getJob;
  client.getJob = async id => {
    if (id.startsWith('execute-')) { reads++; throw new CliError('Secret raw response must not survive', { code: 'HTTP_ERROR', status: 503, details: { token: 'never-retain' } }); }
    return getJob(id);
  };
  const result = await runQueue(client, options);
  assert.equal(result.stopReason, 'overlapped-slice-unresolved'); assert.equal(reads, 3);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair']);
  assert.match(result.checkpoint.slices[0].error.message, /HTTP_ERROR\/503/u);
  assert.doesNotMatch(JSON.stringify(result.checkpoint), /Secret raw response|never-retain/u);
  const parent = await read(result.checkpoint.slices[0].childPath);
  const files = await readdir(`${result.checkpoint.slices[0].childPath}.ready`);
  const child = await read(join(`${result.checkpoint.slices[0].childPath}.ready`, files[0]));
  assert.equal(child.phase, 'executing'); assert.ok(child.executeRequestId); assert.ok(child.executeJobId);
  assert.equal(child.pendingLocalAdmission, null);
});

for (const code of ['forbidden', 'binding', 'terminal', 'cancel']) test(`execute observation never retries ${code} failure or admits unrelated sender`, async t => {
  const { client, calls, options } = await fixture(t);
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1;
  let reads = 0; const getJob = client.getJob; const controller = new AbortController();
  if (code === 'cancel') options.signal = controller.signal;
  client.getJob = async id => {
    if (!id.startsWith('execute-')) return getJob(id);
    reads++;
    if (code === 'forbidden') throw new CliError('Denied', { code: 'HTTP_ERROR', status: 403 });
    if (code === 'binding') return { ...await getJob(id), refId: 'foreign-approval' };
    if (code === 'terminal') return { ...await getJob(id), status: 'failed' };
    controller.abort(); throw new CliError('Read interrupted by pause', { code: 'NETWORK_ERROR' });
  };
  const result = await runQueue(client, options);
  assert.notEqual(result.mode, 'complete'); assert.equal(reads, 1);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair']);
});

test('ordinary conservative workflow bounds read recovery while retaining its original execution only', async t => {
  const { client, calls, options } = await fixture(t);
  options.scopeItemIds = ['ready']; options.batchSize = 1; options.continueHeld = false;
  let reads = 0; const getJob = client.getJob;
  client.getJob = async id => {
    if (id.startsWith('execute-')) { reads++; throw new CliError('Read unavailable', { code: 'NETWORK_ERROR' }); }
    return getJob(id);
  };
  const result = await runQueue(client, options);
  assert.notEqual(result.mode, 'complete'); assert.equal(reads, 3);
  assert.equal(calls.filter(row => row[0] === 'execute').length, 1);
});

test('unattended scoped queue repairs, freshly reviews and sends accepted refs while private/cutoff holds remain visible', async t => {
  const { path, client, calls, options } = await fixture(t);
  const result = await runQueue(client, options);
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 2); assert.equal(result.counts.held, 2);
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['repair', 'private', 'ready']]);
  assert.deepEqual(calls.filter(row => row[0] === 'approve').map(row => row[1]), [[{ id: 'p-repair', revision: 2 }, { id: 'p-ready', revision: 1 }]]);
  assert.equal(calls.filter(row => row[0] === 'editorial').length, 2); assert.equal(calls.filter(row => row[0] === 'repair').length, 1);
  const parent = await read(result.checkpoint.slices[0].childPath);
  const files = await readdir(`${result.checkpoint.slices[0].childPath}.ready`);
  const child = await read(join(`${result.checkpoint.slices[0].childPath}.ready`, files[0]));
  assert.equal(child.phase, 'complete-with-holds'); assert.deepEqual(child.originProposals.map(ref), parent.readyBatches[0].proposals.map(ref));
  assert.equal(child.proposals.find(row => row.id === 'p-repair').revision, 2);
  assert.equal(child.editorialReview.outcome.held[0].reference.id, 'p-private'); assert.equal(child.repairs.length, 1);
  assert.deepEqual(result.checkpoint.scopeHolds, [{ itemId: 'late', reason: 'scope-cutoff-excluded' }]);
  const postCount = calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length;
  await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, postCount);
});

test('lost repair ACK recovers its exact canonical transition after restart without a duplicate repair or premature send', async t => {
  const { path, client, calls, options } = await fixture(t, { loseRepair: true });
  const first = await runQueue(client, options); assert.equal(first.mode, 'stopped');
  assert.equal(calls.filter(row => row[0] === 'repair').length, 1); assert.equal(calls.filter(row => row[0] === 'execute').length, 0);
  const result = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 2);
  assert.equal(calls.filter(row => row[0] === 'repair').length, 1); assert.equal(calls.filter(row => row[0] === 'execute').length, 1);
  assert.ok(calls.some(row => row[0] === 'receipt' && row[1] === 'editorial-repair'));
});

test('unconfirmed repair intent never retries POST or sends another independent scope', async t => {
  const { path, client, calls, options } = await fixture(t, { unconfirmedRepair: true });
  const first = await runQueue(client, { ...options, batchSize: 1 }); assert.equal(first.mode, 'stopped');
  await runQueue(client, { ...options, batchSize: 1, checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => row[0] === 'repair').length, 1); assert.equal(calls.filter(row => row[0] === 'execute').length, 0);
});

test('genuine held first family does not stop the independently prepared next family', async t => {
  const { client, calls, options } = await fixture(t);
  const result = await runQueue(client, { ...options, scopeItemIds: ['private', 'ready'], batchSize: 1 });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.held, 1); assert.equal(result.counts.verifiedReplies, 1);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), [[{ id: 'p-ready', revision: 1 }]]);
});

test('repeated text/context proof stops repair loop and retains hold without dropping independent accepted refs', async t => {
  const { client, calls, options } = await fixture(t, { repeat: true });
  const result = await runQueue(client, options); assert.equal(result.mode, 'complete');
  assert.equal(calls.filter(row => row[0] === 'repair').length, 1); assert.equal(result.counts.verifiedReplies, 1); assert.equal(result.counts.held, 3);
  assert.deepEqual(calls.filter(row => row[0] === 'approve').map(row => row[1]), [[{ id: 'p-ready', revision: 1 }]]);
});

test('acknowledged UNKNOWN sender quarantines A while a later independent scope still executes once', async t => {
  const { path, client, calls, options, operations } = await fixture(t, { unknown: true });
  const first = await runQueue(client, { ...options, scopeItemIds: ['repair', 'ready'], batchSize: 1 }); assert.equal(first.mode, 'stopped');
  assert.equal(first.stopReason, 'operation-outcomes-unresolved'); assert.equal(first.counts.unknown, 1); assert.equal(first.counts.verifiedReplies, 1);
  await runQueue(client, { ...options, scopeItemIds: ['repair', 'ready'], batchSize: 1, checkpointPath: undefined, resumePath: path });
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), [[{ id: 'p-repair', revision: 2 }], [{ id: 'p-ready', revision: 1 }]]);
  assert.equal(operations.filter(row => row.itemId === 'repair').length, 1);
});

test('UNKNOWN outside the fixed manifest does not stop independent accepted scopes', async t => {
  const { client, options, operations } = await fixture(t);
  operations.push({ id: 'external-op', itemId: 'outside', proposalId: 'external-p', status: 'unknown' });
  const result = await runQueue(client, options);
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 2);
  assert.equal(operations.filter(row => row.proposalId === 'external-p').length, 1);
});

test('retargeted manifest/cutoff/policy and forged repaired child revision fail before any new admission', async t => {
  const { path, client, calls, options } = await fixture(t);
  const result = await runQueue(client, options); const before = calls.length;
  for (const change of [{ scopeItemIds: ['outside'] }, { cutoffUtc: '2026-10-02T00:00:00Z' }, { maxRepairRounds: 3 }])
    await assert.rejects(runQueue(client, { ...options, ...change, checkpointPath: undefined, resumePath: path }), { code: 'INVALID_CHECKPOINT' });
  assert.equal(calls.length, before);
  const parentPath = result.checkpoint.slices[0].childPath; const parent = await read(parentPath);
  const files = await readdir(`${parentPath}.ready`); const childPath = join(`${parentPath}.ready`, files[0]);
  const child = await read(childPath); child.phase = 'prepared'; child.approvalId = null; child.approvalRequestId = null;
  child.proposals[0].revision = 9; await writeCheckpoint(childPath, child);
  parent.readyBatches[0].mode = 'started'; await writeCheckpoint(parentPath, parent);
  const posts = calls.filter(row => ['approve', 'execute', 'repair'].includes(row[0])).length;
  await assert.rejects(runWorkflow(client, [], { resumePath: parentPath, autonomous: true, execute: true, ...parent.flowPolicy }), { code: 'INVALID_EDITORIAL_REVIEW' });
  assert.equal(calls.filter(row => ['approve', 'execute', 'repair'].includes(row[0])).length, posts);
});

test('changing proof cannot bypass the durable maximum repair rounds', async t => {
  const { client, calls, options } = await fixture(t, { repeat: true, changingProof: true });
  const result = await runQueue(client, options);
  assert.equal(result.mode, 'complete'); assert.equal(calls.filter(row => row[0] === 'repair').length, 2);
  assert.equal(calls.filter(row => row[0] === 'editorial').length, 3); assert.equal(result.counts.verifiedReplies, 1);
  assert.equal(result.counts.held, 3);
});

test('1200-item campaign uses the existing maximum 100-proposal batches and completes every scoped recipient once', async t => {
  const { path, client, calls, options, items } = await fixture(t);
  items.splice(0, items.length, ...Array.from({ length: 1200 }, (_, index) => ({ id: `campaign-${String(index).padStart(4, '0')}`,
    workflow: 'attention', providerStatus: 'new', conversationKey: `branch-${index}`, createdAt: '2026-09-30T00:00:00Z' })));
  const manifest = items.map(row => row.id);
  const result = await runQueue(client, { ...options, scopeItemIds: manifest, batchSize: 100, maxCycles: 20 });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 1200); assert.equal(result.counts.held, 0);
  assert.equal(calls.filter(row => row[0] === 'prepare').length, 12);
  const approved = calls.filter(row => row[0] === 'approve').flatMap(row => {
    assert.equal(row[1].length, 100); return row[1].map(ref => ref.id);
  });
  assert.equal(approved.length, 1200); assert.equal(new Set(approved).size, 1200);
  const posts = calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length;
  await runQueue(client, { ...options, scopeItemIds: manifest, batchSize: 100, maxCycles: 20, checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, posts);
});

async function mediaFixture(t) {
  const value = await fixture(t); const { client, items, options } = value;
  items.push({ id: 'media', workflow: 'attention', providerStatus: 'new', createdAt: '2026-09-29T00:00:00Z', conversationKey: 'media-branch' });
  let ready = false;
  client.planPrepare = async ids => ({ batches: ids.filter(id => id !== 'media' || ready).length
    ? [{ itemIds: ids.filter(id => id !== 'media' || ready), bytes: 1000 }] : [],
  held: ids.includes('media') && !ready ? [{ itemId: 'media', reason: 'media_pending', detail: 'Canonical media proof job pending' }] : [] });
  options.scopeItemIds = ['media', 'ready']; options.batchSize = 2; options.maxCycles = 10;
  return { ...value, mediaReady: () => { ready = true; } };
}

test('delayed media wakes only its pre-model hold after the independent ready family has already sent', async t => {
  const { client, calls, options, mediaReady } = await mediaFixture(t);
  const wakes = [];
  const result = await runQueue(client, { ...options, waitForDependencies: async context => {
    assert.deepEqual(context.heldItemIds, ['media']); assert.equal(context.heldSlices[0].child, null);
    assert.equal(calls.filter(row => row[0] === 'execute').length, 1);
    assert.equal(calls.find(row => row[0] === 'execute')[1][0].id, 'p-ready');
    mediaReady(); const plan = await client.planPrepare(context.heldItemIds);
    wakes.push(plan); return plan.batches.flatMap(batch => batch.itemIds);
  } });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.verifiedReplies, 2); assert.equal(result.counts.held, 0);
  assert.equal(wakes.length, 1); assert.deepEqual(result.checkpoint.dependencyRequeues[0].itemIds, ['media']);
  const original = result.checkpoint.slices.find(slice => slice.planHoldReason === 'media_pending');
  assert.equal(original.dependencyResolved, true); assert.equal(original.planHoldDetail, 'Canonical media proof job pending');
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['ready'], ['media']]);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-ready', 'p-media']);
});

for (const boundary of ['before-requeue-checkpoint', 'after-requeue-checkpoint']) test(`media wake restart at ${boundary} preserves exact preparation and sends once`, async t => {
  const { path, client, calls, options, mediaReady } = await mediaFixture(t);
  let fail = true; let wakes = 0;
  const waitForDependencies = async ({ heldItemIds }) => {
    wakes++; mediaReady();
    if (boundary === 'before-requeue-checkpoint' && fail) { fail = false; throw new Error('fake crash before persisted requeue'); }
    const plan = await client.planPrepare(heldItemIds); return plan.batches.flatMap(batch => batch.itemIds);
  };
  const onProgress = event => {
    if (boundary === 'after-requeue-checkpoint' && event.event === 'dependency.requeued' && fail) {
      fail = false; throw new Error('fake crash after persisted requeue');
    }
  };
  await assert.rejects(runQueue(client, { ...options, waitForDependencies, onProgress }), /fake crash/);
  const saved = await read(path); const original = saved.slices.find(slice => slice.planHoldReason === 'media_pending');
  assert.equal(original.dependencyResolved === true, boundary === 'after-requeue-checkpoint');
  const result = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path, waitForDependencies });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.held, 0); assert.equal(result.counts.verifiedReplies, 2);
  assert.equal(wakes, boundary === 'before-requeue-checkpoint' ? 2 : 1);
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['ready'], ['media']]);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-ready', 'p-media']);
  assert.equal(result.checkpoint.dependencyRequeues.length, 1);
});

test('dependency callback cannot expand scope, requeue private/editorial/model holds or bypass UNKNOWN', async t => {
  const { client, options, mediaReady } = await mediaFixture(t);
  await assert.rejects(runQueue(client, { ...options, waitForDependencies: async () => { mediaReady(); return ['outside']; } }),
    { code: 'INVALID_DEPENDENCY_SCOPE' });
  const second = await fixture(t);
  const held = await runQueue(second.client, { ...second.options, waitForDependencies: () => assert.fail('editorial/private hold is not a media dependency') });
  assert.equal(held.mode, 'complete');
  const third = await mediaFixture(t); third.operations.push({ id: 'unknown', itemId: 'outside', proposalId: 'external', status: 'unknown' });
  third.items.find(row => row.id === 'media').conversationKey = third.items.find(row => row.id === 'outside').conversationKey;
  const stopped = await runQueue(third.client, { ...third.options, waitForDependencies: () => assert.fail('UNKNOWN conversation excludes dependent recipient') });
  assert.equal(stopped.stopReason, 'operation-outcomes-unresolved');
  assert.deepEqual(third.calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['ready']]);
  const fourth = await mediaFixture(t);
  fourth.operations.push({ id: 'prior-op', itemId: 'media', proposalId: 'prior-proposal', status: 'failed' });
  const occupied = await runQueue(fourth.client, { ...fourth.options,
    waitForDependencies: () => assert.fail('Existing operation excludes a recipient from pre-model media wake') });
  assert.equal(occupied.counts.held, 1);
  assert.deepEqual(fourth.calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['ready']]);
});

test('historical paid model attempt prevents a later typed media hold from entering the dependency callback', async t => {
  const { path, client, calls, options } = await mediaFixture(t);
  await writeCheckpoint(path, { kind: 'communityhero-queue', account: client.account, baseUrl: client.baseUrl, phase: 'running',
    cycle: 1, materialsReady: true, attemptedItemIds: ['media'], scopeItemIds: ['media'], cutoffUtc: options.cutoffUtc,
    flowPolicy: { freshEditorial: true, maxRepairRounds: 2, continueHeld: true },
    slices: [{ id: 'paid-attempt', itemIds: ['media'], status: 'held', error: null, child: { prepareJobId: 'original-paid-job',
      phase: 'prepare-failed', proposals: [], operations: [] } },
    { id: 'typed-media-hold', itemIds: ['media'], status: 'plan-held', child: null, planHoldReason: 'media_unavailable' }] });
  await runQueue(client, { ...options, scopeItemIds: ['media'], checkpointPath: undefined, resumePath: path,
    waitForDependencies: () => assert.fail('A media wake must never re-admit a historical paid model attempt') });
  assert.equal(calls.filter(row => row[0] === 'prepare').length, 0);
});

test('UNKNOWN A quarantines its conversation while unrelated C and D execute once with no second admission for A', async t => {
  const { path, client, options, operations, items, proposals, calls } = await fixture(t);
  items.find(row => row.id === 'private').conversationKey = items.find(row => row.id === 'repair').conversationKey;
  items.push({ id: 'other-ready', workflow: 'attention', providerStatus: 'new', createdAt: '2026-09-30T00:00:09Z', conversationKey: 'other-ready-branch' });
  operations.push({ id: 'original-unknown', itemId: 'repair', proposalId: 'original-proposal', status: 'unknown' });
  proposals.push({ id: 'original-proposal', itemId: 'repair', revision: 1, status: 'unknown', kind: 'reply_and_close', prepareRunId: 'original-job' });
  options.scopeItemIds = ['repair', 'private', 'ready', 'other-ready']; options.maxCycles = 10;
  const result = await runQueue(client, options);
  assert.equal(result.mode, 'stopped'); assert.equal(result.stopReason, 'operation-outcomes-unresolved');
  assert.equal(result.counts.unknown, 1); assert.equal(result.counts.verifiedReplies, 2); assert.equal(result.counts.held, 2);
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), [['ready', 'other-ready']]);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').flatMap(row => row[1].map(ref => ref.id)), ['p-ready', 'p-other-ready']);
  assert.deepEqual(result.checkpoint.scopeHolds.map(row => row.reason), ['unknown-conversation-held', 'unknown-conversation-held']);
  const posts = calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length;
  await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, posts);
  assert.equal(operations.filter(row => row.itemId === 'repair').length, 1);
});

test('Rust alphabetically ordered seed policy resumes and prepare-only mode finishes multiple owned scopes', async t => {
  const { path, client, options, calls } = await fixture(t);
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1; options.execute = false; options.autonomous = false;
  await writeCheckpoint(path, { kind: 'communityhero-queue', account: client.account, baseUrl: client.baseUrl, phase: 'starting',
    createdAt: new Date().toISOString(), cycle: 0, attemptedItemIds: [], slices: [], scopeItemIds: options.scopeItemIds,
    cutoffUtc: options.cutoffUtc, flowPolicy: { continueHeld: true, freshEditorial: true, maxRepairRounds: 2 } });
  const result = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(result.mode, 'complete'); assert.equal(result.counts.prepared, 2);
  assert.deepEqual(result.checkpoint.slices.map(row => row.status), ['prepare-only', 'prepare-only']);
  assert.equal(calls.filter(row => ['editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, 0);
  const childPath = result.checkpoint.slices[0].childPath;
  const child = await read(childPath); child.flowPolicy = { continueHeld: true, freshEditorial: true, maxRepairRounds: 2 };
  await writeCheckpoint(childPath, child);
  const workflow = await runWorkflow(client, child.itemIds, { resumePath: childPath, ...child.flowPolicy });
  assert.equal(workflow.mode, 'prepare-only');
  child.flowPolicy.unexpected = true; await writeCheckpoint(childPath, child);
  await assert.rejects(runWorkflow(client, child.itemIds, { resumePath: childPath, ...options }), { code: 'INVALID_CHECKPOINT' });
});

test('post-dispatch UNKNOWN A allows independent B/C to finish and canonical readback resumes without another effect', async t => {
  const { path, client, calls, options, operations, items } = await fixture(t, { unknown: true });
  items.push({ id: 'other-ready', workflow: 'attention', providerStatus: 'new', createdAt: '2026-09-30T00:00:09Z', conversationKey: 'other-ready-branch' });
  options.scopeItemIds = ['repair', 'ready', 'other-ready']; options.batchSize = 1; options.maxCycles = 10;
  const first = await runQueue(client, options);
  assert.equal(first.mode, 'stopped'); assert.equal(first.stopReason, 'operation-outcomes-unresolved');
  assert.equal(first.counts.unknown, 1); assert.equal(first.counts.verifiedReplies, 2);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair', 'p-ready', 'p-other-ready']);
  assert.equal(operations.filter(row => row.itemId === 'repair').length, 1);
  const posts = calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length;
  await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, posts);
  operations.find(row => row.itemId === 'repair').status = 'succeeded';
  const final = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(final.mode, 'complete'); assert.equal(final.counts.unknown, 0); assert.equal(final.counts.verifiedReplies, 3);
  assert.equal(calls.filter(row => ['prepare', 'editorial', 'repair', 'approve', 'execute'].includes(row[0])).length, posts);
});

test('lost execute ACK without acknowledged operation closure still stops unrelated sender admission', async t => {
  const { client, calls, options } = await fixture(t, { unknown: true });
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1;
  const execute = client.execute;
  client.execute = async (...args) => { await execute(...args); throw new UnknownMutationError('POST', `/api/approvals/${args[0]}/execute`, { code: 'TIMEOUT' }); };
  const first = await runQueue(client, options);
  assert.equal(first.mode, 'stopped'); assert.equal(first.stopReason, 'overlapped-slice-unresolved');
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair']);
});

async function interruptedFixture(t, { loseAck = false } = {}) {
  const value = await fixture(t, { unknown: true }); const { client, options, items, operations, jobs } = value;
  options.scopeItemIds = ['repair', 'ready']; options.batchSize = 1; options.maxCycles = 10;
  const binding = { id: 'connector', workspaceId: 'BAW', accountId: 'BAW', connector: 'fake', providerAccountId: 'fake-provider', revision: 1 };
  for (const item of items) Object.assign(item, { itemId: `provider-${item.id}`, objectId: 'object', postKey: `post-${item.id}`, connectorBinding: binding });
  const execute = client.execute;
  client.execute = async (...args) => {
    const result = await execute(...args);
    jobs.get(result.jobId).conductorRunId = 'original-campaign'; jobs.get(result.jobId).grantGeneration = 1;
    const executedRefs = value.calls.filter(row => row[0] === 'execute').at(-1)[1];
    const ownOperations = operations.filter(row => executedRefs.some(ref => ref.id === row.proposalId));
    for (const operation of ownOperations) {
      const target = structuredClone(items.find(row => row.id === operation.itemId));
      const proposal = value.proposals.find(row => row.id === operation.proposalId);
      Object.assign(operation, { approvalId: result.approvalId, target, approvedBy: { id: 'operator', role: 'operator', name: 'Operator' },
        executedBy: { id: 'operator', role: 'operator', name: 'Operator' }, conductorRunId: 'original-campaign', grantGeneration: 1,
        action: { actionId: operation.id, action: proposal.kind, reply: proposal.text,
          itemId: target.itemId, objectId: target.objectId, conversationKey: target.conversationKey } });
    }
    if (loseAck && ownOperations.some(row => row.itemId === 'repair')) throw new UnknownMutationError('POST', `/api/approvals/${args[0]}/execute`, { code: 'TIMEOUT' });
    return result;
  };
  const getJob = client.getJob;
  let unavailableReads = 0;
  client.getJob = async id => {
    if (id.startsWith('execute-') && unavailableReads < 3) {
      unavailableReads++; const job = jobs.get(id); job.status = 'interrupted';
      throw new CliError('Fake process stopped before operation checkpoint', { code: 'READ_TIMEOUT' });
    }
    return getJob(id);
  };
  return value;
}

for (const lostAck of [false, true]) test(`restart inspects interrupted original execute with no saved operations${lostAck ? ' after canonical lost ACK recovery' : ''} and drains independent B once`, async t => {
  const value = await interruptedFixture(t, { loseAck: lostAck }); const { path, client, calls, options, jobs, operations } = value;
  const first = await runQueue(client, options); assert.equal(first.stopReason, 'overlapped-slice-unresolved');
  const parentPath = first.checkpoint.slices[0].childPath; const files = await readdir(`${parentPath}.ready`);
  const childPath = join(`${parentPath}.ready`, files[0]); const child = await read(childPath);
  assert.equal(child.operations, undefined);
  if (lostAck) {
    assert.equal(child.pendingLocalAdmission.kind, 'execute');
    for (const job of jobs.values()) if (job.kind === 'execute') job.status = 'interrupted';
    const original = client.getJob; client.getJob = async id => jobs.has(id) ? jobs.get(id) : original(id);
  } else assert.ok(child.executeJobId);
  const result = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
  assert.equal(result.stopReason, 'operation-outcomes-unresolved'); assert.equal(result.counts.unknown, 1); assert.equal(result.counts.verifiedReplies, 1);
  assert.equal((await read(childPath)).operations[0].id, 'op-p-repair');
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair', 'p-ready']);
  assert.equal(operations.filter(row => row.itemId === 'repair').length, 1);
});

for (const corruption of ['approval', 'revision', 'target', 'action-id', 'actor', 'actor-role', 'actor-authority', 'grant', 'duplicate', 'coverage'])
  test(`interrupted recovery rejects ${corruption} original operation closure before unrelated dispatch`, async t => {
    const { path, client, calls, options, operations, proposals } = await interruptedFixture(t);
    const first = await runQueue(client, options); assert.equal(first.stopReason, 'overlapped-slice-unresolved');
    const op = operations.find(row => row.itemId === 'repair');
    if (corruption === 'approval') op.approvalId = 'different-approval';
    if (corruption === 'revision') proposals.find(row => row.id === op.proposalId).revision++;
    if (corruption === 'target') op.target.itemId = 'different-recipient';
    if (corruption === 'action-id') op.action.actionId = 'different-operation';
    if (corruption === 'actor') op.executedBy.id = 'different-actor';
    if (corruption === 'actor-role') op.executedBy.role = 'owner';
    if (corruption === 'actor-authority') { op.approvedBy.authorityGeneration = 'original-authority'; op.executedBy.authorityGeneration = 'different-authority'; }
    if (corruption === 'grant') delete op.grantGeneration;
    if (corruption === 'duplicate') operations.push({ ...op, id: 'duplicate-op' });
    if (corruption === 'coverage') {
      const review = client.reviewItems; client.reviewItems = async ids => ({ ...await review(ids), coverage: { operationsComplete: false } });
    }
    const result = await runQueue(client, { ...options, checkpointPath: undefined, resumePath: path });
    assert.equal(result.mode, 'stopped'); assert.equal(result.stopReason, 'overlapped-slice-unresolved');
    assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1][0].id), ['p-repair']);
  });

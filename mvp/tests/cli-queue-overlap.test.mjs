import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { runQueue } from '../cli/queue.mjs';
import { CliError, CommunityHeroClient, UnknownMutationError, localAdmissionPayloadHash } from '../cli/client.mjs';

async function fixture(t, { version = 1, unknown = false, block = true } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'ch-queue-overlap-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'queue.json'); const calls = []; const jobs = new Map();
  const items = [1, 2, 3].map(n => ({ id: `i${n}`, workflow: 'attention', providerStatus: 'new', conversationKey: `branch${n}` }));
  const proposals = []; const operations = []; const receipts = new Map();
  let release; const nextPrepared = new Promise(resolve => { release = resolve; });
  let reconcileAllowed = false;
  const snapshot = () => ({ account: 'BAW', items, proposals, operations, jobs: [...jobs.values()], materials: [{ kind: 'knowledge', imported: true }],
    sync: { openCoverage: { done: true, coverageComplete: true } } });
  const client = { account: 'BAW', baseUrl: 'http://localhost:4187',
    engineStatus: async () => ({ strictGrouping:{version:1,contract:'strict_post_family_v1'},prepareScopeReservations: { version } }),
    requireStrictPreparation: CommunityHeroClient.prototype.requireStrictPreparation,
    importMaterials: async () => ({ jobId: 'materials' }),
    bootstrap: async () => snapshot(),
    sync: async () => ({ jobId: 'sync' }),
    planPrepare: async ids => ({ batches: [{ itemIds: ids, bytes: 1000 }], held: [] }),
    reviewItems: async ids => ({ items: items.filter(row => ids.includes(row.id)), proposals: proposals.filter(row => ids.includes(row.itemId)),
      operations: operations.filter(row => ids.includes(row.itemId)), coverage: { operationsComplete: true } }),
    prepareEngine: async (payload, requestId) => {
      requestId ||= payload.requestId;
      const id = payload.itemIds[0]; calls.push(['prepare', id]);
      const jobId = `prepare-${id}`;
      const scopeReservation = { version: 1, ownerJobId: jobId, keysDigest: 'a'.repeat(64) };
      proposals.push({ id: `p-${id}`, itemId: id, revision: 1, status: 'draft', kind: 'reply_and_close', text: 'Synthetic', prepareRunId: jobId });
      items.find(row => row.id === id).workflow = 'prepared';
      jobs.set(jobId, { id: jobId, kind: 'assistant', purpose: 'engine_prepare', status: 'completed', scopeReservation,
        preparationStages: { groupAdmission: [{ key: id, itemIds: [id], status: 'admitted',
          admission: { candidates: [{ itemId: id, proposalId: `p-${id}`, status: 'review' }] } }] } });
      const result = { jobId, requestId, scopeReservation }; receipts.set(`prepare:${requestId}`, { kind: 'prepare', requestId, status: 'committed',
        payloadHash: localAdmissionPayloadHash({ itemIds: payload.itemIds, instruction: payload.instruction }), result });
      return result;
    },
    getJob: async id => {
      calls.push(['poll', id]);
      if (id === 'execute-p-i1' && block && version === 1) await nextPrepared;
      if (id === 'prepare-i2') release();
      if (['materials', 'sync'].includes(id)) return { id, status: 'completed' };
      return jobs.get(id);
    },
    editorialReview: async (refs, requestId) => {
      calls.push(['editorial', refs[0].id]); const jobId = `editorial-${requestId}`;
      jobs.set(jobId, { id: jobId, kind: 'editorial_review', refId: requestId, status: 'completed', editorialOutcome: { accepted: refs, reused: refs, held: [] } });
      return { jobId, requestId, replayed: false };
    },
    createApproval: async (refs, requestId) => { calls.push(['approve', refs[0].id]); return { id: `approval-${refs[0].id}`, requestId }; },
    execute: async (approvalId, requestId) => {
      const proposalId = approvalId.slice('approval-'.length); calls.push(['execute', proposalId]);
      const jobId = `execute-${proposalId}`; jobs.set(jobId, { id: jobId, kind: 'execute', refId: approvalId, status: 'completed' });
      operations.push({ id: `op-${proposalId}`, approvalId, itemId: proposalId.slice(2), proposalId, status: unknown && proposalId === 'p-i1' ? 'unknown' : 'succeeded' });
      return { jobId, approvalId, requestId, replayed: false };
    },
    localAdmission: async (kind, requestId) => receipts.get(`${kind}:${requestId}`),
    reconcile: async id => {
      calls.push(['reconcile', id]); if (reconcileAllowed) operations.find(row => row.id === id).status = 'succeeded';
      const jobId = `reconcile-${id}`; jobs.set(jobId, { id: jobId, status: 'completed' }); return { jobId };
    }
  };
  return { path, client, calls, jobs, items, operations, release, allowReconcile: () => {
    reconcileAllowed = true;
    // Simulate separately admitted authoritative readback of the original op.
    for (const op of operations) if (op.status === 'unknown') op.status = 'succeeded';
  } };
}
const options = path => ({ checkpointPath: path, batchSize: 1, maxCycles: 1, autonomous: true, execute: true, pollMs: 0 });
const saved = async path => JSON.parse(await readFile(path, 'utf8'));

test('family selection sees exact eligible scope before the chronological window and checkpoints returned windows', async t => {
  const { path, client, items, operations } = await fixture(t, { block: false });
  items.push({ id: 'unknown', workflow: 'attention', providerStatus: 'new', conversationKey: 'uncertain' });
  operations.push({ id: 'retain', itemId: 'unknown', status: 'unknown' });
  const selections = []; const planned = [];
  client.selectPrepareFamilies = async (ids, size, options) => {
    selections.push({ ids, size, options }); return [['i1', 'i3'], ['i2']];
  };
  client.planPrepare = async ids => { planned.push(ids); return { batches: [], held: ids.map(itemId => ({ itemId, reason: 'fixture-held' })) }; };
  const result = await runQueue(client, { ...options(path), batchSize: 2, continueHeld: true });
  assert.deepEqual(selections, [{ ids: ['i1', 'i2', 'i3'], size: 2, options: { maxBatches: 2 } }]);
  assert.deepEqual(planned, [['i1', 'i3'], ['i2']]);
  assert.deepEqual(result.checkpoint.slices.map(slice => slice.itemIds[0]), ['i1', 'i3', 'i2']);
  assert.equal(operations[0].status, 'unknown');
  assert.ok(!result.checkpoint.attemptedItemIds.includes('unknown'));
});

test('ready B admits and confirms before slow advisory A completes, with stable persisted window identity', { timeout:5000 }, async t => {
  const f=await fixture(t,{block:false});
  let releaseA, aFinished=false;
  const slow=new Promise(resolve=>{releaseA=resolve;});t.after(()=>releaseA());
  f.client.engineStatus=async()=>({strictGrouping:{version:1,contract:'strict_post_family_v1'},prepareScopeReservations:{version:1},prepareWorkers:{version:1,maxWorkers:2}});
  f.client.selectPrepareFamilies=async()=>[['i1'],['i2']];
  f.client.planPrepare=async ids=>{if(ids[0]==='i1'){await slow;aFinished=true;}return {batches:[{itemIds:ids,bytes:1000}],held:[]};};
  const prepare=f.client.prepareEngine,poll=f.client.getJob;
  f.client.prepareEngine=async(...args)=>{
    if(args[0].itemIds[0]==='i2'){
      assert.equal(aFinished,false);
      const parent=await saved(f.path);
      assert.equal(parent.pendingSlices[0].id,'cycle-1-window-2-slice-1');
      assert.deepEqual(parent.pendingSlices[0].itemIds,['i2']);
    }
    return prepare(...args);
  };
  f.client.getJob=async id=>{
    const result=await poll(id);
    if(id==='execute-p-i2'){assert.equal(aFinished,false);assert.equal(result.status,'completed');releaseA();}
    return result;
  };
  const result=await runQueue(f.client,{...options(f.path),scopeItemIds:['i1','i2']});
  assert.deepEqual(f.calls.filter(row=>row[0]==='prepare').map(row=>row[1]),['i2','i1']);
  assert.deepEqual(f.calls.filter(row=>row[0]==='execute').map(row=>row[1]),['p-i2','p-i1']);
  assert.equal(result.checkpoint.pendingSlices.length,0);
});

test('advisory failure after B paid ACK preserves its original child and cleanup cannot repeat admission', {timeout:5000}, async t=>{
  const f=await fixture(t,{block:false});let rejectA;
  const failure=new Error('A advisory read failed after B admission');
  const slow=new Promise((_,reject)=>{rejectA=reject;});
  f.client.engineStatus=async()=>({strictGrouping:{version:1,contract:'strict_post_family_v1'},prepareScopeReservations:{version:1},prepareWorkers:{version:1,maxWorkers:2}});
  f.client.selectPrepareFamilies=async()=>[['i1'],['i2']];
  f.client.planPrepare=async ids=>{if(ids[0]==='i1')await slow;return {batches:[{itemIds:ids,bytes:1000}],held:[]};};
  await assert.rejects(runQueue(f.client,{...options(f.path),scopeItemIds:['i1','i2'],onProgress:event=>{
    if(event.event==='prepare.admitted')rejectA(failure);
  }}),error=>error===failure);
  const parent=await saved(f.path);const child=parent.pendingSlices.find(row=>row.itemIds[0]==='i2');
  assert.equal(child.id,'cycle-1-window-2-slice-1');assert.equal((await saved(child.childPath)).prepareJobId,'prepare-i2');
  assert.deepEqual(f.calls.filter(row=>row[0]==='prepare').map(row=>row[1]),['i2']);
  assert.equal(f.jobs.get('prepare-i2').status,'completed');assert.equal(f.calls.filter(row=>row[0]==='execute').length,0);
  await runQueue(f.client,{...options(f.path),checkpointPath:undefined,resumePath:f.path,scopeItemIds:['i1','i2']});
  assert.deepEqual(f.calls.filter(row=>row[0]==='prepare').map(row=>row[1]),['i2']);
});

async function fourWorkers(t, { unknown = false } = {}) {
  const f = await fixture(t, { block: false, unknown });
  f.items.push(...[4, 5].map(n => ({ id: `i${n}`, workflow: 'attention', providerStatus: 'new', conversationKey: `branch${n}` })));
  f.client.engineStatus = async () => ({ strictGrouping:{version:1,contract:'strict_post_family_v1'},prepareScopeReservations: { version: 1 }, prepareWorkers: { version: 1, maxWorkers: 4 } });
  f.client.selectPrepareFamilies = async (ids, size, config) => {
    assert.equal(config.maxBatches, 4); assert.equal(size, 1);
    return ids.slice(0, 4).map(id => [id]);
  };
  // This fixture models four already planned windows. Incremental production
  // intake can start a ready window earlier, so hold only this mock until the
  // public post-persistence events confirm that all four are in the journal.
  let registered; const allRegistered = new Promise(resolve => { registered = resolve; });
  const planned = new Set();
  f.run = config => runQueue(f.client, { ...config, onProgress: event => {
    if (event.event === 'prepare.plan.ready') {
      planned.add(event.window);
      if (planned.size === 4) registered();
    }
    config.onProgress?.(event);
  } });
  let release; const allAdmitted = new Promise(resolve => { release = resolve; });
  const prepare = f.client.prepareEngine; const poll = f.client.getJob;
  let registration;
  const binding = row => ({ id: row.id, itemIds: row.itemIds, childPath: row.childPath,
    familyPlanVersion: row.familyPlanVersion, familyWindow: row.familyWindow, plannedBytes: row.plannedBytes });
  f.client.prepareEngine = async payload => {
    await allRegistered;
    const parent = await saved(f.path);
    if (!registration) {
      assert.equal(parent.pendingSlices.length, 4);
      assert.equal(f.calls.filter(row => row[0] === 'prepare').length, 0);
      registration = parent.pendingSlices.map(binding).sort((a, b) => a.id.localeCompare(b.id));
    }
    const children = [...parent.pendingSlices, ...parent.slices];
    assert.equal(children.length, 4);
    assert.equal(new Set(children.map(row => row.id)).size, 4);
    assert.deepEqual(children.map(binding).sort((a, b) => a.id.localeCompare(b.id)), registration);
    const own = children.find(row => row.itemIds.includes(payload.itemIds[0]));
    const intent = await saved(own.childPath);
    assert.deepEqual(intent.itemIds, payload.itemIds);
    assert.equal(intent.pendingLocalAdmission.kind, 'prepare');
    assert.equal(intent.pendingLocalAdmission.requestId, payload.requestId);
    assert.equal(intent.pendingLocalAdmission.payloadHash,
      localAdmissionPayloadHash({ itemIds: payload.itemIds, instruction: payload.instruction }));
    if (payload.itemIds[0] !== 'i1') {
      // The original paid child is durably registered before any fanout,
      // including after it has completed and moved into durable history.
      const original = children.find(row => row.itemIds[0] === 'i1');
      const first = await saved(original.childPath);
      assert.equal(first.prepareJobId, 'prepare-i1');
      assert.equal(first.prepareScopeReservation.ownerJobId, first.prepareJobId);
    }
    const result = await prepare(payload);
    if (f.calls.filter(row => row[0] === 'prepare').length === 4) release();
    return result;
  };
  f.client.getJob = async id => {
    if (id === 'prepare-i1') { await allAdmitted; f.calls.push(['first-prepare-completes']); }
    return poll(id);
  };
  return f;
}

test('actual width four admits three independent producers before current model completes and retains one sender', async t => {
  const f = await fourWorkers(t); const result = await f.run(options(f.path));
  assert.equal(f.calls.find(row => row[0] === 'prepare')[1], 'i1');
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  const completed = f.calls.findIndex(row => row[0] === 'first-prepare-completes');
  assert.ok(f.calls.findIndex(row => row[0] === 'prepare' && row[1] === 'i4') < completed);
  const sends = f.calls.filter(row => row[0] === 'execute').map(row => row[1]);
  assert.deepEqual([...sends].sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
  for (let n = 1; n < sends.length; n++) assert.ok(f.calls.findIndex(row => row[0] === 'execute' && row[1] === sends[n])
    > f.calls.findIndex(row => row[0] === 'poll' && row[1] === `execute-${sends[n - 1]}`));
  assert.equal(result.checkpoint.pendingSlices.length, 0); assert.ok(!result.checkpoint.attemptedItemIds.includes('i5'));
});

test('the first slow family no longer blocks a completed independent family', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t);
  const poll = f.client.getJob, execute = f.client.execute, prepare = f.client.prepareEngine;
  let releaseFirst, firstCompleted = false;
  const first = new Promise(resolve => { releaseFirst = resolve; });
  t.after(() => releaseFirst());
  // Force a valid race: the last producer reads the journal only after an
  // earlier independent sender has already moved from pending to history.
  let releaseFourth, sendCount = 0;
  const fourth = new Promise(resolve => { releaseFourth = resolve; });
  t.after(() => releaseFourth());
  f.client.prepareEngine = async (...args) => {
    if (args[0].itemIds[0] === 'i4') await fourth;
    return prepare(...args);
  };
  f.client.getJob = async (id, options) => {
    if (id === 'prepare-i1') { await first; firstCompleted = true; }
    return poll(id, options);
  };
  f.client.execute = async (...args) => {
    if (++sendCount === 2) releaseFourth();
    if (args[0] === 'approval-p-i2') {
      assert.equal(firstCompleted, false);
      const parent = await saved(f.path);
      assert.deepEqual(parent.currentSlice.itemIds, ['i2']);
      const firstChild = parent.pendingSlices.find(row => row.itemIds[0] === 'i1');
      assert.equal((await saved(firstChild.childPath)).prepareJobId, 'prepare-i1');
      releaseFirst();
    }
    return execute(...args);
  };
  const result = await f.run(options(f.path));
  const sends = f.calls.filter(row => row[0] === 'execute').map(row => row[1]);
  assert.notEqual(sends[0], 'p-i1');
  assert.ok(sends.indexOf('p-i2') < sends.indexOf('p-i1'));
  assert.deepEqual([...sends].sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.equal(result.checkpoint.pendingSlices.length, 0);
});

test('first-family admission must be acknowledged and persisted before independent fanout', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t), prepare = f.client.prepareEngine;
  let reachedAdmission, acknowledge;
  const admitted = new Promise(resolve => { reachedAdmission = resolve; });
  const ack = new Promise(resolve => { acknowledge = resolve; });
  t.after(() => acknowledge());
  f.client.prepareEngine = async (...args) => {
    const result = await prepare(...args);
    if (args[0].itemIds[0] === 'i1') { reachedAdmission(); await ack; }
    return result;
  };
  const running = f.run(options(f.path));
  await admitted;
  const parent = await saved(f.path), first = await saved(parent.pendingSlices[0].childPath);
  assert.equal(parent.currentSlice, undefined);
  assert.equal(first.pendingLocalAdmission.kind, 'prepare');
  assert.equal(first.prepareJobId, undefined);
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1']);
  acknowledge(); await running;
});

test('lost first-family ACK recovers the original admission before conservative foreground fanout', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t), prepare = f.client.prepareEngine;
  // This fixture does not require fanout before the recovered first model
  // completes: recovery deliberately retains the established sequential path.
  f.client.getJob = async id => {
    f.calls.push(['poll', id]);
    return ['materials', 'sync'].includes(id) ? { id, status: 'completed' } : f.jobs.get(id);
  };
  f.client.prepareEngine = async (...args) => {
    const result = await prepare(...args);
    if (args[0].itemIds[0] === 'i1') throw new UnknownMutationError('POST', '/api/engine/prepare', { code: 'TIMEOUT' });
    return result;
  };
  const result = await f.run(options(f.path));
  assert.equal(result.stopReason, 'max-cycles');
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
});

test('unconfirmed first-family admission never opens fanout or repeats its POST on resume', async t => {
  const f = await fourWorkers(t), prepare = f.client.prepareEngine;
  f.client.prepareEngine = async (...args) => {
    await prepare(...args);
    throw new UnknownMutationError('POST', '/api/engine/prepare', { code: 'TIMEOUT' });
  };
  f.client.localAdmission = async () => undefined;
  const first = await f.run(options(f.path));
  assert.equal(first.stopReason, 'overlapped-slice-unresolved');
  await f.run({ ...options(f.path), checkpointPath: undefined, resumePath: f.path });
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute'), []);
});

test('a split first family retains one observer while an independent family prepares and sends', { timeout: 5000 }, async t => {
  const f = await fixture(t, { block: false });
  f.items.push({ id: 'i4', workflow: 'attention', providerStatus: 'new', conversationKey: 'branch4' });
  f.client.engineStatus = async () => ({ strictGrouping:{version:1,contract:'strict_post_family_v1'},prepareScopeReservations: { version: 1 }, prepareWorkers: { version: 1, maxWorkers: 4 } });
  f.client.selectPrepareFamilies = async () => [['i1', 'i2'], ['i3'], ['i4']];
  f.client.planPrepare = async ids => ({ batches: ids.map(id => ({ itemIds: [id], bytes: 1000 })), held: [] });
  const poll = f.client.getJob, prepare = f.client.prepareEngine, execute = f.client.execute;
  let releaseFirst, firstCompleted = false;
  const first = new Promise(resolve => { releaseFirst = resolve; });
  t.after(() => releaseFirst());
  f.client.getJob = async (...args) => {
    if (args[0] === 'prepare-i1') { await first; firstCompleted = true; }
    return poll(...args);
  };
  f.client.prepareEngine = async (...args) => {
    if (args[0].itemIds[0] === 'i2') assert.equal(firstCompleted, true);
    return prepare(...args);
  };
  f.client.execute = async (...args) => {
    if (args[0] === 'approval-p-i3') { assert.equal(firstCompleted, false); releaseFirst(); }
    return execute(...args);
  };
  await runQueue(f.client, { ...options(f.path), batchSize: 2 });
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
});

test('a ready family sends while an older independent producer is still running', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t);
  let releaseSlow; const slow = new Promise(resolve => { releaseSlow = resolve; });
  t.after(() => releaseSlow());
  const poll = f.client.getJob, execute = f.client.execute;
  f.client.getJob = async id => {
    if (id === 'prepare-i2') {
      f.calls.push(['slow-observer-started']);
      await slow;
      f.calls.push(['slow-prepare-completed']);
    }
    return poll(id);
  };
  f.client.execute = async (...args) => {
    if (args[0] === 'approval-p-i3') {
      const parent = await saved(f.path);
      assert.deepEqual(parent.currentSlice.itemIds, ['i3']);
      assert.equal(parent.pendingSlices[0].id, parent.currentSlice.id);
      assert.ok(parent.pendingSlices.some(row => row.itemIds[0] === 'i2'));
      assert.ok(!f.calls.some(row => row[0] === 'slow-prepare-completed'));
      releaseSlow();
    }
    return execute(...args);
  };
  const result = await f.run(options(f.path));
  assert.ok(f.calls.findIndex(row => row[0] === 'execute' && row[1] === 'p-i3')
    < f.calls.findIndex(row => row[0] === 'execute' && row[1] === 'p-i2'));
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
  const sends = f.calls.filter(row => row[0] === 'execute').map(row => row[1]);
  for (let n = 1; n < sends.length; n++) assert.ok(f.calls.findIndex(row => row[0] === 'execute' && row[1] === sends[n])
    > f.calls.findIndex(row => row[0] === 'poll' && row[1] === `execute-${sends[n - 1]}`));
  assert.equal(result.checkpoint.pendingSlices.length, 0);
});

test('UNKNOWN from a reordered ready family stops and joins the slow observer without losing its paid job', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t);
  const poll = f.client.getJob, execute = f.client.execute;
  let slowAborted = false, recovering = false;
  f.client.getJob = async (id, options) => {
    // Both later paid observers stay pending until the UNKNOWN sender stops
    // them. Otherwise i4 may legitimately finish and send before ready i3.
    if (['prepare-i2', 'prepare-i4'].includes(id) && !recovering) {
      await new Promise((resolve, reject) => {
        const abort = () => { slowAborted = true; reject(options.signal.reason); };
        if (options.signal.aborted) abort();
        else options.signal.addEventListener('abort', abort, { once: true });
      });
    }
    return poll(id, options);
  };
  f.client.execute = async (...args) => {
    const outcome = await execute(...args);
    if (args[0] === 'approval-p-i3') f.operations.find(row => row.proposalId === 'p-i3').status = 'unknown';
    return outcome;
  };
  const first = await f.run(options(f.path));
  assert.equal(first.stopReason, 'overlapped-slice-unresolved');
  assert.equal(slowAborted, true);
  const stoppedSends = f.calls.filter(row => row[0] === 'execute').map(row => row[1]);
  assert.equal(stoppedSends.at(-1), 'p-i3');
  assert.ok(!stoppedSends.includes('p-i2'));
  const slow = first.checkpoint.pendingSlices.find(row => row.itemIds[0] === 'i2');
  assert.equal((await saved(slow.childPath)).prepareJobId, 'prepare-i2');
  assert.ok(first.checkpoint.pendingSlices.some(row => row.itemIds[0] === 'i4'));
  recovering = true; f.allowReconcile();
  await f.run({ ...options(f.path), checkpointPath: undefined, resumePath: f.path });
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
});

test('recovery from the persisted reordered head adopts original jobs before admitting any new work', { timeout: 5000 }, async t => {
  const f = await fourWorkers(t);
  const poll = f.client.getJob, bootstrap = f.client.bootstrap;
  let crashBoundary, recovering = false, slowAborted = false;
  f.client.getJob = async (id, options) => {
    // i3 must be the next ready head for this exact crash boundary. The
    // independent i4 producer could otherwise legitimately send before it.
    if (['prepare-i2', 'prepare-i4'].includes(id) && !recovering) await new Promise((resolve, reject) => {
      const abort = () => { slowAborted = true; reject(options.signal.reason); };
      if (options.signal.aborted) abort();
      else options.signal.addEventListener('abort', abort, { once: true });
    });
    return poll(id, options);
  };
  f.client.bootstrap = async () => {
    const parent = await saved(f.path);
    if (!recovering && parent.currentSlice?.itemIds[0] === 'i3') {
      // This is the immediately preceding durable boundary: reordered head
      // published, current child not yet claimed. No i3 effect was admitted.
      crashBoundary = { ...parent, currentSlice: null };
      throw new CliError('Simulated interrupted handoff', { code: 'TEST_CRASH' });
    }
    return bootstrap();
  };
  const stopped = await f.run(options(f.path));
  assert.equal(stopped.stopReason, 'overlapped-slice-unresolved');
  assert.equal(slowAborted, true);
  assert.deepEqual(crashBoundary.pendingSlices[0].itemIds, ['i3']);
  assert.ok(f.calls.filter(row => row[0] === 'execute').every(row => row[1] === 'p-i1'));
  const { writeCheckpoint } = await import('../cli/workflow.mjs');
  await writeCheckpoint(f.path, crashBoundary);
  recovering = true;
  const result = await f.run({ ...options(f.path), checkpointPath: undefined, resumePath: f.path });
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
  assert.equal(result.checkpoint.pendingSlices.length, 0);
});

test('width four UNKNOWN joins all producers and resumes original paid children without any duplicate admission', async t => {
  const f = await fourWorkers(t, { unknown: true }); const first = await f.run(options(f.path));
  assert.equal(first.stopReason, 'overlapped-slice-unresolved');
  assert.equal(f.calls.filter(row => row[0] === 'execute').at(-1)[1], 'p-i1');
  for (const child of first.checkpoint.pendingSlices) assert.ok((await saved(child.childPath)).prepareJobId);
  f.allowReconcile();
  await f.run({ ...options(f.path), checkpointPath: undefined, resumePath: f.path });
  assert.deepEqual(f.calls.filter(row => row[0] === 'prepare').map(row => row[1]).sort(), ['i1', 'i2', 'i3', 'i4']);
  assert.deepEqual(f.calls.filter(row => row[0] === 'execute').map(row => row[1]).sort(), ['p-i1', 'p-i2', 'p-i3', 'p-i4']);
});

test('reserved next scope prepares during blocked current sender with only one sender and no third producer', async t => {
  const { path, client, calls } = await fixture(t);
  const result = await runQueue(client, options(path));
  assert.equal(result.mode, 'stopped'); assert.equal(result.stopReason, 'max-cycles', JSON.stringify(result.checkpoint.slices));
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1', 'i2']);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1', 'p-i2']);
  const nextPrepare = calls.findIndex(row => row[0] === 'prepare' && row[1] === 'i2');
  const nextSend = calls.findIndex(row => row[0] === 'execute' && row[1] === 'p-i2');
  assert.ok(nextPrepare < calls.findIndex(row => row[0] === 'poll' && row[1] === 'execute-p-i1'));
  assert.ok(nextSend > calls.findIndex(row => row[0] === 'poll' && row[1] === 'prepare-i2'));
  assert.deepEqual(result.checkpoint.pendingSlices, []);
});

for (const version of [undefined, 2]) test(`unverified reservation capability ${version} keeps the original single scope`, async t => {
  const { path, client, calls } = await fixture(t, { version, block: false });
  if (version === undefined) client.engineStatus = async () => ({strictGrouping:{version:1,contract:'strict_post_family_v1'}});
  await runQueue(client, options(path));
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1']);
});

test('UNKNOWN current scope retains paid next job without sending it; recovery never repeats preparation or execute', async t => {
  const { path, client, calls, allowReconcile } = await fixture(t, { unknown: true });
  const result = await runQueue(client, options(path));
  assert.equal(result.stopReason, 'overlapped-slice-unresolved');
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
  const pending = result.checkpoint.pendingSlices[0];
  assert.equal((await saved(pending.childPath)).prepareJobId, 'prepare-i2');
  await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
  allowReconcile();
  await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1', 'p-i2']);
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1', 'i2']);
});

test('server capability removal and crash with a saved current child preserve UNKNOWN quarantine', async t => {
  const { path, client, calls } = await fixture(t, { unknown: true });
  const result = await runQueue(client, options(path));
  const state = await saved(path);
  // Restore the earlier durable crash boundary: A still at pending head and
  // currentSlice, while paid B already has its separate original checkpoint.
  const a = state.slices[0]; state.currentSlice = a;
  state.pendingSlices = [a, ...state.pendingSlices]; state.stopReason = null; state.phase = 'running';
  const { writeCheckpoint } = await import('../cli/workflow.mjs'); await writeCheckpoint(path, state);
  client.engineStatus = async () => ({});
  const resumed = await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.equal(resumed.stopReason, 'overlapped-slice-unresolved');
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
  assert.equal(resumed.checkpoint.pendingSlices[0].id, result.checkpoint.pendingSlices[0].id);
});

test('stopping at next prepare request makes no POST and preserves its pending exact selection', async t => {
  const { path, client, calls, release } = await fixture(t);
  const controller = new AbortController();
  const result = await runQueue(client, { ...options(path), signal: controller.signal, onProgress: event => {
    if (event.preparationOnly && event.event === 'prepare.request') { controller.abort(); release(); }
  } });
  assert.equal(result.stopReason, 'overlapped-slice-unresolved');
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1']);
  const checkpoint = await saved(result.checkpoint.pendingSlices[0].childPath);
  assert.equal(checkpoint.prepareJobId, undefined); assert.equal(checkpoint.pendingLocalAdmission, null);
  assert.deepEqual(checkpoint.itemIds, ['i2']);
});

test('lost next preparation response recovers its exact receipt without another admission', async t => {
  const { path, client, calls, release } = await fixture(t);
  const prepare = client.prepareEngine;
  client.prepareEngine = async (...args) => {
    const result = await prepare(...args);
    if (args[0].itemIds[0] === 'i2') { release(); throw new UnknownMutationError('POST', '/api/engine/prepare', { code: 'TIMEOUT' }); }
    return result;
  };
  const result = await runQueue(client, options(path));
  assert.equal(result.stopReason, 'max-cycles');
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1', 'i2']);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1', 'p-i2']);
});

test('missing next reservation proof fails closed across resume and keeps the original job', async t => {
  const { path, client, calls, jobs, release } = await fixture(t);
  const prepare = client.prepareEngine;
  client.prepareEngine = async (...args) => {
    const result = await prepare(...args);
    if (args[0].itemIds[0] === 'i2') {
      delete result.scopeReservation; delete jobs.get(result.jobId).scopeReservation; release();
    }
    return result;
  };
  const result = await runQueue(client, options(path));
  assert.equal(result.stopReason, 'overlapped-slice-unresolved');
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
  await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1', 'i2']);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
  assert.equal((await saved(result.checkpoint.slices[1].childPath)).prepareJobId, 'prepare-i2');
});

test('explicit next reservation conflict remains a hold without immediate admission retry', async t => {
  const { path, client, calls, release } = await fixture(t);
  const prepare = client.prepareEngine;
  client.prepareEngine = async (...args) => {
    if (args[0].itemIds[0] === 'i2') {
      calls.push(['rejected-prepare', 'i2']); release();
      throw new CliError('Scope is reserved', { code: 'STALE_OR_CONFLICT', status: 409 });
    }
    return prepare(...args);
  };
  const result = await runQueue(client, options(path));
  assert.equal(result.stopReason, 'overlapped-slice-unresolved');
  await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.equal(calls.filter(row => row[0] === 'rejected-prepare').length, 1);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i1']);
});

test('next producer skips a known conflicting branch without dropping its deferred recipient', async t => {
  const { path, client, calls, items } = await fixture(t, { block: false });
  items[1].conversationKey = items[0].conversationKey;
  const result = await runQueue(client, options(path));
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i1', 'i3']);
  assert.equal(items[1].workflow, 'attention');
  assert.equal(result.checkpoint.attemptedItemIds.includes('i2'), false);
});

test('planner reserves only affected recipients while independent later groups still prepare and send', async t => {
  const { path, client, calls, items, jobs, operations } = await fixture(t, { block: false });
  // An old uncertain paid owner is not a new recipient attempt and must never
  // be polled, replaced, retried or released by this queue pass.
  jobs.set('old-owner', { id: 'old-owner', kind: 'assistant', purpose: 'engine_prepare', status: 'unknown' });
  const detail = 'Prior paid or unfinished preparation owns this recipient or branch';
  client.planPrepare = async ids => ({
    batches: ids.filter(id => id !== 'i1').map(id => ({ itemIds: [id], bytes: 1000 })),
    held: ids.includes('i1') ? [{ itemId: 'i1', reason: 'preparation_scope_reserved', detail }] : []
  });
  const result = await runQueue(client, { ...options(path), batchSize: 3 });
  assert.equal(result.stopReason, 'max-cycles');
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i2', 'i3']);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i2', 'p-i3']);
  const hold = result.checkpoint.slices.find(row => row.status === 'plan-held');
  assert.deepEqual(hold.itemIds, ['i1']);
  assert.equal(hold.planHoldReason, 'preparation_scope_reserved');
  assert.equal(hold.planHoldDetail, detail);
  assert.equal(hold.child, null); assert.equal(hold.error, null);
  assert.equal(items[0].workflow, 'attention');
  assert.equal(jobs.get('old-owner').status, 'unknown');
  assert.equal(calls.some(row => row[1] === 'old-owner'), false);
  assert.equal(result.counts.held, 1); assert.equal(result.counts.sliceFailures, 0);
  assert.deepEqual(operations.map(row => [row.itemId, row.status]), [['i2', 'succeeded'], ['i3', 'succeeded']]);
  assert.deepEqual(result.checkpoint.pendingSlices, []);
  assert.equal((await saved(path)).slices.find(row => row.status === 'plan-held').planHoldDetail, detail);
});

test('all planner reservation holds remain durable without preparing any recipient', async t => {
  const { path, client, calls, items } = await fixture(t, { block: false });
  client.planPrepare = async ids => ({ batches: [], held: ids.map(itemId => ({ itemId, reason: 'preparation_scope_reserved' })) });
  const result = await runQueue(client, { ...options(path), batchSize: 3 });
  assert.equal(result.stopReason, 'max-cycles');
  assert.equal(calls.some(row => ['prepare', 'editorial', 'approve', 'execute'].includes(row[0])), false);
  assert.equal(result.counts.held, 3); assert.equal(result.counts.sliceFailures, 0);
  assert.deepEqual(result.checkpoint.attemptedItemIds, ['i1', 'i2', 'i3']);
  assert.ok(result.checkpoint.slices.every(row => row.status === 'plan-held' && row.child === null));
  assert.ok(items.every(row => row.workflow === 'attention'));
});

test('resuming a planner hold at pending head retains an already paid independent child identity', async t => {
  const { path, client, calls } = await fixture(t, { block: false });
  const { generateProposals, writeCheckpoint } = await import('../cli/workflow.mjs');
  const childPath = `${path}.slices/paid-independent.json`;
  const child = await generateProposals(client, ['i2'], { checkpointPath: childPath, materialsAlreadyRefreshed: true,
    planAlreadyChecked: true, requireScopeReservation: true, pollMs: 0 });
  assert.equal(child.prepareJobId, 'prepare-i2');
  await writeCheckpoint(path, { kind: 'communityhero-queue', account: client.account, baseUrl: client.baseUrl,
    phase: 'running', materialsReady: true, cycle: 1, syncCycles: 1, overlapVersion: 1,
    attemptedItemIds: [], slices: [], pendingSlices: [
      { id: 'reserved', itemIds: ['i1'], planHoldReason: 'preparation_scope_reserved', planHoldDetail: 'Original paid owner remains held' },
      { id: 'paid-independent', itemIds: ['i2'], plannedBytes: 1000, childPath },
      { id: 'later-independent', itemIds: ['i3'], plannedBytes: 1000, childPath: `${path}.slices/later-independent.json` }
    ] });
  const result = await runQueue(client, { ...options(path), checkpointPath: undefined, resumePath: path });
  assert.equal(result.stopReason, 'max-cycles');
  assert.deepEqual(calls.filter(row => row[0] === 'prepare').map(row => row[1]), ['i2', 'i3']);
  assert.deepEqual(calls.filter(row => row[0] === 'execute').map(row => row[1]), ['p-i2', 'p-i3']);
  assert.equal((await saved(childPath)).prepareJobId, child.prepareJobId);
  assert.equal((await saved(childPath)).prepareRequestId, child.prepareRequestId);
  assert.equal(result.checkpoint.slices[0].status, 'plan-held');
  assert.equal(result.counts.held, 1);
  assert.deepEqual(result.checkpoint.pendingSlices, []);
});

test('same-company producer checkpoint cannot substitute different in-grant recipients before receipt or model work', async t => {
  const { client, calls } = await fixture(t, { block: false });
  const { generateProposals } = await import('../cli/workflow.mjs');
  const checkpoint = { account: client.account, baseUrl: client.baseUrl, phase: 'prepare-admitting', itemIds: ['i3'],
    instruction: 'same company', proposals: [], prepareRequestId: 'original',
    pendingLocalAdmission: { kind: 'prepare', requestId: 'original', payloadHash: 'a'.repeat(64) } };
  client.localAdmission = async () => assert.fail('Mismatch must fail before original receipt lookup');
  await assert.rejects(generateProposals(client, ['i2'], { checkpoint, requireScopeReservation: true }), { code: 'INVALID_CHECKPOINT' });
  assert.deepEqual(calls, []);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { createQueuePreparePlanning } from '../cli/queue-prepare-planning.mjs';
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => { resolve=a;reject=b; }); return { promise,resolve,reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));

test('a completed later window is consumable before the first window finishes', async () => {
  const gates=[deferred(),deferred()], calls=[];
  const plans=createQueuePreparePlanning({ async planPrepare(ids) { const i=Number(ids[0]);calls.push(i);await gates[i].promise;return {batches:[{itemIds:ids,bytes:10}],held:[]}; } },[['0'],['1']], 'test', {parallelism:2});
  assert.deepEqual(calls,[0,1]);gates[1].resolve();const ready=await plans.next();
  assert.equal(ready.window,1);assert.deepEqual(ready.batches[0].itemIds,['1']);assert.equal(plans.done,false);
  assert.equal(await plans.next({wait:false}),undefined);gates[0].resolve();assert.equal((await plans.next()).window,0);
  await plans.joinAll();assert.equal(plans.done,true);
});
test('bounded workers retain stable original window identity and a copied selection', async () => {
  const gates=[deferred(),deferred(),deferred()], windows=[['0'],['1'],['2']], calls=[];
  const plans=createQueuePreparePlanning({async planPrepare(ids){const i=Number(ids[0]);calls.push(i);await gates[i].promise;return {batches:[],held:[]};}},windows,'test',{parallelism:2});
  windows[2][0]='tampered';assert.deepEqual(calls,[0,1]);gates[1].resolve();assert.equal((await plans.next()).window,1);
  assert.deepEqual(calls,[0,1,2]);gates[2].resolve();assert.equal((await plans.next()).window,2);gates[0].resolve();assert.equal((await plans.next()).window,0);await plans.joinAll();
});
test('a failed reader stops new readers, surfaces original failure and joins started siblings only at cleanup', async () => {
  const gates=[deferred(),deferred()], failure=new Error('original read failure'), calls=[];
  const plans=createQueuePreparePlanning({async planPrepare(ids){const i=Number(ids[0]);calls.push(i);await gates[i].promise;return {batches:[],held:[]};}},[['0'],['1'],['2']],'test',{parallelism:2});
  gates[0].reject(failure);await assert.rejects(plans.next(),error=>error===failure);
  let joined=false;const cleaning=plans.joinAll({stop:true}).then(()=>{joined=true;});await tick();assert.equal(joined,false);
  gates[1].resolve();await cleaning;assert.deepEqual(calls,[0,1]);await assert.rejects(plans.next({wait:false}),error=>error===failure);
});
test('overlapping or malformed selections cannot start any reader', () => {
  const client={planPrepare(){assert.fail('invalid selection invoked reader');}};
  for(const windows of [[],[['a'],['a']],[[]],[[null]],Array.from({length:9},(_,i)=>[String(i)])])
    assert.throws(()=>createQueuePreparePlanning(client,windows,'test',{parallelism:2}));
});

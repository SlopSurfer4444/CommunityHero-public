import test from 'node:test';
import assert from 'node:assert/strict';
import {createPrepareProducers} from '../cli/prepare-producers.mjs';

const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};
const tick=()=>new Promise(resolve=>setImmediate(resolve));

test('ready wait observes a sibling registered after the first observer began waiting',async()=>{
  const manager=createPrepareProducers({maxProducers:2}),first=deferred(),second=deferred();
  manager.start({id:'first',windowId:'one',run:()=>first.promise});
  const ready=manager.joinReady({wait:true});
  await tick();
  manager.start({id:'second',windowId:'two',run:()=>second.promise});
  second.resolve({prepareJobId:'second-paid',phase:'prepared'});
  assert.deepEqual(await ready,[{id:'second',checkpoint:{prepareJobId:'second-paid',phase:'prepared'}}]);
  assert.equal(manager.has('first'),true);
  first.resolve({prepareJobId:'first-paid',phase:'prepared'});
  await manager.joinAll();
});

test('ready drain releases completed children without joining a slow older sibling',async()=>{
  const manager=createPrepareProducers({maxProducers:3}),slow=deferred(),ready=deferred(),failed=deferred();
  manager.start({id:'slow',windowId:'one',run:()=>slow.promise});
  manager.start({id:'ready',windowId:'two',run:()=>ready.promise});
  manager.start({id:'failed',windowId:'three',run:()=>failed.promise});
  assert.deepEqual(await manager.joinReady(),[]);
  const first=manager.joinReady({wait:true});
  ready.resolve({prepareJobId:'original-ready',phase:'prepared'});
  assert.deepEqual(await first,[{id:'ready',checkpoint:{prepareJobId:'original-ready',phase:'prepared'}}]);
  assert.equal(manager.has('slow'),true);assert.equal(manager.has('ready'),false);
  const failure=new Error('original failed child');failed.reject(failure);
  assert.deepEqual(await manager.joinReady({wait:true}),[{id:'failed',error:failure}]);
  assert.equal(manager.has('slow'),true);
  slow.resolve({prepareJobId:'original-slow',phase:'prepared'});
  assert.deepEqual(await manager.joinReady({wait:true}),[{id:'slow',checkpoint:{prepareJobId:'original-slow',phase:'prepared'}}]);
  assert.deepEqual(await manager.joinReady({wait:true}),[]);
});

test('width three owns distinct children and windows until individually joined',async()=>{
  const manager=createPrepareProducers({maxProducers:3});
  const pending=Array.from({length:3},deferred);let active=0,peak=0;
  for(let i=0;i<3;i++)assert.equal(manager.start({id:`child-${i}`,windowId:`family-${i}`,run:async()=>{
    active++;peak=Math.max(peak,active);try{return await pending[i].promise;}finally {active--;}
  }}),true);
  assert.equal(manager.start({id:'other',windowId:'other',run:()=>{throw new Error('must not run');}}),false);
  assert.equal(manager.start({id:'child-0',windowId:'different',run:()=>{throw new Error('duplicate child');}}),false);
  assert.equal(manager.start({id:'different',windowId:'family-0',run:()=>{throw new Error('duplicate family');}}),false);
  await tick();assert.equal(peak,3);
  const checkpoint={prepareJobId:'ack-server-job',phase:'prepared'};
  pending[0].resolve(checkpoint);await tick();
  assert.equal(manager.start({id:'other',windowId:'other',run:()=>{throw new Error('unconsumed slot');}}),false);
  assert.deepEqual(await manager.join('child-0'),{id:'child-0',checkpoint});
  assert.equal(manager.start({id:'other',windowId:'family-0',run:async()=>({phase:'prepared'})}),true);
  pending[1].resolve('one');pending[2].resolve('two');
  assert.deepEqual(await manager.joinAll(),[{id:'child-1',checkpoint:'one'},{id:'child-2',checkpoint:'two'},
    {id:'other',checkpoint:{phase:'prepared'}}]);
  assert.deepEqual(await manager.joinAll(),[]);assert.equal(await manager.join('missing'),undefined);
});

test('synchronous and asynchronous producer failures are captured without detached rejection',async()=>{
  const manager=createPrepareProducers({maxProducers:2}),sync=new Error('sync'),async=new Error('async');
  const unhandled=[];const observe=error=>unhandled.push(error);
  process.on('unhandledRejection',observe);
  try {
    manager.start({id:'sync',windowId:'one',run:()=>{throw sync;}});
    manager.start({id:'async',windowId:'two',run:()=>Promise.reject(async)});
    await tick();await tick();assert.deepEqual(unhandled,[]);
    assert.deepEqual(await manager.joinAll(),[{id:'sync',error:sync},{id:'async',error:async}]);
  }finally {process.removeListener('unhandledRejection',observe);}
});

test('stop aborts observation but joins the owner and retains its acknowledged checkpoint',async()=>{
  const manager=createPrepareProducers(),finished=deferred();let childSignal,observedAbort=false,joinFinished=false;
  const checkpoint={prepareJobId:'server-still-running',phase:'prepare-polling',requestId:'persisted-request'};
  manager.start({id:'persisted-child',windowId:'family',run:async signal=>{
    childSignal=signal;await finished.promise;return checkpoint;
  }});
  await tick();childSignal.addEventListener('abort',()=>{observedAbort=true;},{once:true});
  const joined=manager.joinAll({stop:true}).then(results=>{joinFinished=true;return results;});
  await tick();assert.equal(observedAbort,true);assert.equal(joinFinished,false);
  assert.equal(manager.start({id:'new',windowId:'new',run:()=>{throw new Error('closed');}}),false);
  finished.resolve();assert.deepEqual(await joined,[{id:'persisted-child',checkpoint}]);
  assert.equal(checkpoint.prepareJobId,'server-still-running');
});

test('parent abort stops existing observers, refuses future starts and removes settled listeners',async()=>{
  const parent=new AbortController(),reason=new Error('parent stopped'),manager=createPrepareProducers({signal:parent.signal});
  let listenerAdds=0,listenerRemoves=0;
  const originalAdd=parent.signal.addEventListener.bind(parent.signal),originalRemove=parent.signal.removeEventListener.bind(parent.signal);
  parent.signal.addEventListener=(...args)=>{listenerAdds++;return originalAdd(...args);};
  parent.signal.removeEventListener=(...args)=>{listenerRemoves++;return originalRemove(...args);};
  manager.start({id:'observing',windowId:'family',run:signal=>new Promise((resolve,reject)=>{
    signal.addEventListener('abort',()=>reject(signal.reason),{once:true});
  })});
  await tick();parent.abort(reason);
  assert.deepEqual(await manager.joinAll(),[{id:'observing',error:reason}]);
  assert.equal(listenerAdds,1);assert.equal(listenerRemoves,1);
  assert.equal(manager.start({id:'new',windowId:'new',run:()=>{throw new Error('must not start');}}),false);
});

test('abort before factory dispatch cannot start work; width one is reusable after join and width zero stays idle',async()=>{
  const parent=new AbortController(),manager=createPrepareProducers({signal:parent.signal});let starts=0;
  manager.start({id:'queued',windowId:'family',run:()=>{starts++;return 'unexpected';}});parent.abort();
  const [result]=await manager.joinAll();assert.equal(starts,0);assert.equal(result.error.name,'AbortError');
  const single=createPrepareProducers();
  for(const id of ['first','second']){
    assert.equal(single.start({id,windowId:'same-family',run:async()=>id}),true);
    assert.equal(single.start({id:'other',windowId:'other',run:async()=>null}),false);
    assert.deepEqual(await single.join(id),{id,checkpoint:id});
  }
  const zero=createPrepareProducers({maxProducers:0});
  assert.equal(zero.start({id:'zero',windowId:'zero',run:async()=>null}),false);
  assert.deepEqual(await zero.joinAll(),[]);
  assert.throws(()=>createPrepareProducers({maxProducers:-1}),TypeError);
  assert.throws(()=>createPrepareProducers({maxProducers:1.5}),TypeError);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

function fixture(t,{post,readback}={}){
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const items=[1,2].map(n=>({id:`item-${n}`,itemId:`comment-${n}`,revision:1,workflow:'attention',providerStatus:'new',
    platform:'vk',draft:'',contextEvidenceDigest:'a'.repeat(64),branchContextDigest:'b'.repeat(64),author:`Author ${n}`,text:`Comment ${n}`}));
  let raw={operator:{id:'operator-1'},csrfToken:'csrf',account:'LikeAvto',items,posts:[],branches:[],proposals:[],operations:[]};
  const saved={items:{}},calls=[],dialogs=[],messages=[];
  globalThis.document={activeElement:null,body:{append(){}},createElement:tag=>{
    if(tag==='textarea')return {set innerHTML(value){this.value=value;},value:''};
    const button={disabled:false,addEventListener(){}};
    const dialog={showModal(){dialogs.push(this);},remove(){},close(){},addEventListener(){},querySelectorAll:()=>[],
      querySelector:selector=>selector==='[data-confirm]'?button:{hidden:true,textContent:''},set innerHTML(value){this.html=value;}};
    return dialog;
  }};
  let data={items:[]};
  const receipt=(body,statuses=['created','created'])=>({requestId:body.requestId,created:statuses.filter(x=>x==='created').length,
    existing:statuses.filter(x=>x==='existing').length,rejected:statuses.filter(x=>x==='rejected').length,replayed:false,
    results:statuses.map((status,index)=>({index,itemId:body.proposals[index].itemId,status,
      ...(status==='rejected'?{httpStatus:409,error:'Stale revision'}:{proposalId:`proposal-${index+1}`,proposalRevision:1,itemRevision:2})}))});
  const publish=(body,statuses=['created','created'])=>{
    raw={...raw,items:items.map((item,index)=>({...item,revision:statuses[index]==='rejected'?1:2,workflow:statuses[index]==='rejected'?'attention':'prepared'})),
      proposals:statuses.flatMap((status,index)=>status==='rejected'?[]:[{id:`proposal-${index+1}`,itemId:items[index].id,revision:1,
        itemRevision:2,kind:'close',text:'',status:'draft',contextEvidenceDigest:items[index].contextEvidenceDigest,
        branchContextDigest:items[index].branchContextDigest}])};
  };
  globalThis.fetch=async(path,options={})=>{
    const body=options.body?JSON.parse(options.body):undefined;
    calls.push({path,method:options.method||'GET',body});
    if(path==='/api/bootstrap')return {ok:true,json:async()=>raw};
    if(path==='/api/proposals/batch')return post({body,receipt,publish});
    if(path.startsWith('/api/proposals/batch/'))return readback({path,receipt,publish});
    throw Error(`Unexpected request ${path}`);
  };
  const make=async()=>{
    const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,
      stateFor:item=>saved.items[item.id]||=structuredClone(item.initialState),persist(){},render(){},announce:message=>messages.push(message),
      esc:String,icon:()=>''});
    data=await connection.load();connection.hydrate(undefined,{repaint:false});
    t.after(()=>connection.stop());return connection;
  };
  return {items,saved,calls,dialogs,messages,make,receipt,publish};
}
const pendingBatch=saved=>Object.values(saved.mvpPendingProposalBatches||{})[0];

test('close group sends one exact proposal batch and opens human review only after refresh',async t=>{
  const f=fixture(t,{post:async({body,receipt,publish})=>{publish(body);return {ok:true,json:async()=>receipt(body)};}});
  const connection=await f.make();await connection.closeMany(f.items);
  const writes=f.calls.filter(call=>call.method==='POST');
  assert.deepEqual(writes.map(call=>call.path),['/api/proposals/batch']);
  assert.equal(writes[0].body.proposals.length,2);
  assert.equal(new Set(writes[0].body.proposals.map(entry=>entry.itemId)).size,2);
  for(const entry of writes[0].body.proposals){
    assert.equal(entry.kind,'close');assert.equal(entry.expectedRevision,1);assert.equal(entry.text,'');
    assert.ok(entry.eventId&&entry.sessionId&&entry.draftSessionId);
  }
  assert.equal(f.dialogs.length,1);
  assert.match(f.dialogs[0].html,/Проверить действия/);
  assert.match(f.dialogs[0].html,/Подтвердить и выполнить 2/);
  assert.equal(pendingBatch(f.saved),undefined);
});

test('lost POST response reads durable receipt with the same key and no second POST',async t=>{
  let posted;
  const f=fixture(t,{post:async({body,receipt,publish})=>{posted=receipt(body);publish(body);throw Error('Connection lost');},
    readback:async()=>({ok:true,json:async()=>posted})});
  const connection=await f.make();await connection.closeMany(f.items);
  assert.equal(f.calls.filter(call=>call.path==='/api/proposals/batch').length,1);
  assert.equal(f.calls.filter(call=>call.path.startsWith('/api/proposals/batch/')).length,1);
  assert.equal(f.calls.find(call=>call.path.startsWith('/api/proposals/batch/')).path.split('/').at(-1),posted.requestId);
  assert.equal(f.dialogs.length,1);
});

test('unknown result survives a new connection and retries lookup without resending',async t=>{
  let available=false,posted;
  const f=fixture(t,{post:async({body,receipt,publish})=>{posted=receipt(body);publish(body);throw Error('Connection lost');},
    readback:async()=>available?{ok:true,json:async()=>posted}:{ok:false,status:404,json:async()=>({error:'Not found'})}});
  const first=await f.make();
  await assert.rejects(first.closeMany(f.items),/Результат группы пока неизвестен/);
  assert.equal(pendingBatch(f.saved).body.requestId,posted.requestId);
  first.stop();available=true;
  const second=await f.make();await second.closeMany(f.items);
  assert.equal(f.calls.filter(call=>call.path==='/api/proposals/batch').length,1);
  assert.equal(f.dialogs.length,1);assert.equal(pendingBatch(f.saved),undefined);
});

test('a later explicit retry replays the exact durable request after receipt 404',async t=>{
  let first=true;
  const f=fixture(t,{post:async({body,receipt,publish})=>{
    if(first){first=false;throw Error('Connection lost before commit');}
    publish(body);return {ok:true,json:async()=>receipt(body)};
  },readback:async()=>({ok:false,status:404,json:async()=>({error:'Not found'})})});
  const connection=await f.make();
  await assert.rejects(connection.closeMany(f.items),/Результат группы пока неизвестен/);
  const pending=pendingBatch(f.saved);
  await connection.closeMany(f.items);
  const posts=f.calls.filter(call=>call.path==='/api/proposals/batch');
  assert.equal(posts.length,2);
  assert.deepEqual(posts[0].body,posts[1].body);
  assert.equal(posts[0].body.requestId,pending.body.requestId);
  assert.equal(f.dialogs.length,1);
});

test('partial rejection does not open review or silently approve successful subset',async t=>{
  const f=fixture(t,{post:async({body,receipt,publish})=>{publish(body,['created','rejected']);return {ok:true,json:async()=>receipt(body,['created','rejected'])};}});
  const connection=await f.make();await connection.closeMany(f.items);
  assert.equal(f.dialogs.length,0);
  assert.match(f.messages.at(-1),/частично: 1 из 2/);
  assert.equal(f.calls.filter(call=>call.method==='POST').length,1);
});

test('malformed receipt remains pending and cannot enter review',async t=>{
  const f=fixture(t,{post:async({body,receipt,publish})=>{publish(body);return {ok:true,json:async()=>({...receipt(body),results:[{index:1,itemId:body.proposals[0].itemId,status:'created',proposalId:'wrong',proposalRevision:1}]})};}});
  const connection=await f.make();
  await assert.rejects(connection.closeMany(f.items),/Ответ пакета неполный/);
  assert.equal(f.dialogs.length,0);
  assert.ok(pendingBatch(f.saved)?.body.requestId);
});

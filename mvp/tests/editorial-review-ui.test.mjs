import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

// Offline DOM/HTTP fixture. No model, server, company database or provider.
function fixture(t,{submit,lookup,job}={}){
  const original={fetch:globalThis.fetch,document:globalThis.document};
  t.after(()=>Object.assign(globalThis,original));
  const item={id:'item-1',itemId:'comment-1',revision:1,workflow:'prepared',providerStatus:'new',platform:'vk',draft:'Мой ответ',
    contextEvidenceDigest:'context',branchContextDigest:'branch',author:'Автор',text:'Комментарий'};
  const proposal={id:'proposal-1',itemId:item.id,revision:1,itemRevision:1,kind:'reply_and_close',text:'Мой ответ',status:'draft',
    contextEvidenceDigest:'context',branchContextDigest:'branch'};
  let raw={operator:{id:'operator-1'},csrfToken:'csrf',account:'LikeAvto',items:[item],posts:[],branches:[],proposals:[proposal],operations:[],jobs:[]};
  const saved={items:{}},calls=[],dialogs=[],messages=[],connections=[];
  const control=()=>({disabled:false,hidden:false,checked:false,textContent:'',innerHTML:'',listeners:{},dataset:{},
    addEventListener(name,handler){this.listeners[name]=handler;},async click(){if(!this.disabled)await this.listeners.click?.({currentTarget:this});}});
  globalThis.document={activeElement:null,body:{append(){}},createElement(tag){
    if(tag==='textarea')return {set innerHTML(value){this.value=value;},value:''};
    const selectors=['confirm','close','partial-approval','admission-readback','admission-retire','admission-result','media-readiness','editorial-result','editorial-readback'];
    const controls=Object.fromEntries(selectors.map(name=>[`[data-${name}]`,control()]));controls['#mvp-history-order']=control();
    return {controls,html:'',listeners:{},remove(){},showModal(){dialogs.push(this);},close(){this.listeners.close?.();},
      addEventListener(name,handler){this.listeners[name]=handler;},querySelector:selector=>controls[selector]||null,
      querySelectorAll(selector){
        if(selector!=='[data-editorial-review]')return [];
        return this.editorialButtons||=[...this.html.matchAll(/data-editorial-review="([^"]+)"/g)].map(match=>{
          const button=control();button.dataset.editorialReview=match[1];return button;
        });
      },set innerHTML(value){this.html=value;}};
  }};
  const ok=value=>({ok:true,json:async()=>value});
  const receipt=body=>({jobId:'editorial-job-1',requestId:body.requestId,replayed:false});
  let lastBody;
  const outcome=()=>({accepted:lastBody.proposals,reused:[],held:[]});
  const completed=()=>({id:'editorial-job-1',kind:'editorial_review',purpose:'editorial_review',refId:lastBody.requestId,status:'completed',result:outcome()});
  globalThis.fetch=async(path,options={})=>{
    const body=options.body?JSON.parse(options.body):undefined;calls.push({path,method:options.method||'GET',body});
    if(path==='/api/bootstrap')return ok(raw);
    if(path==='/api/proposals/editorial-review'){lastBody=body;return submit?submit({body,receipt,ok}):ok(receipt(body));}
    if(path.startsWith('/api/local-admissions/editorial/'))return lookup?lookup({path,body:lastBody,receipt,ok}):ok({kind:'editorial',status:'committed',requestId:lastBody.requestId,result:receipt(lastBody)});
    if(path==='/api/jobs/editorial-job-1')return job?job({completed,outcome,ok}):ok(completed());
    if(path==='/api/approvals')return ok({id:'approval-1',status:'approved',requestId:body.requestId,replayed:false});
    if(path==='/api/approvals/approval-1/execute')return ok({jobId:'execute-job-1'});
    throw Error(`Unexpected request ${path}`);
  };
  let data={items:[]};
  const make=async()=>{
    const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:item=>saved.items[item.id]||=structuredClone(item.initialState),
      persist(){},render(){},announce:message=>messages.push(message),esc:String,icon:()=>''});
    connections.push(connection);data=await connection.load();connection.hydrate(undefined,{repaint:false});return connection;
  };
  t.after(()=>connections.forEach(connection=>connection.stop()));
  return {saved,calls,dialogs,messages,make,completed,ok,receipt,get raw(){return raw;},set raw(value){raw=value;},
    dialog:()=>dialogs.at(-1),button:name=>dialogs.at(-1).controls[`[data-${name}]`],review:connection=>connection.review(['proposal-1']),
    posts:()=>calls.filter(call=>call.method==='POST'),pending:()=>Object.values(saved.mvpPendingEditorial||{})[0]};
}

test('final reply review starts an editorial job only; accepted exact version still needs explicit send confirmation',async t=>{
  const f=fixture(t);const connection=await f.make();f.review(connection);
  assert.equal(f.button('confirm').textContent,'Проверить ответ');await f.button('confirm').click();
  assert.deepEqual(f.posts().map(call=>call.path),['/api/proposals/editorial-review']);
  assert.deepEqual(f.posts()[0].body.proposals,[{id:'proposal-1',revision:1}]);
  assert.match(f.posts()[0].body.requestId,/^[a-f0-9-]{36}$/);
  assert.match(f.button('editorial-result').innerHTML,/Проверка завершена/);
  await f.button('confirm').click();
  assert.deepEqual(f.posts().map(call=>call.path),['/api/proposals/editorial-review','/api/approvals','/api/approvals/approval-1/execute']);
});

test('pending model job returns promptly and existing workspace refresh admits matching completed outcome',async t=>{
  const f=fixture(t,{job:async({completed,ok})=>ok({...completed(),status:'running',result:null})});
  const connection=await f.make();f.review(connection);await f.button('confirm').click();
  assert.equal(f.button('confirm').disabled,true);assert.equal(f.button('editorial-readback').hidden,false);
  assert.match(f.button('editorial-result').innerHTML,/Можно вернуться к работе/);
  f.raw={...f.raw,jobs:[f.completed()]};connection.hydrate(f.raw,{repaint:false});
  assert.equal(f.button('confirm').disabled,false);assert.equal(f.posts().length,1);
});

test('hold and revise show reasons and suggestion without changing draft or approving partial group',async t=>{
  for(const decision of ['revise','hold']){
    const f=fixture(t,{job:async({completed,ok})=>ok({...completed(),result:{accepted:[],reused:[],held:[{reference:{id:'proposal-1',revision:1},decision,reason:'Проверьте намерение автора',...(decision==='revise'?{suggestedText:'Вариант редактора'}:{})}]}})});
    const connection=await f.make();f.review(connection);await f.button('confirm').click();
    assert.equal(f.button('confirm').disabled,true);assert.match(f.button('editorial-result').innerHTML,/Проверьте намерение автора/);
    if(decision==='revise')assert.match(f.button('editorial-result').innerHTML,/Вариант редактора/);
    else assert.doesNotMatch(f.button('editorial-result').innerHTML,/Предложенный вариант/);
    assert.equal(f.saved.items['item-1'].draft,'Мой ответ');assert.equal(f.raw.proposals[0].text,'Мой ответ');
    await f.button('confirm').click();assert.equal(f.posts().length,1);connection.stop();
  }
});

test('lost editorial acknowledgement recovers same request/job and never submits review twice',async t=>{
  const f=fixture(t,{submit:async()=>{throw Error('Response lost');}});const connection=await f.make();f.review(connection);
  await f.button('confirm').click();assert.equal(f.posts().length,1);
  assert.equal(f.calls.find(call=>call.path.startsWith('/api/local-admissions/editorial/')).path.split('/').at(-1),f.pending().body.requestId);
  assert.equal(f.button('confirm').disabled,false);await f.button('confirm').click();assert.equal(f.posts().length,3);
});

test('unknown editorial admission survives reload and history recovers frozen refs without another POST',async t=>{
  let available=false;
  const f=fixture(t,{submit:async()=>{throw Error('Response lost');},lookup:async({body,receipt,ok})=>ok(available?
    {kind:'editorial',status:'committed',requestId:body.requestId,result:receipt(body)}:{status:'pending_or_unknown',result:null,retryAuthorized:false})});
  const first=await f.make();f.review(first);await f.button('confirm').click();const requestId=f.pending().body.requestId;
  await f.button('editorial-readback').click();assert.equal(f.posts().length,1);first.stop();
  const second=await f.make();second.openHistory();assert.match(f.dialog().html,new RegExp(requestId));
  await f.dialog().querySelectorAll('[data-editorial-review]')[0].click();available=true;await f.button('editorial-readback').click();
  assert.equal(f.posts().length,1);assert.equal(f.button('confirm').disabled,false);
});

test('edited proposal revision requires a fresh review and cannot use previous accepted receipt',async t=>{
  const f=fixture(t);const connection=await f.make();f.review(connection);await f.button('confirm').click();
  const original=f.pending().body.requestId;
  f.raw={...f.raw,proposals:[{...f.raw.proposals[0],revision:2,text:'Новая версия'}]};connection.hydrate(f.raw,{repaint:false});
  assert.equal(f.button('confirm').disabled,true);f.review(connection);assert.equal(f.button('confirm').textContent,'Проверить ответ');
  await f.button('confirm').click();assert.equal(f.posts().length,2);assert.notEqual(f.posts()[1].body.requestId,original);
  assert.deepEqual(f.posts()[1].body.proposals,[{id:'proposal-1',revision:2}]);assert.equal(f.posts().some(call=>call.path==='/api/approvals'),false);
});

test('misbound job and incomplete, duplicate, changed or invalid reused partitions block approval',async t=>{
  const cases=[job=>({...job,refId:'another-request'}),job=>({...job,kind:'assistant'}),job=>({...job,purpose:'reply'}),
    job=>({...job,id:'another-job'}),job=>({...job,result:{accepted:[],reused:[],held:[]}}),
    job=>({...job,result:{accepted:[...job.result.accepted,...job.result.accepted],reused:[],held:[]}}),
    job=>({...job,result:{accepted:[{id:'proposal-1',revision:2}],reused:[],held:[]}}),
    job=>({...job,result:{...job.result,reused:[{id:'other',revision:1}]}})];
  for(const corrupt of cases){
    const f=fixture(t,{job:async({completed,ok})=>ok(corrupt(completed()))});const connection=await f.make();f.review(connection);
    await f.button('confirm').click();assert.equal(f.button('confirm').disabled,true);assert.equal(f.posts().length,1);
    assert.ok(f.pending().body.requestId);connection.stop();
  }
});

test('actor switch during editorial admission cannot read or approve under the replacement actor',async t=>{
  let connection,f;f=fixture(t,{submit:async({body,receipt,ok})=>{
    f.raw={...f.raw,operator:{id:'operator-2'}};connection.hydrate(f.raw,{repaint:false});return ok(receipt(body));
  }});connection=await f.make();f.review(connection);await f.button('confirm').click();
  assert.equal(f.posts().length,1);assert.equal(f.calls.some(call=>call.path.startsWith('/api/jobs/')),false);
  assert.equal(f.saved.mvpPendingEditorial,undefined);
});

test('previously admitted reply executes without retrospective editorial review; uncertain execute cannot repeat',async t=>{
  const f=fixture(t);const connection=await f.make();
  const key=JSON.stringify(['proposal-1']);
  f.saved.mvpPendingApprovals={[key]:{actorId:'operator-1',account:'LikeAvto',rows:structuredClone(f.raw.proposals),
    body:{requestId:'already-admitted',proposals:[{id:'proposal-1',revision:1}]},
    receipt:{id:'approval-1',status:'approved',requestId:'already-admitted',accepted:[{id:'proposal-1',revision:1}],held:[]}}};
  f.review(connection);await f.button('confirm').click();
  assert.deepEqual(f.posts().map(call=>call.path),['/api/approvals/approval-1/execute']);
  f.saved.mvpPendingApprovals={[key]:{actorId:'operator-1',account:'LikeAvto',rows:structuredClone(f.raw.proposals),
    executeSubmitted:true,body:{requestId:'uncertain-execute',proposals:[{id:'proposal-1',revision:1}]},
    receipt:{id:'approval-1',status:'approved',requestId:'uncertain-execute',accepted:[{id:'proposal-1',revision:1}],held:[]}}};
  f.review(connection);assert.equal(f.button('confirm').disabled,true);await f.button('confirm').click();
  assert.equal(f.posts().length,1);
});

test('reused exact acceptance is usable but failed editorial job keeps draft and cannot restart blindly',async t=>{
  const f=fixture(t,{job:async({completed,ok})=>{const value=completed();return ok({...value,result:{...value.result,reused:value.result.accepted}});}});
  const connection=await f.make();f.review(connection);await f.button('confirm').click();assert.equal(f.button('confirm').disabled,false);
  connection.stop();
  const failed=fixture(t,{job:async({completed,ok})=>ok({...completed(),status:'failed',result:null})});
  const second=await failed.make();failed.review(second);await failed.button('confirm').click();
  assert.equal(failed.button('confirm').disabled,true);assert.match(failed.button('editorial-result').innerHTML,/Черновик сохранён/);
  await failed.button('editorial-readback').click();assert.equal(failed.posts().length,1);assert.equal(failed.saved.items['item-1'].draft,'Мой ответ');
});

test('reopening an overlapping group cannot start another model job for the same pending reply',async t=>{
  const f=fixture(t,{job:async({completed,ok})=>ok({...completed(),status:'running',result:null})});
  f.raw={...f.raw,items:[...f.raw.items,{...f.raw.items[0],id:'item-2',itemId:'comment-2'}],
    proposals:[...f.raw.proposals,{...f.raw.proposals[0],id:'proposal-2',itemId:'item-2'}]};
  const connection=await f.make();connection.review(['proposal-1','proposal-2']);await f.button('confirm').click();
  f.review(connection);await f.button('confirm').click();assert.equal(f.posts().length,1);
  assert.match(f.messages.at(-1),/уже проверяется в другой группе/);
  assert.equal(Object.keys(f.saved.mvpPendingEditorial).length,1);
});

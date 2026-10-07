import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

// DOM and HTTP seams only: no browser, database, service or social connector.
function fixture(t,{approve,lookup,execute}={}){
  const original={fetch:globalThis.fetch,document:globalThis.document};
  t.after(()=>Object.assign(globalThis,original));
  const items=[1,2,3].map(n=>({id:`item-${n}`,itemId:`comment-${n}`,revision:1,workflow:'prepared',providerStatus:'new',
    platform:'vk',draft:'',contextEvidenceDigest:'context',branchContextDigest:'branch',author:`Author ${n}`,text:`Comment ${n}`}));
  const proposals=items.map((item,index)=>({id:`proposal-${index+1}`,itemId:item.id,revision:1,itemRevision:1,
    kind:'close',text:'',status:'draft',contextEvidenceDigest:'context',branchContextDigest:'branch'}));
  let raw={operator:{id:'operator-1'},csrfToken:'csrf',account:'LikeAvto',items,posts:[],branches:[],proposals,operations:[]};
  const saved={items:{}},calls=[],dialogs=[],messages=[],connections=[];
  const control=()=>({disabled:false,hidden:false,checked:false,textContent:'',innerHTML:'',listeners:{},dataset:{},
    addEventListener(name,handler){this.listeners[name]=handler;},
    async click(){if(!this.disabled)await this.listeners.click?.({currentTarget:this});},
    async change(){await this.listeners.change?.({currentTarget:this});}});
  globalThis.document={activeElement:null,body:{append(){}},createElement(tag){
    if(tag==='textarea')return {set innerHTML(value){this.value=value;},value:''};
    const selectors=['[data-confirm]','[data-close]','[data-partial-approval]','[data-admission-readback]','[data-admission-retire]','[data-admission-result]','[data-media-readiness]','#mvp-history-order'];
    const controls=Object.fromEntries(selectors.map(selector=>[selector,control()]));
    return {controls,html:'',listeners:{},remove(){},showModal(){dialogs.push(this);},
      close(){this.listeners.close?.();},addEventListener(name,handler){this.listeners[name]=handler;},
      querySelector:selector=>controls[selector]||null,
      querySelectorAll(selector){
        if(selector!=='[data-approval-review]')return [];
        return this.historyButtons||=[...this.html.matchAll(/data-approval-review="([^"]+)"/g)].map(match=>{
          const button=control();button.dataset.approvalReview=match[1];return button;
        });
      },set innerHTML(value){this.html=value;}};
  }};
  const ok=value=>({ok:true,json:async()=>value});
  const result=(body,{accepted=body.proposals,held=[]}={})=>({id:accepted.length?'approval-1':null,status:accepted.length?'approved':'held',
    requestId:body.requestId,replayed:false,...(body.admissionMode==='partial'?{accepted,held}:{})});
  globalThis.fetch=async(path,options={})=>{
    const body=options.body?JSON.parse(options.body):undefined;
    calls.push({path,method:options.method||'GET',body});
    if(path==='/api/bootstrap')return ok(raw);
    if(path==='/api/approvals')return approve?approve({body,result,ok}):ok(result(body));
    if(path.startsWith('/api/local-admissions/approval/'))return lookup?lookup({path,ok}):ok({status:'pending_or_unknown',retryAuthorized:false,result:null});
    if(path==='/api/approvals/approval-1/execute')return execute?execute({ok}):ok({jobId:'execute-job-1'});
    throw Error(`Unexpected request ${path}`);
  };
  let data={items:[]};
  const make=async()=>{
    const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:item=>saved.items[item.id]||=structuredClone(item.initialState),
      persist(){},render(){},announce:message=>messages.push(message),esc:String,icon:()=>''});
    connections.push(connection);data=await connection.load();connection.hydrate(undefined,{repaint:false});return connection;
  };
  t.after(()=>connections.forEach(connection=>connection.stop()));
  const hold=(reference,reason='media_wait',message='Контекст видео ожидается')=>({reference,reason,message,httpStatus:409});
  const dialog=()=>dialogs.at(-1),button=name=>dialog().controls[`[data-${name}]`];
  return {saved,calls,dialogs,messages,make,result,hold,dialog,button,refs:proposals.map(({id,revision})=>({id,revision})),
    get raw(){return raw;},set raw(value){raw=value;},
    review(connection,ids=proposals.map(row=>row.id)){connection.review(ids);},
    async partial(){button('partial-approval').checked=true;await button('partial-approval').change();},
    posts:()=>calls.filter(call=>call.method==='POST')};
}

test('default remains one atomic approve then execute, with a persisted admission key',async t=>{
  const f=fixture(t);const connection=await f.make();f.review(connection);
  assert.equal(f.button('partial-approval').checked,false);
  await f.button('confirm').click();
  assert.deepEqual(f.posts().map(call=>call.path),['/api/approvals','/api/approvals/approval-1/execute']);
  assert.equal(f.posts()[0].body.admissionMode,undefined);
  assert.deepEqual(f.posts()[0].body.proposals,f.refs);assert.ok(f.posts()[0].body.requestId);
});

test('partial opt-in displays every held reference/reason and waits for confirmation of admitted subset',async t=>{
  let f;
  f=fixture(t,{approve:async({body,result,ok})=>ok(result(body,{accepted:[body.proposals[0]],held:[
    f.hold(body.proposals[1]),f.hold(body.proposals[2],'stale_revision','Проверьте версию')]}))});
  const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  assert.equal(f.posts().length,1);assert.equal(f.posts()[0].body.admissionMode,'partial');
  assert.deepEqual(f.posts()[0].body.proposals,f.refs);
  const report=f.button('admission-result').innerHTML;
  assert.match(report,/Принято: 1 из 3/);assert.match(report,/proposal-2 · версия 1: media_wait/);
  assert.match(report,/proposal-3 · версия 1: stale_revision/);
  assert.equal(f.button('confirm').textContent,'Выполнить принятые 1');
  await f.button('confirm').click();
  assert.equal(f.posts().at(-1).path,'/api/approvals/approval-1/execute');
});

test('all-held admission never executes and shows all reasons',async t=>{
  let f;f=fixture(t,{approve:async({body,result,ok})=>ok(result(body,{accepted:[],held:body.proposals.map(ref=>f.hold(ref))}))});
  const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  assert.equal(f.button('confirm').disabled,true);assert.match(f.button('admission-result').innerHTML,/Принято: 0 из 3/);
  for(const ref of f.refs)assert.ok(f.button('admission-result').innerHTML.includes(ref.id));
  await f.button('confirm').click();assert.equal(f.posts().length,1);
});

test('explicit retirement of all-held receipt opens current versions with a new admission key',async t=>{
  let f,originalReceipt;
  f=fixture(t,{approve:async({body,result,ok})=>{
    if(!originalReceipt){originalReceipt=result(body,{accepted:[],held:body.proposals.map(ref=>f.hold(ref))});return ok(originalReceipt);}
    return ok(result(body));
  }});
  const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  const oldRequest=f.posts()[0].body.requestId;
  f.raw={...f.raw,proposals:f.raw.proposals.map(row=>row.id==='proposal-1'?{...row,revision:2}:row)};
  connection.hydrate(f.raw,{repaint:false});f.review(connection);
  assert.equal(f.button('admission-retire').hidden,false);await f.button('admission-retire').click();
  assert.equal(Object.keys(f.saved.mvpPendingApprovals).length,0);
  await f.partial();await f.button('confirm').click();
  assert.equal(f.posts().length,2);assert.notEqual(f.posts()[1].body.requestId,oldRequest);
  assert.equal(f.posts()[1].body.proposals[0].revision,2);
  assert.equal(originalReceipt.held[0].reference.revision,1,'retirement never changes the original server receipt');
  assert.equal(originalReceipt.status,'held');
});

test('known admission with changed accepted ref can be explicitly retired; valid admission stays pinned',async t=>{
  const f=fixture(t);const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  assert.equal(f.button('admission-retire').hidden,true);
  await f.button('admission-retire').listeners.click();assert.equal(Object.keys(f.saved.mvpPendingApprovals).length,1);
  const oldRequest=f.posts()[0].body.requestId;
  f.raw={...f.raw,proposals:f.raw.proposals.map(row=>row.id==='proposal-1'?{...row,revision:2}:row)};
  connection.hydrate(f.raw,{repaint:false});assert.equal(f.button('admission-retire').hidden,false);
  await f.button('admission-retire').click();await f.partial();await f.button('confirm').click();
  assert.equal(f.posts().length,2);assert.notEqual(f.posts()[1].body.requestId,oldRequest);
  assert.equal(f.posts()[1].body.proposals[0].revision,2);
});

test('explicit partial admits frozen refs after one proposal changes or disappears; atomic stays blocked',async t=>{
  for(const change of ['revision','missing']){
    let f;f=fixture(t,{approve:async({body,result,ok})=>ok(result(body,{accepted:body.proposals.slice(1),
      held:[f.hold(body.proposals[0],'stale_revision','Предложение изменилось или недоступно')]}))});
    const connection=await f.make();f.review(connection);
    f.raw={...f.raw,proposals:change==='missing'?f.raw.proposals.slice(1):f.raw.proposals.map(row=>row.id==='proposal-1'?{...row,revision:2}:row)};
    connection.hydrate(f.raw,{repaint:false});
    assert.equal(f.button('confirm').disabled,true);assert.match(f.button('media-readiness').textContent,/Предложения изменились/);
    await f.button('confirm').click();assert.equal(f.posts().length,0);
    await f.partial();assert.equal(f.button('confirm').disabled,false);await f.button('confirm').click();
    assert.equal(f.posts().length,1);assert.deepEqual(f.posts()[0].body.proposals,f.refs,'original reviewed versions are never rebased');
    assert.match(f.button('admission-result').innerHTML,/proposal-1 · версия 1: stale_revision/);
    assert.equal(f.button('confirm').textContent,'Выполнить принятые 2');assert.equal(f.button('confirm').disabled,false);
    await f.button('confirm').click();assert.equal(f.posts().length,2);connection.stop();
  }
});

test('changed accepted proposal still blocks execution after partial approval',async t=>{
  const f=fixture(t);const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  f.raw={...f.raw,proposals:f.raw.proposals.map(row=>row.id==='proposal-1'?{...row,revision:2}:row)};
  connection.hydrate(f.raw,{repaint:false});assert.equal(f.button('confirm').disabled,true);
  await f.button('confirm').click();assert.equal(f.posts().length,1);
});

test('partial admission keeps held media outside execution; newly held accepted action blocks dispatch',async t=>{
  let f;f=fixture(t,{approve:async({body,result,ok})=>ok(result(body,{accepted:[body.proposals[0]],held:body.proposals.slice(1).map(ref=>f.hold(ref))}))});
  const connection=await f.make();f.review(connection);
  f.raw={...f.raw,items:f.raw.items.map(row=>row.id==='item-2'?{...row,mediaReadiness:{schemaVersion:2,required:true,status:'media_wait'}}:row)};
  connection.hydrate(f.raw,{repaint:false});assert.equal(f.button('confirm').disabled,true);
  await f.partial();assert.equal(f.button('confirm').disabled,false);await f.button('confirm').click();
  assert.equal(f.button('confirm').disabled,false,'held member does not block accepted subset');
  f.raw={...f.raw,items:f.raw.items.map(row=>row.id==='item-1'?{...row,mediaReadiness:{schemaVersion:2,required:true,status:'media_wait'}}:row)};
  connection.hydrate(f.raw,{repaint:false});assert.equal(f.button('confirm').disabled,true);
  await f.button('confirm').click();assert.equal(f.posts().length,1);
});

test('lost approval response recovers original receipt without another approval and waits for execute',async t=>{
  let receipt;
  const f=fixture(t,{approve:async({body,result})=>{receipt=result(body);throw Error('response lost');},
    lookup:async({ok})=>ok({kind:'approval',status:'committed',requestId:receipt.requestId,result:receipt})});
  const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  assert.equal(f.posts().length,1);assert.equal(f.calls.filter(call=>call.path.startsWith('/api/local-admissions/')).length,1);
  assert.equal(f.button('confirm').textContent,'Выполнить принятые 3');
  await f.button('confirm').click();assert.equal(f.posts().length,2);
});

test('unknown approval survives reload; website history exposes frozen readback without resubmitting',async t=>{
  let receipt,available=false;
  const f=fixture(t,{approve:async({body,result})=>{receipt=result(body);throw Error('response lost');},
    lookup:async({ok})=>ok(available?{kind:'approval',status:'committed',requestId:receipt.requestId,result:receipt}:
      {status:'pending_or_unknown',retryAuthorized:false,result:null})});
  const first=await f.make();f.review(first);await f.partial();await f.button('confirm').click();
  assert.equal(f.button('confirm').disabled,true);assert.equal(f.button('admission-readback').hidden,false);
  assert.equal(f.button('admission-retire').hidden,true);await f.button('admission-retire').listeners.click();
  assert.equal(Object.keys(f.saved.mvpPendingApprovals).length,1,'unknown admission cannot be retired');
  await f.button('admission-readback').click();assert.equal(f.posts().length,1);
  first.stop();available=true;f.raw={...f.raw,proposals:f.raw.proposals.map(row=>({...row,status:'approved'}))};
  const second=await f.make();second.openHistory();
  assert.match(f.dialog().html,/Одобрения, требующие проверки/);assert.match(f.dialog().html,new RegExp(receipt.requestId));
  // Reopen the exact reviewed rows, although they are no longer draft proposals.
  await f.dialog().querySelectorAll('[data-approval-review]')[0].click();await f.button('admission-readback').click();
  assert.equal(f.button('confirm').disabled,false);assert.equal(f.posts().length,1);
  await f.button('confirm').click();assert.equal(f.posts().length,2);
});

test('missing, duplicated or changed receipt references cannot dispatch',async t=>{
  for(const corrupt of [refs=>[refs[0]],refs=>[refs[0],refs[0],refs[2]],refs=>[...refs.slice(0,2),{id:refs[2].id,revision:2}]]){
    const f=fixture(t,{approve:async({body,result,ok})=>ok(result(body,{accepted:corrupt(body.proposals)}))});
    const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
    assert.equal(f.button('confirm').disabled,true);assert.equal(f.posts().length,1);connection.stop();
  }
});

test('actor switch while admission is pending cannot execute or read old admission as new actor',async t=>{
  let connection,f;
  f=fixture(t,{approve:async({body,result,ok})=>{
    f.raw={...f.raw,operator:{id:'operator-2'}};connection.hydrate(f.raw,{repaint:false});return ok(result(body));
  }});
  connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();
  assert.equal(f.posts().length,1);assert.equal(f.calls.filter(call=>call.path.startsWith('/api/local-admissions/')).length,0);
  assert.equal(f.saved.mvpPendingApprovals,undefined);
});

test('lost execute response stays recorded and cannot dispatch twice on reopen',async t=>{
  const f=fixture(t,{execute:async()=>{throw Error('execute response lost');}});
  const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();await f.button('confirm').click();
  assert.equal(f.posts().length,2);assert.equal(f.button('confirm').disabled,true);
  assert.equal(f.button('admission-retire').hidden,true);await f.button('admission-retire').listeners.click();
  assert.equal(Object.values(f.saved.mvpPendingApprovals)[0].executeSubmitted,true);
  f.review(connection);await f.button('confirm').click();
  assert.equal(f.posts().length,2);assert.match(f.button('admission-result').innerHTML,/Выполнение уже запрошено/);
});

test('malformed 2xx execution result retains dispatch protection and cannot report launch success',async t=>{
  for(const result of [{},{jobId:''},{jobId:'   '},{jobId:42},{jobId:'job\u0000bad'},{jobId:'job\ninvalid'}]){
    const f=fixture(t,{execute:async({ok})=>ok(result)});
    const connection=await f.make();f.review(connection);await f.partial();await f.button('confirm').click();await f.button('confirm').click();
    assert.equal(f.posts().length,2);assert.equal(Object.values(f.saved.mvpPendingApprovals)[0].executeSubmitted,true);
    assert.match(f.messages.at(-1),/Приём выполнения не подтверждён/);
    assert.ok(!f.messages.some(message=>message.includes('Выполнение запущено')));
    f.review(connection);await f.button('confirm').click();assert.equal(f.posts().length,2);
    assert.equal(f.button('admission-retire').hidden,true);await f.button('admission-retire').listeners.click();
    assert.equal(Object.keys(f.saved.mvpPendingApprovals).length,1);connection.stop();
  }
});

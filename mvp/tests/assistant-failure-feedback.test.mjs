import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection,discussionFailures,discussionOutstandingJob} from '../workshop/mvp-connection.js';
const escape=value=>String(value??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;');
const job={id:'failed-job',kind:'assistant',purpose:'discussion',operatorId:'local-owner',refId:'chat',sourceUserMessageId:'user-one',status:'failed',error:'Assistant request is no longer owned and active'};
const user={id:'user-one',role:'user',text:'Синтетический запрос <не html>'};
const raw=()=>({operator:{id:'local-owner'},items:[],posts:[],branches:[],materials:[],proposals:[],jobs:[job],conversations:[{id:'chat',messages:[user]}]});

test('reload with no local receipt shows durable failure and return-to-editor, not owner trace',async t=>{
  const old=globalThis.fetch;t.after(()=>globalThis.fetch=old);let posts=0;
  globalThis.fetch=async(_path,options={})=>{if(options.method==='POST')posts++;return {ok:true,json:async()=>raw()};};
  const saved={mvpConversationId:'chat'};
  for(let attempt=0;attempt<2;attempt++){
    const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
    await connection.load();const html=connection.aiHtml();
    assert.match(html,/Не удалось получить ответ ассистента/);assert.match(html,/Вернуть запрос в поле/);
    assert.doesNotMatch(html,/no longer owned|Ассистент печатает/);assert.match(html,/&lt;не html&gt;/);
    assert.equal(saved.mvpAssistantSubmission,undefined);connection.stop();
  }
  assert.equal(posts,0);
});

test('failure receipts require exact actor, conversation, source user and discussion kind',()=>{
  const snapshot=raw(),convo=snapshot.conversations[0];
  assert.equal(discussionFailures(snapshot,convo).length,1);
  for(const patch of [{operatorId:'other'},{operatorId:undefined},{refId:'other'},{sourceUserMessageId:'absent'},{sourceUserMessageId:undefined},{kind:'media'},{purpose:'auto_prepare'},{status:'unknown'},{status:'running'},{status:'completed'}]){
    assert.deepEqual(discussionFailures({...snapshot,jobs:[{...job,...patch}]},convo),[]);
  }
  assert.deepEqual(discussionFailures(snapshot,{...convo,messages:[user,{role:'assistant',prepareRunId:job.id,text:'Ответ'}]}),[]);
  assert.deepEqual(discussionFailures({...snapshot,jobs:[job,{...job,id:'new',status:'running'}]},convo),[]);
});

test('two exact failed requests remain separately attributable',()=>{
  const snapshot=raw(),second={...user,id:'user-two',text:'Другой запрос'};
  snapshot.conversations[0].messages.push(second);snapshot.jobs.push({...job,id:'second-job',sourceUserMessageId:second.id});
  assert.deepEqual(discussionFailures(snapshot,snapshot.conversations[0]).map(f=>[f.job.id,f.message.text]),[[job.id,user.text],['second-job',second.text]]);
});

test('durable discussion job restores progress after local receipt clears and remains actor-bound',async t=>{
  const old=globalThis.fetch;t.after(()=>globalThis.fetch=old);
  const active={...job,id:'running-job',status:'running',error:undefined};
  let server={...raw(),jobs:[active]};
  globalThis.fetch=async()=>({ok:true,json:async()=>server});
  const saved={mvpConversationId:'chat'};
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();
  assert.equal(discussionOutstandingJob(server,server.conversations[0])?.id,active.id);
  assert.match(connection.aiHtml(),/Ассистент печатает/);
  server={...server,jobs:[{...active,status:'queued'}]};connection.hydrate(server,{repaint:false});
  assert.match(connection.aiHtml(),/В очереди/);
  for(const patch of [{operatorId:'other'},{refId:'other'},{sourceUserMessageId:'missing'},{purpose:'auto_prepare'}]){
    const foreign={...server,jobs:[{...active,...patch}]};
    assert.equal(discussionOutstandingJob(foreign,foreign.conversations[0]),null);
  }
  connection.stop();
});

test('completed job without a message offers readback and disappears when its answer arrives',async t=>{
  const old=globalThis.fetch;t.after(()=>globalThis.fetch=old);
  const completed={...job,id:'completed-job',status:'completed',error:undefined};
  const server={...raw(),jobs:[completed]};
  globalThis.fetch=async()=>({ok:true,json:async()=>server});
  const connection=createMvpConnection({getSaved:()=>({mvpConversationId:'chat'}),currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();
  assert.match(connection.aiHtml(),/Задание завершено, но ответ пока не появился/);
  assert.match(connection.aiHtml(),/data-refresh-assistant/);
  server.conversations[0].messages.push({id:'reply',role:'assistant',prepareRunId:completed.id,text:'Ответ'});
  connection.hydrate(server,{repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Задание завершено, но ответ пока не появился/);
  connection.stop();
});

test('explicit restore stages exact text without a POST, preserves nonempty editor and rechecks failure',async t=>{
  const originals={fetch:globalThis.fetch,document:globalThis.document,requestAnimationFrame:globalThis.requestAnimationFrame};t.after(()=>Object.assign(globalThis,originals));
  let posts=0,click;const saved={mvpConversationId:'chat',mvpAiInput:''},input={value:'',addEventListener(){},focus(){}},submit={disabled:true};
  const chat={clientHeight:100,scrollHeight:100,scrollTop:0,isConnected:true,addEventListener(){}};
  const panel={querySelector:selector=>selector==='.ai-scroll'?chat:null,querySelectorAll:()=>[],removeEventListener(){},addEventListener(name,fn){if(name==='click')click=fn;}};
  const form={addEventListener(){}};
  const shell={querySelector:selector=>({'.ai':panel,'#ai-input':input,'.ai-form':form,'.ai-form .send-button':submit}[selector]||null)};
  globalThis.document={querySelector:selector=>selector==='#shell'?shell:selector==='#ai-input'?input:selector==='.ai-form .send-button'?submit:null};
  globalThis.requestAnimationFrame=()=>{};
  globalThis.fetch=async(_path,options={})=>{if(options.method==='POST')posts++;return {ok:true,json:async()=>raw()};};
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>'',persist(){}});
  await connection.load();connection.bindAi();const event={target:{closest:()=>({dataset:{restoreFailedRequest:job.id}})}};
  click(event);assert.equal(input.value,user.text);assert.equal(saved.mvpAiInput,user.text);assert.equal(submit.disabled,false);assert.equal(posts,0);
  input.value='Новая правка';saved.mvpAiInput='Новая правка';click(event);assert.equal(input.value,'Новая правка');assert.equal(saved.mvpAiInput,'Новая правка');
  input.value='';saved.mvpAiInput='';connection.hydrate({...raw(),jobs:[{...job,status:'running'}]},{repaint:false});
  click(event);assert.equal(input.value,'');assert.equal(posts,0);connection.stop();
});

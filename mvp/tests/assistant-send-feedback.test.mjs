import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

const response=(value,status=200)=>({ok:status<400,status,json:async()=>value});
const snapshot=()=>({operator:{id:'operator'},items:[],posts:[],branches:[],materials:[],proposals:[],jobs:[],
  conversations:[{id:'chat',messages:[]}]});
const escape=value=>String(value??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;');
const deferred=()=>{let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};};

test('click clears the unchanged editor and shows one local bubble before POST acknowledgment',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const post=deferred(),refreshGate=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Hello'},input={value:'Hello'};
  let bootCount=0,posts=0,server=snapshot();
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return ++bootCount===1?response(server):refreshGate.promise;
    if(path==='/api/conversations/chat/messages'){posts++;return post.promise;}
    throw new Error(`Unexpected ${path}`);
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();
  const pending=connection.sendAssistant('Hello');
  assert.equal(input.value,'');assert.equal(saved.mvpAiInput,'');assert.equal(posts,1);
  assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>Hello<\/p>/);
  assert.doesNotMatch(connection.aiHtml(),/Отправляем|Проверяем|Ассистент печатает/);
  assert.equal(connection.sendAssistant('Hello') instanceof Promise,true);
  assert.equal(posts,1,'a second click must not issue another POST while pending');
  saved.mvpAiInput='Next';input.value='Next';
  post.resolve(response({jobId:'job'}));
  const result=await pending;
  assert.equal(result.jobId,'job');assert.equal(saved.mvpAiInput,'Next');assert.equal(input.value,'Next');
  assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>Hello<\/p>/);
  assert.match(connection.aiHtml(),/Ассистент печатает/,'acknowledged job shows progress before the slow bootstrap');
  server={...server,jobs:[{id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'running'}],
    conversations:[{id:'chat',messages:[{id:'user-message',role:'user',text:'Hello'}]}]};
  refreshGate.resolve(response(server));await connection.refresh();
  const html=connection.aiHtml();
  assert.equal((html.match(/<strong>Вы<\/strong><p>Hello<\/p>/g)||[]).length,1);
  assert.match(html,/Ассистент печатает/);assert.equal(saved.mvpAiInput,'Next');
  connection.stop();
});

test('known POST rejection restores the original text and marks its bubble as failed',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const post=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Original'},input={value:'Original'};
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>path==='/api/bootstrap'?response(snapshot()):post.promise;
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();const pending=connection.sendAssistant('Original');
  assert.equal(input.value,'');post.resolve(response({error:'Дождитесь ответа'},409));
  await assert.rejects(pending,/Дождитесь ответа/);
  assert.equal(saved.mvpAiInput,'Original');assert.equal(input.value,'Original');
  assert.match(connection.aiHtml(),/Не принято: Дождитесь ответа/);
  connection.stop();
});

test('uncertain POST outcome keeps a newer draft and does not retry',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const post=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'First'},input={value:'First'};
  let posts=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return response(snapshot());
    if(path==='/api/conversations/chat/messages'){posts++;return post.promise;}
    throw new Error(`Unexpected ${path}`);
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();const pending=connection.sendAssistant('First');
  saved.mvpAiInput='Second';input.value='Second';post.reject(new TypeError('Network lost'));
  await assert.rejects(pending,/Network lost/);
  assert.equal(saved.mvpAiInput,'Second');assert.equal(input.value,'Second');assert.equal(posts,1);
  assert.match(connection.aiHtml(),/Исход не подтверждён/);
  connection.stop();
});

test('first-ever chat shows the bubble before conversation creation and keeps it through deferred POST',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const create=deferred(),post=deferred(),refreshGate=deferred(),saved={mvpAiInput:'First question'},input={value:'First question'};
  const first={...snapshot(),conversations:[]};let bootCount=0,renders=0,posts=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return ++bootCount===1?response(first):refreshGate.promise;
    if(path==='/api/conversations')return create.promise;
    if(path==='/api/conversations/new-chat/messages'){posts++;return post.promise;}
    throw new Error(`Unexpected ${path}`);
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),
    esc:escape,icon:()=>'',render:()=>renders++});
  await connection.load();const pending=connection.sendAssistant('First question');
  assert.equal(saved.mvpConversationId,undefined);assert.equal(input.value,'');assert.equal(renders,1,'missing progress slot must repaint immediately');
  assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>First question<\/p>/);
  create.resolve(response({id:'new-chat'}));
  await new Promise(setImmediate);
  assert.equal(saved.mvpConversationId,'new-chat');assert.equal(posts,1);
  assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>First question<\/p>/);
  post.resolve(response({jobId:'job'}));await pending;
  assert.equal(saved.mvpAiInput,'');assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>First question<\/p>/);
  assert.match(connection.aiHtml(),/Ассистент печатает/,'first conversation may not be in the old snapshot yet');
  refreshGate.resolve(response({...first,conversations:[{id:'new-chat',messages:[{id:'first',role:'user',text:'First question'}]}]}));
  await connection.refresh();
  assert.equal((connection.aiHtml().match(/<strong>Вы<\/strong><p>First question<\/p>/g)||[]).length,1);
  connection.stop();
});

test('optimistic insertion scrolls the chat to the new bubble without a full render',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document,oldFrame=globalThis.requestAnimationFrame;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;globalThis.requestAnimationFrame=oldFrame;});
  const post=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Bottom'},input={value:'Bottom',addEventListener:()=>{}};
  const slot={_html:'',set innerHTML(value){this._html=value;chat.scrollHeight=500;},get innerHTML(){return this._html;}};
  const chat={clientHeight:100,scrollHeight:300,scrollTop:0,isConnected:true,
    querySelector:selector=>selector==='[data-assistant-progress]'?slot:null,addEventListener:()=>{}};
  const panel={querySelector:selector=>selector==='.ai-scroll'?chat:null,querySelectorAll:()=>[],addEventListener:()=>{},removeEventListener:()=>{}};
  const form={addEventListener:()=>{}};
  const shell={querySelector:selector=>({'.ai':panel,'#ai-input':input,'.ai-form':form}[selector]||null)};
  globalThis.document={querySelector:selector=>selector==='#shell'?shell:selector==='#ai-input'?input:null};
  globalThis.requestAnimationFrame=()=>{};
  globalThis.fetch=async path=>path==='/api/bootstrap'?response(snapshot()):post.promise;
  let renders=0;
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),
    esc:escape,icon:()=>'',render:()=>renders++});
  await connection.load();connection.bindAi();chat.scrollTop=20;
  const pending=connection.sendAssistant('Bottom');
  assert.equal(chat.scrollTop,400);assert.match(slot.innerHTML,/<strong>Вы<\/strong><p>Bottom<\/p>/);
  assert.equal(renders,0,'progress slot should update without rebuilding the whole workspace');
  post.resolve(response({error:'busy'},409));await assert.rejects(pending,/busy/);
  connection.stop();
});

test('refresh failure after acknowledged POST leaves the accepted bubble and never retries POST',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const refreshGate=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Hello'},input={value:'Hello'},notices=[];
  let bootCount=0,posts=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return ++bootCount===1?response(snapshot()):refreshGate.promise;
    if(path==='/api/conversations/chat/messages'){posts++;return response({jobId:'job'});}
    throw new Error(`Unexpected ${path}`);
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),
    esc:escape,icon:()=>'',announce:message=>notices.push(message)});
  await connection.load();await connection.sendAssistant('Hello');
  refreshGate.reject(new Error('refresh offline'));
  await new Promise(setImmediate);
  assert.equal(saved.mvpAiInput,'');assert.equal(posts,1);
  assert.match(connection.aiHtml(),/<strong>Вы<\/strong><p>Hello<\/p>/);
  assert.doesNotMatch(connection.aiHtml(),/Не принято|Исход не подтверждён/);
  assert.ok(notices.some(message=>message.includes('Сообщение принято, но обновление задерживается')));
  connection.stop();
});

test('reload during an unacknowledged POST retains copyable text as unknown without replay or overwriting new typing',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const post=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'First'},input={value:'First'};
  let posts=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return response(snapshot());
    if(path==='/api/conversations/chat/messages'){posts++;return post.promise;}
    throw new Error(`Unexpected ${path}`);
  };
  const hooks={getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''};
  const first=createMvpConnection(hooks);await first.load();void first.sendAssistant('First');
  assert.equal(saved.mvpAssistantSubmission.phase,'pending');assert.equal(saved.mvpAssistantSubmission.text,'First');
  assert.equal(saved.mvpAiInput,'');saved.mvpAiInput='New draft';input.value='New draft';first.stop();
  const reloaded=createMvpConnection(hooks);await reloaded.load();
  assert.equal(saved.mvpAssistantSubmission.phase,'unknown');assert.equal(saved.mvpAiInput,'New draft');
  assert.match(reloaded.aiHtml(),/First/);assert.match(reloaded.aiHtml(),/Исход не подтверждён/);
  assert.equal(posts,1);reloaded.stop();
});

test('reload after acknowledged POST preserves accepted bubble while bootstrap has not caught up',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const saved={mvpConversationId:'chat',mvpAiInput:'Sent'},input={value:'Sent'},refreshGate=deferred();
  let boots=0,posts=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return ++boots===2?refreshGate.promise:response(snapshot());
    if(path==='/api/conversations/chat/messages'){posts++;return response({jobId:'job'});}
    throw new Error(`Unexpected ${path}`);
  };
  const hooks={getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''};
  const first=createMvpConnection(hooks);await first.load();await first.sendAssistant('Sent');first.stop();
  assert.equal(saved.mvpAssistantSubmission.phase,'accepted');
  const reloaded=createMvpConnection(hooks);await reloaded.load();
  assert.match(reloaded.aiHtml(),/<strong>Вы<\/strong><p>Sent<\/p>/);
  assert.doesNotMatch(reloaded.aiHtml(),/Не принято|Исход не подтверждён/);
  assert.equal(posts,1);refreshGate.resolve(response(snapshot()));reloaded.stop();
});

test('reloaded progress accepts only the current operator and conversation running assistant job',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'chat',mvpAiInput:'',mvpAssistantSubmission:{operatorId:'operator',conversationId:'chat',text:'Sent',
    phase:'accepted',matchingCount:0,jobId:'job'}};
  const base={...snapshot(),conversations:[{id:'chat',messages:[{id:'user',role:'user',text:'Sent'}]}]};
  let server={...base,jobs:[{id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'running',sourceUserMessageId:'user'}]};
  globalThis.fetch=async()=>response(server);
  const hooks={getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''};
  const reloaded=createMvpConnection(hooks);await reloaded.load();
  assert.equal(saved.mvpAssistantSubmission,undefined,'authoritative user message clears durable pending copy');
  assert.equal((reloaded.aiHtml().match(/<strong>Вы<\/strong><p>Sent<\/p>/g)||[]).length,1);
  assert.match(reloaded.aiHtml(),/Ассистент печатает/);reloaded.stop();
  for(const changed of [{operatorId:'other'},{refId:'other-chat'},{kind:'sync'},{status:'completed'}]){
    const scoped={...saved,mvpAssistantSubmission:{operatorId:'operator',conversationId:'chat',text:'Sent',phase:'accepted',matchingCount:0,jobId:'job'}};
    server={...base,jobs:[{id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'running',sourceUserMessageId:'user',...changed}]};
    const next=createMvpConnection({...hooks,getSaved:()=>scoped});await next.load();
    assert.doesNotMatch(next.aiHtml(),/Ассистент печатает/);next.stop();
  }
});

test('a pending submission from another operator is removed before it can appear',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'chat',mvpAssistantSubmission:{operatorId:'operator',conversationId:'chat',text:'Private draft',phase:'pending',matchingCount:0}};
  globalThis.fetch=async()=>response({...snapshot(),operator:{id:'other'}});
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();assert.equal(saved.mvpAssistantSubmission,undefined);
  assert.doesNotMatch(connection.aiHtml(),/Private draft/);connection.stop();
});

test('acknowledged progress never appears before POST or in another chat or actor',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const post=deferred(),refreshGate=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Hello'},input={value:'Hello'};
  let boots=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return ++boots===1?response(snapshot()):refreshGate.promise;
    if(path==='/api/conversations/chat/messages')return post.promise;
    throw new Error(`Unexpected ${path}`);
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();const pending=connection.sendAssistant('Hello');
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  post.resolve(response({jobId:'job'}));await pending;
  assert.match(connection.aiHtml(),/Ассистент печатает/);
  saved.mvpConversationId='another-chat';assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  saved.mvpConversationId='chat';
  connection.hydrate({...snapshot(),operator:{id:'other'}},{repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  refreshGate.resolve(response(snapshot()));connection.stop();
});

test('final assistant message or terminal readback suppresses provisional typing',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const refreshGate=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Hello'},input={value:'Hello'};
  let boots=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>path==='/api/bootstrap'
    ?++boots===1?response(snapshot()):refreshGate.promise
    :response({jobId:'job'});
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();await connection.sendAssistant('Hello');
  assert.match(connection.aiHtml(),/Ассистент печатает/);
  refreshGate.resolve(response(snapshot()));
  await connection.refresh();
  assert.match(connection.aiHtml(),/Ассистент печатает/,'a stale bootstrap without this job cannot retire acknowledged progress');
  await connection.refresh();
  assert.match(connection.aiHtml(),/Ассистент печатает/,'a second stale bootstrap still cannot retire it');
  const final={...snapshot(),jobs:[{id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'completed'}],
    conversations:[{id:'chat',messages:[{id:'user',role:'user',text:'Hello'},
      {id:'reply',role:'assistant',prepareRunId:'job',text:'Done'}]}]};
  connection.hydrate(final,{repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  connection.hydrate({...snapshot(),jobs:[{id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'failed',error:'Stopped'}]}, {repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  connection.hydrate(snapshot(),{repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/,'terminal job must not revive when later snapshot omits it');
  connection.stop();
});

test('a bound user readback without job stops provisional typing even after stored copy is cleared',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const refreshGate=deferred(),saved={mvpConversationId:'chat',mvpAiInput:'Hello'},input={value:'Hello'};
  let boots=0;
  globalThis.document={querySelector:selector=>selector==='#ai-input'?input:null};
  globalThis.fetch=async path=>path==='/api/bootstrap'
    ?++boots===1?response(snapshot()):refreshGate.promise
    :response({jobId:'job'});
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]}),esc:escape,icon:()=>''});
  await connection.load();await connection.sendAssistant('Hello');
  assert.match(connection.aiHtml(),/Ассистент печатает/);
  const userOnly={...snapshot(),conversations:[{id:'chat',messages:[{id:'user',role:'user',text:'Hello'}]}]};
  connection.hydrate(userOnly,{repaint:false});
  assert.equal(saved.mvpAssistantSubmission,undefined);
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/);
  connection.hydrate(snapshot(),{repaint:false});
  assert.doesNotMatch(connection.aiHtml(),/Ассистент печатает/,'later missing job must not revive the provisional state');
  refreshGate.resolve(response(snapshot()));connection.stop();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection,freezeAssistantContext,currentContextProposals,submitsAssistantMessage,normalizeMvpItem} from '../workshop/mvp-connection.js';

const item=id=>({id,revision:1,workflow:'prepared',contextEvidenceDigest:'c',branchContextDigest:'b'});
const proposal=id=>({id:`p-${id}`,itemId:id,status:'draft',kind:'reply_and_close',text:`Answer ${id}`,itemRevision:1,contextEvidenceDigest:'c',branchContextDigest:'b'});
const raw=()=>({items:['one','two','three'].map(item),proposals:['one','two','three'].map(proposal),conversations:[{id:'chat',messages:[{role:'user',text:'Earlier question'},{role:'assistant',text:'Earlier answer'}]}]});
const escape=value=>String(value??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('"','&quot;');

test('first operator login renders assistant with no personal conversation or active run',async t=>{
  const original=globalThis.fetch;t.after(()=>globalThis.fetch=original);
  globalThis.fetch=async()=>({ok:true,json:async()=>({...raw(),operator:{id:'dmitry'},conversations:[],jobs:[]})});
  for(const saved of [{},{mvpConversationId:'unavailable-old-chat'}]){
    const connection=createMvpConnection({esc:escape,icon:()=>'',getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]})});
    await connection.load();
    const html=connection.aiHtml();
    assert.match(html,/Что обсудим/);
    assert.doesNotMatch(html,/Earlier question|Задача в очереди|Работаю с комментариями/);
    connection.stop();
  }
});

test('assistant keeps history, follows current scope and has no global proposals or manual context controls',async t=>{
  const original=globalThis.fetch;t.after(()=>globalThis.fetch=original);
  globalThis.fetch=async()=>({ok:true,json:async()=>raw()});
  let context={itemId:'one',itemIds:['one'],key:'item:one',label:'Комментарий · Автор'};
  const saved={mvpConversationId:'chat'};
  const connection=createMvpConnection({esc:escape,icon:()=>'',getSaved:()=>saved,currentAssistantContext:()=>context});
  await connection.load();
  const html=connection.aiHtml();
  assert.match(html,/Earlier question/);assert.match(html,/Earlier answer/);
  assert.match(html,/data-context-item="one"/);assert.doesNotMatch(html,/Проверить решение|mvp-review|mvp-proposals/);
  assert.doesNotMatch(html,/mvp-conversation|attach-ai-context|mvp-new-conversation|mvp-proposal-edit|Answer two|Answer one/);
  assert.doesNotMatch(html,/assistant-find|assistant-new-chat|assistant-chat-select|Найти комментарий|Мои обсуждения/);
  context={itemIds:[],key:'overview',label:'Обзор'};
  assert.doesNotMatch(connection.aiHtml(),/id="mvp-review"/);
  context={itemIds:['one','two'],topicKey:'topic',key:'topic',label:'Тема'};
  assert.doesNotMatch(connection.aiHtml(),/Проверить решения|mvp-review|mvp-proposals/);
  connection.stop();
});

test('proposal preview scope excludes other comments, duplicates, stale and closed records',()=>{
  const snapshot=raw();snapshot.items[2].workflow='closed';
  snapshot.proposals.push({...proposal('one'),id:'new-one'},{...proposal('two'),id:'stale-two',itemRevision:0});
  const context=freezeAssistantContext({itemIds:['one','two','three','one']});
  assert.deepEqual(currentContextProposals(snapshot,context).map(row=>row.id),['p-two','new-one']);
  assert.deepEqual(context.itemIds,['one','two','three']);assert.ok(Object.isFrozen(context.itemIds));
});

test('request item IDs are frozen before conversation creation, navigation cannot retarget it',async t=>{
  const original=globalThis.fetch;t.after(()=>globalThis.fetch=original);
  const snapshot={...raw(),conversations:[]},requests=[];let release;
  const gate=new Promise(resolve=>release=resolve);
  globalThis.fetch=async(path,options)=>{
    const body=options.body?JSON.parse(options.body):null;requests.push({path,body});
    if(path==='/api/conversations'){await gate;return {ok:true,json:async()=>({id:'new-chat'})};}
    if(path==='/api/conversations/new-chat/messages'){
      snapshot.conversations=[{id:'new-chat',messages:[{role:'user',text:body.text}]}];
      return {ok:true,json:async()=>({id:'job'})};
    }
    assert.equal(path,'/api/bootstrap');return {ok:true,json:async()=>snapshot};
  };
  let context={itemIds:['one','two'],topicKey:'topic'};
  const saved={mvpAiInput:'Help'};
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>context});
  await connection.load();
  const pending=connection.sendAssistant('Help');context.itemIds.push('three');context={itemIds:['three']};
  saved.mvpAiInput='Next message';release();await pending;
  assert.deepEqual(requests.find(row=>row.path.endsWith('/messages')).body,{text:'Help',itemIds:['one','two']});
  assert.equal(saved.mvpAiInput,'Next message');assert.equal(saved.mvpConversationId,'new-chat');
  await connection.sendAssistant('Next message');
  assert.equal(requests.filter(row=>row.path==='/api/conversations').length,1);
  assert.deepEqual(requests.filter(row=>row.path.endsWith('/messages')).at(-1).body.itemIds,['three']);
  assert.equal(saved.mvpAiInput,'');connection.stop();
});

test('Enter submits; Shift+Enter and IME Enter leave input untouched',()=>{
  assert.equal(submitsAssistantMessage({key:'Enter'}),true);
  for(const event of [{key:'Enter',shiftKey:true},{key:'Enter',isComposing:true},{key:'Enter',keyCode:229},{key:'a'}])assert.equal(submitsAssistantMessage(event),false);
});

test('bound editor submits Enter once and respects an active composition session',t=>{
  const original=globalThis.document;t.after(()=>globalThis.document=original);
  const listeners={},input={value:'Question',addEventListener:(name,fn)=>listeners[name]=fn};
  let submitted=0,prevented=0;
  const form={addEventListener:()=>{},requestSubmit:()=>submitted++};
  const panel={querySelectorAll:()=>[],querySelector:()=>null};
  const shell={querySelector:selector=>({'.ai':panel,'#ai-input':input,'.ai-form':form}[selector]||null)};
  globalThis.document={querySelector:()=>shell};
  const connection=createMvpConnection({getSaved:()=>({}),currentAssistantContext:()=>({itemIds:[]})});
  connection.bindAi();
  const enter={key:'Enter',preventDefault:()=>prevented++};
  listeners.keydown(enter);assert.equal(submitted,1);assert.equal(prevented,1);
  listeners.keydown({...enter,shiftKey:true});assert.equal(submitted,1);
  listeners.compositionstart();listeners.keydown(enter);assert.equal(submitted,1);
  listeners.compositionend();listeners.keydown(enter);assert.equal(submitted,2);
  connection.stop();
});

test('normalization preserves source preview metadata and uses ordinary status wording',()=>{
  const original={...item('one'),autoPreparation:{status:'needs_attention',reason:'Проверить в Angry.Space'},sourcePreview:{title:'Пост',imageUrl:'https://example.test/image.jpg'}};
  const normalized=normalizeMvpItem(original);
  assert.deepEqual(normalized.sourcePreview,original.sourcePreview);
  assert.doesNotMatch(normalized.reason,/Angry.Space/);
  assert.doesNotMatch(normalized.initialState.note,/Angry.Space/);
});

test('assistant binding restores reading across context changes and independent refreshes',t=>{
  const originalDocument=globalThis.document,originalFrame=globalThis.requestAnimationFrame;
  t.after(()=>{globalThis.document=originalDocument;globalThis.requestAnimationFrame=originalFrame;});
  const frames=[];globalThis.requestAnimationFrame=fn=>frames.push(fn);
  const makeChat=()=>({scrollTop:0,clientHeight:400,scrollHeight:1600,isConnected:true,addEventListener(){}});
  let chat=makeChat();
  const panel={querySelectorAll:()=>[],querySelector:selector=>selector==='.ai-scroll'?chat:null};
  globalThis.document={querySelector:()=>({querySelector:selector=>selector==='.ai'?panel:null})};
  const saved={mvpConversationId:'persistent-chat'};
  let context={itemIds:['first']};
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>context});
  connection.bindAi();frames.splice(0).forEach(fn=>fn());
  assert.equal(chat.scrollTop,1200);
  chat.scrollTop=250;connection.rememberAssistantReading();chat.isConnected=false;
  context={itemIds:['second']};chat=makeChat();connection.bindAi();frames.splice(0).forEach(fn=>fn());
  assert.equal(chat.scrollTop,250);
  connection.rememberAssistantReading();chat.isConnected=false;
  chat=makeChat();chat.scrollHeight=2000;connection.bindAi();frames.splice(0).forEach(fn=>fn());
  assert.equal(chat.scrollTop,250);
  chat.scrollTop=1600;connection.rememberAssistantReading();chat.isConnected=false;
  chat=makeChat();chat.scrollHeight=2500;connection.bindAi();frames.splice(0).forEach(fn=>fn());
  assert.equal(chat.scrollTop,2100);
  connection.stop();
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

const item={id:'a',revision:2,workflow:'prepared',draft:'Old draft',contextEvidenceDigest:'c',branchContextDigest:'b'};
const candidate={id:'proposal',revision:1,itemId:'a',itemRevision:2,prepareRunId:'run',status:'draft',kind:'reply_and_close',text:'New candidate',contextEvidenceDigest:'c',branchContextDigest:'b'};
const state=()=>({draft:'Old draft',_serverDraft:'Old draft',_serverRevision:2,_serverDraftEdited:true,manualEdited:true,history:[],redo:[]});
const raw=()=>({items:[item],proposals:[candidate],conversations:[{id:'chat-a',messages:[{role:'assistant',prepareRunId:'run',text:'Candidate'}]},{id:'chat-b',messages:[]}]});
const response=value=>({ok:true,json:async()=>value});

test('candidate card shows original recipient and comment before an explicit replacement',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  globalThis.document={createElement:()=>({set innerHTML(value){this.value=value;}})};
  const snapshot=raw();snapshot.items[0]={...item,branchId:'branch',targetId:'message',text:'Original & question'};
  snapshot.branches=[{id:'branch',postId:'post',messages:[{id:'message',author:'Actual author'}]}];snapshot.posts=[{id:'post',title:'Actual post'}];
  globalThis.fetch=async()=>response(snapshot);
  const esc=value=>String(value??'').replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;');
  const connection=createMvpConnection({esc,icon:()=>'',getSaved:()=>({mvpConversationId:'chat-a'}),currentAssistantContext:()=>({itemIds:['a']})});
  await connection.load();const html=connection.aiHtml();
  assert.match(html,/Actual author/);assert.match(html,/Original &amp; question/);assert.match(html,/Actual post/);assert.match(html,/href="#item\/a"/);connection.stop();
});

test('switching chat while selected draft saves must not redirect the submitted message',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'chat-a',mvpAiInput:'Rewrite'},local=state();local.draft='Unsaved edit';
  let release;const gate=new Promise(resolve=>release=resolve),requests=[];
  globalThis.fetch=async(path,options={})=>{
    requests.push({path,body:options.body?JSON.parse(options.body):null});
    if(path==='/api/items/a'){await gate;return response({...item,draft:'Unsaved edit',revision:3});}
    if(path.endsWith('/messages'))return response({jobId:'job'});
    return response(raw());
  };
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>({items:[item]}),stateFor:()=>local,currentAssistantContext:()=>({itemId:'a',itemIds:['a']})});
  await connection.load();const pending=connection.sendAssistant('Rewrite');
  saved.mvpConversationId='chat-b';release();await pending;
  assert.equal(requests.find(r=>r.path.endsWith('/messages')).path,'/api/conversations/chat-a/messages');
  assert.equal(saved.mvpConversationId,'chat-b');connection.stop();
});

test('conversation creation cannot steal a chat selected while creation was pending',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'',mvpAiInput:'First question'};let release;
  const gate=new Promise(resolve=>release=resolve),requests=[];
  globalThis.fetch=async(path,options={})=>{
    requests.push(path);
    if(path==='/api/conversations'){await gate;return response({id:'created-chat'});}
    if(path.endsWith('/messages'))return response({jobId:'job'});
    return response(raw());
  };
  const connection=createMvpConnection({getSaved:()=>saved,currentAssistantContext:()=>({itemIds:[]})});
  await connection.load();const pending=connection.sendAssistant('First question');
  saved.mvpConversationId='chat-b';saved.mvpAiInput='Question for chat B';release();await pending;
  assert.ok(requests.includes('/api/conversations/created-chat/messages'));
  assert.equal(saved.mvpConversationId,'chat-b');assert.equal(saved.mvpAiInput,'Question for chat B');connection.stop();
});

test('natural fresh-discussion request creates a private conversation without erasing drafts or old chat',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'chat-a'},local=state(),snapshot=raw(),requests=[];
  globalThis.fetch=async(path,options={})=>{
    requests.push(path);
    if(path==='/api/conversations')return response({id:'fresh-chat'});
    return response(path.endsWith('/messages')?{jobId:'job'}:snapshot);
  };
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>({items:[item]}),stateFor:()=>local,currentAssistantContext:()=>({itemId:'a',itemIds:['a']})});
  await connection.load();await connection.sendAssistant('Начни новое обсуждение: обсудим этот комментарий');
  assert.equal(saved.mvpConversationId,'fresh-chat');assert.ok(requests.includes('/api/conversations/fresh-chat/messages'));
  assert.equal(local.draft,'Old draft');assert.equal(snapshot.conversations[0].id,'chat-a');connection.stop();
});

test('visible derived draft is attached with its exact proposal without being autosaved as a manual edit',async t=>{
  const oldFetch=globalThis.fetch;t.after(()=>globalThis.fetch=oldFetch);
  const saved={mvpConversationId:'chat-a'},local={...state(),draft:candidate.text,_serverDraft:'',_derivedDraft:candidate.text,manualEdited:false,_sourceProposalId:candidate.id,_sourceProposalRevision:1};
  const requests=[];
  globalThis.fetch=async(path,options={})=>{requests.push({path,body:options.body?JSON.parse(options.body):null});return response(path.endsWith('/messages')?{jobId:'job'}:raw());};
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>({items:[item]}),stateFor:()=>local,currentAssistantContext:()=>({itemId:'a',itemIds:['a']})});
  await connection.load();await connection.sendAssistant('Сделай короче');
  assert.equal(requests.some(r=>r.path==='/api/items/a'),false);
  assert.deepEqual(requests.find(r=>r.path.endsWith('/messages')).body.displayedDraft,{itemId:'a',text:candidate.text,proposalId:'proposal',proposalRevision:1});connection.stop();
});

test('typing during explicit candidate save must survive the response',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const local=state(),saved={mvpConversationId:'chat-a'},snapshot=raw();
  let apply;
  const button={dataset:{applyCandidate:'proposal'},addEventListener:(event,fn)=>{apply=fn;}};
  const panel={querySelector:()=>null,querySelectorAll:selector=>selector==='[data-apply-candidate]'?[button]:[]};
  const shell={querySelector:selector=>selector==='.ai'?panel:null};
  globalThis.document={querySelector:()=>shell};
  const patches=[];
  globalThis.fetch=async(path,options={})=>{
    if(path==='/api/items/a'){
      const body=JSON.parse(options.body);patches.push(body);
      if(patches.length===1){local.draft='Typed while saving candidate';local.revision=1;}
      snapshot.items=[{...item,draft:body.draft,draftEdited:true,revision:2+patches.length}];
      return response(snapshot.items[0]);
    }
    return response(snapshot);
  };
  const data={items:[item]};
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>data,stateFor:()=>local,currentAssistantContext:()=>({itemId:'a',itemIds:['a']})});
  await connection.load();connection.bindAi();await apply();
  assert.equal(local.draft,'Typed while saving candidate');
  await connection.saveDraft(item);
  assert.equal(patches.length,2);assert.equal(patches[1].expectedRevision,3);assert.equal(patches[1].draft,'Typed while saving candidate');
  connection.stop();
});

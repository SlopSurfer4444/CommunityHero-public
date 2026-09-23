import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection,normalizeMvpItem} from '../workshop/mvp-connection.js';

test('comment updates reuse unchanged post transcripts and branch projection',async t=>{
  const priorFetch=globalThis.fetch;t.after(()=>globalThis.fetch=priorFetch);
  const priorDocument=globalThis.document;t.after(()=>globalThis.document=priorDocument);
  globalThis.document={createElement:()=>({value:'',set innerHTML(value){this.value=value;}})};
  const post={id:'post',postKey:'post',title:'Video',attachments:[]};
  const branch={id:'branch',postId:'post',messages:[{id:'message',author:'Author',text:'Hello'}]};
  const first={operator:{id:'operator'},workspaceVersion:'v1',items:[],posts:[post],branches:[branch],materials:[],proposals:[],conversations:[],jobs:[]};
  globalThis.fetch=async()=>({ok:true,json:async()=>first});
  const saved={items:{},branches:{}},state=new Map();let data;
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>data,stateFor:item=>{
    if(!state.has(item.id))state.set(item.id,{...item.initialState});
    return state.get(item.id);
  }});
  data=await connection.load();
  const posts=data.posts,branches=data.branches;
  const comment={id:'comment',branchId:'branch',postId:'post',workflow:'attention',revision:1};
  connection.hydrate({...first,workspaceVersion:'v2',items:[comment]},{repaint:false});
  assert.equal(data.posts,posts);
  assert.equal(data.branches,branches);
  assert.equal(data.items[0].id,'comment');
  const materials=[{id:'transcript',kind:'transcript',postKey:'post',text:'Spoken text'}];
  connection.hydrate({...first,workspaceVersion:'v3',items:[comment],materials},{repaint:false});
  assert.notEqual(data.posts,posts);
  assert.equal(data.branches,branches);
  assert.equal(data.posts[0].transcripts[0].id,'transcript');
  const newerBranches=[{...branch,messages:[...branch.messages,{id:'second',text:'New'}]}];
  const latestPosts=data.posts;
  connection.hydrate({...first,workspaceVersion:'v4',items:[comment],materials,branches:newerBranches},{repaint:false});
  assert.equal(data.posts,latestPosts);
  assert.notEqual(data.branches,branches);
  connection.stop();
});

test('proposal grouping preserves newest valid candidate for each comment',()=>{
  const item={id:'comment',workflow:'prepared',revision:4,contextEvidenceDigest:'c',branchContextDigest:'b'};
  const candidate=(id,revision)=>({id,itemId:'comment',kind:'reply_and_close',text:id,status:'draft',itemRevision:revision,contextEvidenceDigest:'c',branchContextDigest:'b'});
  const all=[candidate('old',4),{...candidate('other',4),itemId:'elsewhere'},candidate('stale',3),candidate('new',4)];
  const grouped=all.filter(row=>row.itemId===item.id);
  assert.deepEqual(normalizeMvpItem(item,grouped).initialState,normalizeMvpItem(item,all).initialState);
  assert.equal(normalizeMvpItem(item,grouped).initialState._displayedProposalId,'new');
});

test('a tool receipt updates assistant progress without rebuilding the active editor',async t=>{
  const priorFetch=globalThis.fetch,priorDocument=globalThis.document,priorFrame=globalThis.requestAnimationFrame;
  t.after(()=>{globalThis.fetch=priorFetch;globalThis.document=priorDocument;globalThis.requestAnimationFrame=priorFrame;});
  const saved={mvpConversationId:'chat',mvpAiInput:''},job={id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'running'};
  let snapshot={operator:{id:'operator'},items:[],posts:[],branches:[],materials:[],proposals:[],jobs:[],conversations:[{id:'chat',messages:[]}]};
  globalThis.fetch=async(path)=>{
    if(path==='/api/conversations/chat/messages'){
      snapshot={...snapshot,jobs:[job]};
      return {ok:true,json:async()=>({jobId:'job'})};
    }
    return {ok:true,json:async()=>snapshot};
  };
  const slot={innerHTML:''},chat={clientHeight:100,scrollHeight:200,scrollTop:100,isConnected:true,
    querySelector:selector=>selector==='[data-assistant-progress]'?slot:null,addEventListener:()=>{}};
  const panel={querySelector:selector=>selector==='.ai-scroll'?chat:null,querySelectorAll:()=>[],addEventListener:()=>{},removeEventListener:()=>{}};
  const input={id:'ai-input',value:'still typing',selectionStart:4,selectionEnd:4,addEventListener:()=>{},focus:()=>{},setSelectionRange:()=>{}};
  const form={addEventListener:()=>{}},shell={querySelector:selector=>({'.ai':panel,'#ai-input':input,'.ai-form':form}[selector]||null)};
  globalThis.document={visibilityState:'visible',activeElement:input,querySelector:selector=>selector==='#shell'?shell:null,
    createElement:()=>({value:'',set innerHTML(value){this.value=value;}})};
  globalThis.requestAnimationFrame=callback=>callback();
  let data,renders=0;
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>data,currentAssistantContext:()=>({itemIds:[],key:'overview'}),
    assistantNavigationRevision:()=>0,assistantScreenKey:()=>'',render:()=>renders++,esc:value=>String(value??''),icon:()=>''});
  data=await connection.load();
  connection.hydrate(snapshot,{repaint:false});
  await connection.sendAssistant('Check workspace');
  connection.bindAi();
  await connection.refresh();
  const before=renders;
  snapshot={...snapshot,jobs:[{...job,toolResults:[{id:'tool',name:'workspace_stats',ok:true,result:{total:7}}]}]};
  await connection.refresh();
  assert.equal(renders,before);
  assert.equal(input.value,'still typing');
  assert.match(slot.innerHTML,/В рабочем месте: 7/);
  assert.equal(chat.scrollTop,100);
  connection.stop();
});

test('completed navigation does not request a second full render',async t=>{
  const priorFetch=globalThis.fetch,priorDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=priorFetch;globalThis.document=priorDocument;});
  const saved={mvpConversationId:'chat'},job={id:'job',kind:'assistant',refId:'chat',operatorId:'operator',status:'running'};
  let snapshot={operator:{id:'operator'},items:[],posts:[],branches:[],materials:[],proposals:[],jobs:[],conversations:[{id:'chat',messages:[]}]};
  globalThis.fetch=async path=>{
    if(path==='/api/conversations/chat/messages'){snapshot={...snapshot,jobs:[job]};return {ok:true,json:async()=>({jobId:'job'})};}
    return {ok:true,json:async()=>snapshot};
  };
  globalThis.document={activeElement:null};
  let data,renders=0,navigations=0;
  const connection=createMvpConnection({getSaved:()=>saved,getData:()=>data,currentAssistantContext:()=>({itemIds:[]}),
    assistantNavigationRevision:()=>0,assistantScreenKey:()=>'',navigateAssistant:()=>{navigations++;return true;},render:()=>renders++});
  data=await connection.load();connection.hydrate(snapshot,{repaint:false});
  await connection.sendAssistant('Open prepared');await connection.refresh();
  const before=renders;
  snapshot={...snapshot,jobs:[{...job,status:'completed'}],conversations:[{id:'chat',messages:[{role:'assistant',prepareRunId:'job',navigation:{kind:'queue',workflow:'prepared'}}]}]};
  await connection.refresh();
  assert.equal(navigations,1);
  assert.equal(renders,before);
  await connection.refresh();
  assert.equal(navigations,1);
  connection.stop();
});

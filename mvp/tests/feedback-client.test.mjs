import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection,normalizeMvpItem,mergeMvpItemState} from '../workshop/mvp-connection.js';

const base={id:'a',revision:2,workflow:'prepared',draft:'',contextEvidenceDigest:'c',branchContextDigest:'b'};
const ai={id:'ai-a',revision:1,itemId:'a',itemRevision:2,status:'draft',kind:'reply_and_close',text:'AI baseline',contextEvidenceDigest:'c',branchContextDigest:'b'};
const settle=async()=>{for(let i=0;i<15;i++)await Promise.resolve();};
function setup(t){
  let server={...base},selected='a',fail=false;
  const calls=[],saved={items:{}},data={items:[]};
  const editor={value:ai.text,getClientRects:()=>[{}]};
  const old={fetch:globalThis.fetch,document:globalThis.document};
  globalThis.document={visibilityState:'visible',querySelector:selector=>selector==='#draft'?editor:null};
  const snapshot=()=>({csrfToken:'test',items:[server,{...base,id:'b'}],proposals:[ai]});
  globalThis.fetch=async(path,options)=>{
    const body=options.body&&JSON.parse(options.body);calls.push({path,body,method:options.method});
    if(fail&&path==='/api/feedback/events')throw new Error('offline');
    if(fail==='draft'&&options.method==='PATCH')throw new Error('draft offline');
    if(options.method==='PATCH')server={...server,draft:body.draft,draftEdited:true,revision:server.revision+1,draftOrigin:{...body}};
    return {ok:true,json:async()=>options.method==='PATCH'?server:snapshot()};
  };
  const hooks={getData:()=>data,getSaved:()=>saved,selectedItem:()=>data.items.find(i=>i.id===selected),stateFor:item=>saved.items[item.id]??=structuredClone(item.initialState)};
  const connection=createMvpConnection(hooks);connection.hydrate(snapshot(),{repaint:false});
  t.after(()=>{connection.stop();Object.assign(globalThis,old);});
  return {connection,calls,saved,data,editor,hooks,snapshot,select:id=>selected=id,fail:value=>fail=value};
}
test('first full clear persists lineage; refresh and fresh normalization never resurrect AI text',async t=>{
  const {connection,data,saved,calls}=setup(t),state=saved.items.a;
  state.draft='';state.manualEdited=true;
  await connection.saveDraft(data.items[0]);
  const patch=calls.find(c=>c.method==='PATCH').body;
  assert.equal(patch.draft,'');assert.equal(patch.sourceProposalId,ai.id);assert.equal(patch.sourceProposalRevision,1);
  assert.ok(patch.draftSessionId);assert.ok(patch.eventId);
  assert.equal(data.items[0].draft,'');
  assert.equal(normalizeMvpItem({...base,draftEdited:true},[ai]).draft,'');
  await connection.saveDraft(data.items[0]);
  assert.equal(calls.filter(c=>c.method==='PATCH').length,1);
});
test('undo and redo stay one source session, switching item cannot retarget save',async t=>{
  const {connection,data,saved,calls,select}=setup(t),item=data.items[0],state=saved.items.a;
  for(const value of ['Edited',ai.text,'']){
    state.draft=value;state.manualEdited=true;select('b');await connection.saveDraft(item);
  }
  const writes=calls.filter(c=>c.method==='PATCH');
  assert.equal(writes.length,3);assert.ok(writes.every(c=>c.path==='/api/items/a'));
  assert.equal(new Set(writes.map(c=>c.body.draftSessionId)).size,1);
  assert.ok(writes.every(c=>c.body.sourceProposalId===ai.id));
});
test('only visible selected AI composer is presented; offline retries retain event identity across connection restart',async t=>{
  const ctx=setup(t),{connection,saved,calls,editor}=ctx;
  globalThis.document.visibilityState='hidden';connection.trackPresented();assert.equal(saved.mvpFeedbackOutbox,undefined);
  globalThis.document.visibilityState='visible';editor.value='manual';connection.trackPresented();assert.equal(saved.mvpFeedbackOutbox,undefined);
  editor.value=ai.text;ctx.fail(true);connection.trackPresented();await settle();
  const id=saved.mvpFeedbackOutbox[0].eventId;
  connection.trackPresented();await settle();assert.equal(saved.mvpFeedbackOutbox.length,1);
  connection.stop();const restarted=createMvpConnection(ctx.hooks);t.after(()=>restarted.stop());
  ctx.fail(false);restarted.hydrate(ctx.snapshot(),{repaint:false});await settle();assert.equal(saved.mvpFeedbackOutbox.length,0);
  restarted.trackPresented();await settle();
  assert.ok(calls.filter(c=>c.path==='/api/feedback/events').every(c=>c.body.eventId===id));
  assert.equal(Object.keys(saved.mvpPresented).length,1);
});
test('failed draft request reuses exact event and revision after connection restart',async t=>{
  const ctx=setup(t),{connection,data,saved,calls}=ctx;
  saved.items.a.draft='edited';saved.items.a.manualEdited=true;ctx.fail('draft');
  await assert.rejects(connection.saveDraft(data.items[0]),/offline/);
  const original=calls.find(c=>c.method==='PATCH').body;
  connection.stop();ctx.fail(false);
  const restarted=createMvpConnection(ctx.hooks);t.after(()=>restarted.stop());
  await restarted.saveDraft(data.items[0]);
  assert.deepEqual(calls.filter(c=>c.method==='PATCH')[1].body,original);
  assert.equal(saved.items.a._pendingDraftSave,undefined);
});
test('new untouched generation resets source; local edit retains original generation',()=>{
  const old=normalizeMvpItem(base,[ai]),next=normalizeMvpItem(base,[{...ai,id:'new',revision:2}]);
  const edited={...old.initialState,draft:'edit',manualEdited:true};mergeMvpItemState(edited,next);
  assert.equal(edited._sourceProposalId,ai.id);
  const untouched={...old.initialState};mergeMvpItemState(untouched,next);assert.equal(untouched._sourceProposalId,'new');
});
test('preparing and cancelling a manual descendant retains AI lineage after refresh',()=>{
  const initial=normalizeMvpItem(base,[ai]);
  const state={...initial.initialState,_draftSessionId:'original-session'};
  const descendant={...ai,id:'manual-review',revision:3,text:'Displayed review text',origin:{...ai,prepareRunId:'run-ai'}};
  const refreshed=normalizeMvpItem(base,[ai,descendant]);
  mergeMvpItemState(state,refreshed);
  assert.equal(state.draft,'Displayed review text');
  assert.equal(state._derivedDraft,'Displayed review text');
  assert.equal(state._sourceProposalId,ai.id);
  assert.equal(state._sourceProposalRevision,ai.revision);
  assert.equal(state._draftSessionId,'original-session');
  state.draft='Edited after cancelled review';state.manualEdited=true;
  mergeMvpItemState(state,refreshed);
  assert.equal(state._sourceProposalId,ai.id);
  assert.equal(state.draft,'Edited after cancelled review');
});

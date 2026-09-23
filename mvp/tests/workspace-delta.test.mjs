import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {canRequestWorkspaceDelta,mergeWorkspaceDelta,createWorkspaceChangeTracker} from '../workshop/workspace-delta.js';
import {createMvpConnection} from '../workshop/mvp-connection.js';

const snapshot=()=>({workspaceVersion:'v1',operator:{id:'owner'},csrfToken:'token',account:'LikeAvto',
  items:[{id:'a',revision:1,workflow:'attention',draft:''},{id:'b',revision:1,workflow:'attention',draft:''}],
  posts:[],branches:[],proposals:[],operations:[],materials:[],conversations:[{id:'private-old',messages:[]}],jobs:[],sync:{status:'idle'}});
const delta=(changes={})=>({kind:'delta',baseVersion:'v1',workspaceVersion:'v2',actorId:'owner',collections:{},set:{},remove:[],...changes});
const freeze=value=>{if(value&&typeof value==='object'){Object.freeze(value);Object.values(value).forEach(freeze);}return value;};

test('browser reconstructs the exact Rust producer shared contract',()=>{
  const fixture=JSON.parse(readFileSync(new URL('./fixtures/workspace-delta-contract.json',import.meta.url),'utf8'));
  assert.deepEqual(mergeWorkspaceDelta(freeze(fixture.base),freeze(fixture.delta)),fixture.current);
});

test('keyed changes preserve server order and do not mutate the base or transport response',()=>{
  const before=freeze(snapshot()),response=freeze(delta({collections:{items:{upsert:[{id:'a',revision:2},{id:'c',revision:1}],remove:['b'],order:['c','a']},
    conversations:{upsert:[],remove:['private-old'],order:[]}},set:{sync:{status:'complete'}},remove:['csrfToken']}));
  const merged=mergeWorkspaceDelta(before,response);
  assert.deepEqual(merged.items,[{id:'c',revision:1},{id:'a',revision:2}]);
  assert.deepEqual(merged.conversations,[]);assert.equal(merged.sync.status,'complete');assert.equal(merged.workspaceVersion,'v2');
  assert.equal('csrfToken' in merged,false);assert.equal(before.csrfToken,'token');assert.equal(before.items[0].revision,1);
  assert.equal(merged.posts,before.posts);
});

test('replacement rows and order-only changes are supported; full resets remove old private data',()=>{
  const before=snapshot();
  const replaced=mergeWorkspaceDelta(before,delta({collections:{items:{upsert:[{id:'a',revision:2}],remove:[]}}}));
  assert.deepEqual(replaced.items.map(row=>row.id),['a','b']);assert.equal(replaced.items[0].draft,undefined);
  assert.deepEqual(mergeWorkspaceDelta(before,delta({collections:{items:{upsert:[],remove:[],order:['b','a']}}})).items.map(row=>row.id),['b','a']);
  const next={...snapshot(),workspaceVersion:'reset',operator:{id:'other'},conversations:[]};
  assert.equal(mergeWorkspaceDelta(before,{kind:'full',snapshot:next}),next);
  assert.equal(mergeWorkspaceDelta(before,{kind:'full',snapshot:next}).conversations.length,0);
});

test('malformed, actor-crossing and stale deltas fail atomically',()=>{
  const before=freeze(snapshot());
  const malformed=[
    delta({baseVersion:'old'}),delta({actorId:'other'}),delta({workspaceVersion:null}),
    delta({collections:{items:{upsert:[{id:'a'},{id:'a'}],remove:[]}}}),
    delta({collections:{items:{upsert:[{id:'c'}],remove:[]}}}),
    delta({collections:{items:{upsert:[],remove:['a']}}}),
    delta({collections:{items:{upsert:[],remove:[],order:['a','a']}}}),
    delta({collections:{items:{upsert:[],remove:[],order:['a','unknown']}}}),
    delta({collections:{items:{upsert:[{id:'a'}],remove:['a'],order:['b']}}}),
    delta({collections:{unknown:{upsert:[],remove:[]}}}),delta({set:{items:[]}}),
    delta({set:{operator:{id:'other'}}}),delta({remove:['operator']}),delta({set:{account:'x'},remove:['account']}),
    delta({set:JSON.parse('{"__proto__":{"polluted":true}}')}),{kind:'full',snapshot:{items:[]}},
    delta({collections:{items:{upsert:[{id:'a',revision:99}],remove:[]},jobs:{upsert:[],remove:['missing']}}}),
  ];
  for(const response of malformed)assert.throws(()=>mergeWorkspaceDelta(before,response),/Invalid workspace delta/);
  assert.equal(before.items[0].revision,1);assert.equal({}.polluted,undefined);
  assert.equal(canRequestWorkspaceDelta({workspaceVersion:'v1'}),false);
});

function setup(t,extraHooks={}){
  const original={fetch:globalThis.fetch,document:globalThis.document};
  globalThis.document={activeElement:null,visibilityState:'visible',querySelector:()=>null,
    createElement:()=>({innerHTML:'',get value(){return this.innerHTML;}})};
  const state={paths:[],full:snapshot(),response:null,status:200,renders:0,merges:0,version:'v1',wait:null};
  globalThis.fetch=async path=>{
    state.paths.push(path);
    if(path.startsWith('/api/bootstrap/delta')){
      if(state.wait)await state.wait;
      return {ok:state.status===200,status:state.status,json:async()=>structuredClone(state.response)};
    }
    return {ok:true,json:async()=>structuredClone(path==='/api/workspace-version'?{workspaceVersion:state.version,...(state.versionActor?{actorId:state.versionActor}:{}),...(state.versionCsrf?{csrfToken:state.versionCsrf}:{})}:state.full)};
  };
  const saved={items:{},selected:'a',mvpConversationId:'private-old'},data={items:[]};
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:item=>{state.merges++;return saved.items[item.id]??=structuredClone(item.initialState);},render:()=>state.renders++,...extraHooks});
  t.after(()=>{connection.stop();for(const [key,value] of Object.entries(original)){if(value===undefined)delete globalThis[key];else globalThis[key]=value;}});
  return {state,saved,data,connection};
}

test('bootstrap then delta uses exact accepted generation, keeps draft/selection and defers focused paint',async t=>{
  const {state,saved,data,connection}=setup(t);
  const loaded=await connection.load();Object.assign(data,loaded);connection.hydrate();
  saved.items.a.draft='Typing locally';saved.items.a.manualEdited=true;saved.items.a._serverRevision=3;
  globalThis.document.activeElement={matches:()=>true};
  state.response=delta({collections:{items:{upsert:[{...state.full.items[0],revision:2,draft:'older server draft'}],remove:[]}}});
  await connection.refresh();
  assert.deepEqual(state.paths,['/api/bootstrap','/api/bootstrap/delta?since=v1']);
  assert.equal(saved.items.a.draft,'Typing locally');assert.equal(saved.items.a._serverRevision,3);assert.equal(saved.selected,'a');assert.equal(state.renders,1);
  globalThis.document.activeElement=null;
  state.response=delta({baseVersion:'v2',workspaceVersion:'v3'});await connection.refresh();
  assert.equal(state.paths.at(-1),'/api/bootstrap/delta?since=v2');assert.equal(state.renders,2);
});

test('cheap unchanged check skips delta; changed check never becomes the delta base',async t=>{
  const {state,connection}=setup(t);await connection.refresh();
  await connection.refresh({background:true});assert.deepEqual(state.paths,['/api/bootstrap','/api/workspace-version']);
  state.version='probe-generation';state.response=delta({workspaceVersion:'committed-generation'});
  await connection.refresh({background:true});assert.equal(state.paths.at(-1),'/api/bootstrap/delta?since=v1');
  state.response=delta({baseVersion:'committed-generation',workspaceVersion:'next'});await connection.refresh();
  assert.equal(state.paths.at(-1),'/api/bootstrap/delta?since=committed-generation');
});

test('invalid delta falls back without partial changes; unsupported endpoint is disabled once',async t=>{
  const {state,data,connection}=setup(t);await connection.refresh();
  state.response=delta({actorId:'another',collections:{items:{upsert:[{id:'a',draft:'must not apply'}],remove:[]}}});
  state.full.workspaceVersion='fallback';await connection.refresh();
  assert.deepEqual(state.paths.slice(-2),['/api/bootstrap/delta?since=v1','/api/bootstrap']);assert.equal(data.items[0].draft,'');
  state.status=404;await connection.refresh();await connection.refresh();
  assert.deepEqual(state.paths.slice(-3),['/api/bootstrap/delta?since=fallback','/api/bootstrap','/api/bootstrap']);
});

test('server full reset replaces conversations and removals clear missing selection',async t=>{
  const {state,saved,data,connection}=setup(t);await connection.refresh();
  state.response={kind:'full',snapshot:{...snapshot(),workspaceVersion:'reset',operator:{id:'new-actor'},items:[],conversations:[]}};
  await connection.refresh();assert.equal(saved.selected,null);assert.equal(data.items.length,0);
  state.response=delta({baseVersion:'reset',workspaceVersion:'after-reset',actorId:'new-actor'});
  await connection.refresh();assert.equal(state.paths.at(-1),'/api/bootstrap/delta?since=reset');
});

test('a delayed delta cannot apply after stop',async t=>{
  const {state,data,connection}=setup(t);await connection.refresh();
  let release;state.wait=new Promise(resolve=>release=resolve);
  state.response=delta({collections:{items:{upsert:[{...state.full.items[0],draft:'late'}],remove:[]}}});
  const pending=connection.refresh();connection.stop();release();await pending;
  assert.equal(data.items[0].draft,'');assert.equal(state.renders,1);
});

test('semantic tracker serializes only new row references and retains full-refresh equality',()=>{
  const detect=createWorkspaceChangeTracker(),counts={a:0,b:0,branch:0};
  const tracked=(id,fields={})=>({id,...fields,toJSON(){counts[id]++;const {toJSON,...value}=this;return value;}});
  const base={...snapshot(),items:[tracked('a',{draft:'A'}),tracked('b',{draft:'B'})],branches:[tracked('branch',{messages:[{text:'Long unchanged discussion'}]})]};
  assert.equal(detect(base,null,'idle').dataChanged,true);
  assert.deepEqual(counts,{a:1,b:1,branch:1});
  const clocksOnly=mergeWorkspaceDelta(base,delta({collections:{items:{upsert:[tracked('a',{draft:'A',providerObservedAt:'new'})],remove:[]}}}));
  assert.deepEqual(detect(clocksOnly,null,'idle'),{dataChanged:false,uiChanged:false});
  assert.deepEqual(counts,{a:2,b:1,branch:1},'unchanged row and branch were not serialized');
  const changed=mergeWorkspaceDelta(clocksOnly,delta({baseVersion:'v2',workspaceVersion:'v3',collections:{items:{upsert:[tracked('a',{draft:'Edited'})],remove:[]}}}));
  assert.equal(detect(changed,null,'idle').dataChanged,true);assert.deepEqual(counts,{a:3,b:1,branch:1});
  const equivalent=JSON.parse(JSON.stringify(changed));
  assert.deepEqual(detect(equivalent,null,'idle'),{dataChanged:false,uiChanged:false});
});

test('clock-only delta performs no normalization and does not serialize unchanged transport rows',async t=>{
  const {state,connection}=setup(t),base=snapshot();let serialized=0;
  base.items[1].toJSON=function(){serialized++;const {toJSON,...row}=this;return row;};
  connection.hydrate(base);const mergedBefore=state.merges;
  const changed=mergeWorkspaceDelta(base,delta({collections:{items:{upsert:[{...base.items[0],providerObservedAt:'new'}],remove:[]}}}));
  connection.hydrate(changed);
  assert.equal(serialized,1);assert.equal(state.merges,mergedBefore);assert.equal(state.renders,1);
});

test('actor reset removes drafts/private state and timers but retains general display preferences',async t=>{
  t.mock.timers.enable({apis:['setTimeout']});
  const {state,saved,data,connection}=setup(t);await connection.refresh();
  saved.items.a.draft='Old actor private draft';saved.items.a.manualEdited=true;
  Object.assign(saved,{columnWidths:{list:360},filter:'reply',mvpHistoryOrder:'oldest',mvpAiInput:'Private input',mvpFeedbackSessionId:'old-session',
    mvpFeedbackOutbox:[{eventId:'old-event'}],mvpPresented:{private:true},assistantSession:{chat:['private']},overviewInputs:{x:'private'},branches:{private:{}}});
  connection.scheduleDraft(data.items[0]);
  globalThis.document.activeElement={matches:()=>true};
  state.response={kind:'full',snapshot:{...snapshot(),workspaceVersion:'new',operator:{id:'other'},csrfToken:'new-token',conversations:[]}};
  await connection.refresh({repaint:false});
  assert.equal(saved.items.a.draft,'');assert.equal(saved.items.a.manualEdited,undefined);assert.equal(saved.selected,null);
  for(const key of ['mvpAiInput','mvpFeedbackSessionId','mvpFeedbackOutbox','mvpPresented','assistantSession','overviewInputs'])assert.equal(saved[key],undefined,key);
  assert.deepEqual(saved.branches,{});assert.deepEqual(saved.columnWidths,{list:360});assert.equal(saved.filter,'reply');assert.equal(saved.mvpHistoryOrder,'oldest');
  assert.equal(saved.mvpActorId,'other');assert.equal(state.renders,2,'actor change clears focused old-actor DOM even with deferred repaint requested');
  t.mock.timers.tick(1000);await Promise.resolve();assert.equal(state.paths.some(path=>path==='/api/items/a'),false);
});

test('session reload hook sees old drafts and blocks merge, new-token writes and late autosave continuations',async t=>{
  t.mock.timers.enable({apis:['setTimeout']});
  let context,seenDraft;
  context=setup(t,{onActorChange:()=>{seenDraft=context.saved.items.a.draft;context.connection.stop();return true;}});
  const {state,saved,data,connection}=context;await connection.refresh();
  saved.items.a.draft='Preserve under old actor';saved.items.a.manualEdited=true;
  let release;const pendingPatch=new Promise(resolve=>release=resolve),fetchWorkspace=globalThis.fetch,patchTokens=[];
  globalThis.fetch=async(path,options)=>{
    if(path==='/api/items/a'){patchTokens.push(options.headers['X-CSRF-Token']);await pendingPatch;return {ok:true,json:async()=>({...state.full.items[0],revision:2,draft:'Preserve under old actor'})};}
    return fetchWorkspace(path,options);
  };
  const saving=connection.saveDraft(data.items[0]);const savingRejected=assert.rejects(saving,/Пользователь изменился/);
  connection.scheduleDraft(data.items[1]);
  state.response={kind:'full',snapshot:{...snapshot(),operator:{id:'other'},csrfToken:'new-token',workspaceVersion:'other'}};
  await assert.rejects(connection.refresh(),/Пользователь изменился/);
  release();await savingRejected;t.mock.timers.tick(1000);await Promise.resolve();
  assert.equal(seenDraft,'Preserve under old actor');assert.equal(saved.items.a.draft,'Preserve under old actor');assert.equal(saved.mvpActorId,'owner');
  assert.deepEqual(patchTokens,['token']);assert.equal(state.renders,1);assert.equal(data.items[0].draft,'');
  await assert.rejects(connection.load(),/Пользователь изменился/);
});

test('persisted actor marker clears mismatched drafts before initial normalization',async t=>{
  let reloads=0;
  const {state,saved,connection}=setup(t,{operator:{id:'owner'},onActorChange:()=>{reloads++;return true;}});
  saved.mvpActorId='previous';saved.items.a={draft:'Other identity draft',manualEdited:true};saved.mvpAiInput='Other identity input';
  await connection.load();connection.hydrate();
  assert.equal(saved.items.a.draft,'');assert.equal(saved.mvpAiInput,undefined);assert.equal(saved.mvpActorId,'owner');assert.equal(state.paths.length,1);assert.equal(reloads,0);
});

test('unchanged workspace probe from another actor cannot suppress session replacement',async t=>{
  let actorChanges=0;
  const {state,connection}=setup(t,{onActorChange:()=>{actorChanges++;return true;}});await connection.refresh();
  state.versionActor='other';state.response={kind:'full',snapshot:{...snapshot(),operator:{id:'other'},csrfToken:'other-token'}};
  await assert.rejects(connection.refresh({background:true}),/Пользователь изменился/);
  assert.equal(actorChanges,1);assert.deepEqual(state.paths,['/api/bootstrap','/api/workspace-version','/api/bootstrap/delta?since=v1']);
});

test('same actor relogin refreshes csrf even at unchanged generation before the next mutation',async t=>{
  const {state,saved,data,connection}=setup(t);await connection.refresh();
  state.versionActor='owner';state.versionCsrf='renewed-token';state.response=delta({workspaceVersion:'v1',set:{csrfToken:'renewed-token'}});
  await connection.refresh({background:true});
  assert.deepEqual(state.paths,['/api/bootstrap','/api/workspace-version','/api/bootstrap/delta?since=v1']);
  const fetchWorkspace=globalThis.fetch,tokens=[];
  globalThis.fetch=async(path,options)=>{
    if(path==='/api/items/a'){tokens.push(options.headers['X-CSRF-Token']);return {ok:true,json:async()=>({...state.full.items[0],revision:2,draft:'New edit'})};}
    return fetchWorkspace(path,options);
  };
  saved.items.a.draft='New edit';saved.items.a.manualEdited=true;await connection.saveDraft(data.items[0]);
  assert.deepEqual(tokens,['renewed-token']);assert.equal(saved.items.a._serverRevision,2);
});

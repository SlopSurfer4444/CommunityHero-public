import test from 'node:test';
import assert from 'node:assert/strict';
import {createMvpConnection} from '../workshop/mvp-connection.js';

const flush=async()=>{for(let i=0;i<12;i++)await Promise.resolve();};
const raw=()=>({items:[{id:'one',revision:1,workflow:'attention',draft:''}],posts:[],branches:[],proposals:[],jobs:[{id:'job',kind:'sync',status:'running',updatedAt:'old'}],sync:{status:'running',open:{hasMore:false,coverage:{complete:false},lastSyncedAt:'old'},scan:{updatedAt:'old'}}});
function setup(t){
  t.mock.timers.enable({apis:['setTimeout','setInterval','Date'],now:100000});
  const original={fetch:globalThis.fetch,document:globalThis.document,EventSource:globalThis.EventSource};
  const state={calls:0,renders:0,merges:0,source:null,response:raw(),pending:null,paths:[],listeners:{},version:null,versionUnavailable:false};
  globalThis.document={activeElement:null,visibilityState:'visible',querySelector:()=>null,addEventListener:(event,fn)=>state.listeners[event]=fn,removeEventListener:event=>delete state.listeners[event]};
  globalThis.EventSource=class{constructor(){state.source=this;this.closed=false;}addEventListener(_,fn){this.emit=fn;}close(){this.closed=true;}};
  globalThis.fetch=async(path)=>{state.calls++;state.paths.push(path);if(state.pending)await state.pending;
    if(path==='/api/workspace-version')return {ok:!state.versionUnavailable,status:state.versionUnavailable?404:200,json:async()=>({workspaceVersion:state.version})};
    return {ok:true,json:async()=>structuredClone(state.response)};};
  const saved={items:{}},data={items:[]};
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:item=>{state.merges++;return saved.items[item.id]??=structuredClone(item.initialState);},render:()=>state.renders++});
  t.after(()=>{connection.stop();for(const [key,value] of Object.entries(original)){if(value===undefined)delete globalThis[key];else globalThis[key]=value;}});
  return {connection,state,data,saved};
}

test('SSE burst coalesces and continuous events cannot starve refresh',async t=>{
  const {connection,state}=setup(t);connection.start();
  for(let i=0;i<80;i++)state.source.emit();
  t.mock.timers.tick(249);await flush();assert.equal(state.calls,0);
  t.mock.timers.tick(1);await flush();assert.equal(state.calls,1);
  for(let i=0;i<5;i++){state.source.emit();t.mock.timers.tick(200);await flush();}
  assert.equal(state.calls,2);
});

test('timestamp-only snapshots skip normalization/render while meaningful jobs and items update',async t=>{
  const {connection,state,data}=setup(t);
  await connection.refresh();assert.equal(state.renders,1);assert.equal(state.merges,1);
  const item=data.items[0];
  state.response.items[0].providerObservedAt='new';state.response.jobs[0].updatedAt='new';state.response.sync.scan.updatedAt='new';state.response.sync.open.lastSyncedAt='new';
  state.response.items[0].statusObservedAt='new';state.response.items[0].providerStatusObservedAt='new';state.response.items[0].contextObservedAt='new';
  await connection.refresh();assert.equal(state.renders,1);assert.equal(state.merges,1);assert.equal(data.items[0],item);
  state.response.jobs[0].status='completed';state.response.sync.status='partial';
  await connection.refresh();assert.equal(state.renders,2);assert.equal(state.merges,1);
  state.response.items[0].revision=2;state.response.items[0].draft='Server change';
  await connection.refresh();assert.equal(state.renders,3);assert.equal(state.merges,2);assert.equal(data.items[0].draft,'Server change');
});

test('queue coverage updates repaint without remapping items and use the latest canonical observation',async t=>{
  const {connection,state,data}=setup(t);
  await connection.refresh();const item=data.items[0];
  assert.equal(connection.queueCoverage('attention').state,'unknown');
  state.response.sync.openCoverage={scope:'all-open',done:false,coverageComplete:false,pages:1};
  await connection.refresh();assert.equal(state.renders,2);assert.equal(state.merges,1);
  assert.equal(connection.queueCoverage('attention').note,'Сверка очереди продолжается.');
  state.response.sync.openCoverage={scope:'all-open',done:true,traversalComplete:true,contextComplete:true,coverageComplete:true,
    snapshotConsistent:false,accounting:{version:1,trackedUnique:1,importedUnique:1,unresolvedUnique:0,unverifiedPages:0,overflow:false}};
  await connection.refresh();assert.equal(state.renders,3);assert.equal(state.merges,1);assert.equal(data.items[0],item);
  assert.equal(connection.queueCoverage('attention').state,'complete');
  state.response.sync.openCoverage.invalidatedAt='changed';
  await connection.refresh();assert.equal(state.renders,4);assert.equal(connection.queueCoverage('attention').state,'incomplete');
  state.response.sync.background={state:'backoff'};
  await connection.refresh();assert.equal(state.renders,5);assert.match(connection.queueCoverage('attention').note,/Связь временно недоступна/);
});

test('background checks cheap generation but explicit refresh always obtains the actual snapshot',async t=>{
  const {connection,state}=setup(t);state.response.workspaceVersion='one';state.version='one';
  await connection.refresh();connection.start();state.source.emit();t.mock.timers.tick(250);await flush();
  assert.deepEqual(state.paths,['/api/bootstrap','/api/workspace-version']);
  state.version='two';state.response.workspaceVersion='three';state.source.emit();t.mock.timers.tick(250);await flush();
  assert.deepEqual(state.paths.slice(-2),['/api/workspace-version','/api/bootstrap']);
  state.version='three';state.source.emit();t.mock.timers.tick(250);await flush();
  assert.equal(state.paths.filter(p=>p==='/api/bootstrap').length,2,'accepted snapshot version is three, never cheap probe two');
  await connection.refresh();assert.equal(state.paths.filter(p=>p==='/api/bootstrap').length,3);
});

test('hidden page suspends background traffic and resumes with one version check',async t=>{
  const {connection,state}=setup(t);state.response.workspaceVersion='one';state.version='one';await connection.refresh();connection.start();
  globalThis.document.visibilityState='hidden';state.listeners.visibilitychange();
  for(let i=0;i<20;i++)state.source.emit();t.mock.timers.tick(60000);await flush();assert.equal(state.calls,1);
  globalThis.document.visibilityState='visible';state.listeners.visibilitychange();t.mock.timers.tick(250);await flush();
  assert.deepEqual(state.paths,['/api/bootstrap','/api/workspace-version']);
  connection.stop();assert.equal(state.listeners.visibilitychange,undefined);
});

test('old server falls back once on unavailable version endpoint',async t=>{
  const {connection,state}=setup(t);state.response.workspaceVersion='one';state.versionUnavailable=true;await connection.refresh();connection.start();
  state.source.emit();t.mock.timers.tick(250);await flush();
  state.source.emit();t.mock.timers.tick(250);await flush();
  assert.equal(state.paths.filter(p=>p==='/api/workspace-version').length,1);
  assert.equal(state.paths.filter(p=>p==='/api/bootstrap').length,3);
});

test('events during fetch produce one followup instead of disappearing',async t=>{
  const {connection,state}=setup(t);connection.start();
  let release;state.pending=new Promise(resolve=>release=resolve);
  state.source.emit();t.mock.timers.tick(250);await flush();assert.equal(state.calls,1);
  for(let i=0;i<100;i++)state.source.emit();
  state.pending=null;release();await flush();assert.equal(state.calls,2);
  t.mock.timers.tick(1000);await flush();assert.equal(state.calls,2);
});

test('stop removes debounce/poll and prevents late responses or queued followups',async t=>{
  const {connection,state}=setup(t);connection.start();state.source.emit();connection.stop();
  t.mock.timers.tick(30000);await flush();assert.equal(state.calls,0);assert.equal(state.source.closed,true);
  connection.start();let release;state.pending=new Promise(resolve=>release=resolve);
  state.source.emit();t.mock.timers.tick(250);await flush();state.source.emit();connection.stop();
  release();await flush();t.mock.timers.tick(30000);await flush();assert.equal(state.calls,1);assert.equal(state.renders,0);
});

test('typing is preserved and deferred paint occurs after typing ends even on unchanged snapshot',async t=>{
  const {connection,state,saved}=setup(t);await connection.refresh();
  saved.items.one.draft='Typing';saved.items.one.manualEdited=true;
  globalThis.document.activeElement={matches:()=>true};state.response.items[0].revision=2;
  await connection.refresh();assert.equal(state.renders,1);assert.equal(saved.items.one.draft,'Typing');
  globalThis.document.activeElement=null;await connection.refresh();assert.equal(state.renders,2);assert.equal(saved.items.one.draft,'Typing');
});

test('newly available transcript updates existing post without changing item revisions or calling media',async t=>{
  const {connection,state,data}=setup(t);
  globalThis.document.createElement=()=>({innerHTML:'',get value(){return this.innerHTML;}});
  state.response.account='LikeAvto';state.response.posts=[{id:'post',postKey:'vk',sourceUrl:'https://youtu.be/abcdefghijk',title:'Video'}];
  state.response.materials=[];
  await connection.refresh();assert.equal(data.posts[0].transcripts.length,0);
  state.response.materials=[{id:'transcript',kind:'transcript',postKey:'youtube',sourceUrl:'https://youtube.com/watch?v=abcdefghijk',text:'Already prepared transcript'}];
  await connection.refresh();assert.equal(data.posts[0].transcripts[0].text,'Already prepared transcript');
  assert.equal(data.posts[0].transcripts[0].shared,true);
  assert.equal(state.calls,2);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {mediaPreparationHold,replyReadiness,updateMediaActionControls} from '../workshop/preparation-readiness.js';
import {normalizeMvpItem,mergeMvpItemState,createMvpConnection} from '../workshop/mvp-connection.js';
import {queueTags,outcomeCounts} from '../workshop/workspace-presentation.js';
import {mergeWorkspaceDelta} from '../workshop/workspace-delta.js';

const readiness=status=>({schemaVersion:2,required:true,status});
const item={id:'one',revision:2,workflow:'prepared',draft:'Ручной ответ',draftEdited:true,providerStatus:'new',contextEvidenceDigest:'c',branchContextDigest:'b',autoPreparation:{status:'prepared'}};
const proposal={id:'p',revision:1,itemId:'one',itemRevision:2,status:'draft',kind:'reply_and_close',text:'Ручной ответ',contextEvidenceDigest:'c',branchContextDigest:'b'};

test('strict v2 ready supersedes legacy hold; malformed and unknown explicit readiness hold',()=>{
  const legacy={preparationMediaWait:{status:'media_wait'}};
  assert.equal(mediaPreparationHold(legacy).status,'media_wait');
  assert.equal(mediaPreparationHold({...legacy,mediaReadiness:readiness('ready')}),null);
  assert.equal(mediaPreparationHold({}),null,'absent projection preserves legacy compatibility');
  for(const marker of [null,{},'ready',{schemaVersion:1,required:true,status:'ready'},readiness('unknown'),{schemaVersion:2,required:false,status:'ready'}]){
    assert.equal(mediaPreparationHold({mediaReadiness:marker}).status,'media_unavailable');
  }
});

test('protected manual draft remains exact but held presentation is excluded from prepared counts',()=>{
  const raw={...item,mediaReadiness:readiness('media_wait')},before=structuredClone(raw);
  const normalized=normalizeMvpItem(raw,[proposal]),state=normalized.initialState;
  assert.deepEqual(raw,before);assert.equal(normalized.workflow,'prepared');assert.equal(state.view,'attention');
  assert.equal(state.draft,item.draft);assert.equal(state._serverDraftEdited,true);
  const record={item:normalized,state,messages:[]};
  assert.equal(outcomeCounts([record],{view:'prepared',filters:{}}).all,0);
  assert.equal(outcomeCounts([record],{view:'attention',filters:{}}).all,1);
  assert.equal(queueTags(state,normalized)[0].label,'Получаем контекст видео');
  assert.equal(replyReadiness(normalized,state).disabled,true);
  assert.doesNotMatch(state.note,/Ошибка разбора|Решение подготовлено/);
});

test('wait to ready to unavailable preserves unsaved edits, clears and undo history',()=>{
  for(const draft of ['Несохранённая правка','']){
    const state={...normalizeMvpItem(item).initialState,draft,manualEdited:true,history:['раньше'],redo:['позже']};
    for(const status of ['media_wait','ready','media_unavailable']){
      const normalized=normalizeMvpItem({...item,mediaReadiness:readiness(status)});
      mergeMvpItemState(state,normalized);
      assert.equal(state.draft,draft);assert.deepEqual(state.history,['раньше']);assert.deepEqual(state.redo,['позже']);
      assert.equal(state.view,status==='ready'?'prepared':'attention');
      assert.equal(replyReadiness(normalized,state).disabled,status!=='ready'||!draft);
    }
  }
});

test('unavailable media differs from parsing failure and closed outcomes remain closed',()=>{
  const normalized=normalizeMvpItem({...item,mediaReadiness:readiness('media_unavailable'),autoPreparation:{status:'error'}});
  assert.equal(queueTags(normalized.initialState,normalized)[0].label,'Контекст видео пока недоступен');
  const closed=normalizeMvpItem({...item,workflow:'closed',mediaReadiness:readiness('media_wait')});
  assert.equal(closed.view,'closed');assert.equal(queueTags(closed.initialState,closed)[0].label,'Без ответа');
});

test('full and delta readiness transitions have identical presentation without item revision changes',()=>{
  const base={workspaceVersion:'w:1',operator:{id:'owner'},items:[item]};
  const next={...item,mediaReadiness:readiness('media_wait')};
  const delta={kind:'delta',actorId:'owner',baseVersion:'w:1',workspaceVersion:'w:2',collections:{items:{upsert:[next],remove:[]}},set:{},remove:[]};
  const merged=mergeWorkspaceDelta(base,delta);
  assert.deepEqual(normalizeMvpItem(merged.items[0]),normalizeMvpItem(next));
  assert.equal(base.items[0].mediaReadiness,undefined);
});

test('focused editor controls stay held through typing and re-enable only on explicit ready',()=>{
  const send={},close={},status={dataset:{},textContent:'Черновик · не отправлен'},editor={value:'local edit'};
  const root={querySelector:selector=>({'.composer .send-button':send,'#close-comment':close,'#draft-status':status})[selector]};
  const state={draft:editor.value,manualEdited:true};
  for(const draft of ['new edit','', 'undo text']){
    state.draft=draft;
    updateMediaActionControls(root,{mediaReadiness:readiness('media_wait')},state);
    assert.equal(send.disabled,true);assert.equal(close.disabled,true);assert.equal(status.textContent,'Получаем контекст видео');
  }
  updateMediaActionControls(root,{mediaReadiness:readiness('ready')},state);
  assert.equal(send.disabled,false);assert.equal(close.disabled,false);assert.doesNotMatch(status.textContent,/Получаем/);
  assert.equal(editor.value,'local edit','controls never modify text');
});

test('reply, close and mixed bulk hold before any mutation or draft autosave',async t=>{
  const old=globalThis.fetch;t.after(()=>globalThis.fetch=old);
  let calls=0;globalThis.fetch=async()=>{calls++;throw Error('unexpected network');};
  const held=normalizeMvpItem({...item,mediaReadiness:readiness('media_wait')});
  const ready=normalizeMvpItem({...item,id:'other',mediaReadiness:readiness('ready')});
  const states=new Map([held,ready].map(row=>[row.id,{...row.initialState,manualEdited:true,draft:'local edit'}]));
  const connection=createMvpConnection({getData:()=>({items:[held,ready]}),getSaved:()=>({}),stateFor:row=>states.get(row.id)});
  t.after(()=>connection.stop());
  await assert.rejects(connection.prepareReply(held),/Получаем контекст видео/);
  await assert.rejects(connection.closeOne(held),/Получаем контекст видео/);
  await assert.rejects(connection.closeMany([ready,held]),/Получаем контекст видео/);
  assert.equal(calls,0);assert.equal(states.get(held.id).draft,'local edit');
});

test('already open review rechecks fresh readiness before approval or execute',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  let clicks,html='',calls=[];const button={disabled:false,addEventListener:(_name,handler)=>clicks=handler},mediaStatus={},controls=new Map();
  const control=selector=>{if(!controls.has(selector))controls.set(selector,{disabled:false,hidden:false,checked:false,innerHTML:'',addEventListener(){}});return controls.get(selector);};
  const dialog={remove(){},showModal(){},addEventListener(){},querySelectorAll:()=>[],querySelector:selector=>selector==='[data-media-readiness]'?mediaStatus:selector==='[data-confirm]'?button:control(selector),set innerHTML(value){html=value;}};
  globalThis.document={activeElement:null,createElement:()=>dialog,body:{append(){}}};
  const snapshot={items:[{...item,mediaReadiness:readiness('ready')}],posts:[],branches:[],proposals:[proposal],operations:[]};
  let raw=snapshot,data={items:[]},readinessUpdates=0,holdDuringApproval=false,editorialBody;const saved={items:{}};
  globalThis.fetch=async(path,options={})=>{
    const body=options.body?JSON.parse(options.body):undefined;
    calls.push({path,method:options.method||'GET',body});
    if(path==='/api/proposals/editorial-review'){
      editorialBody=body;
      return {ok:true,json:async()=>({jobId:'editorial-media-fixture',requestId:body.requestId,replayed:false})};
    }
    if(path==='/api/jobs/editorial-media-fixture')return {ok:true,json:async()=>({id:'editorial-media-fixture',kind:'editorial_review',purpose:'editorial_review',refId:editorialBody.requestId,status:'completed',result:{accepted:editorialBody.proposals,reused:[],held:[]}})};
    if(path==='/api/approvals'&&holdDuringApproval){raw={...snapshot,items:[{...item,mediaReadiness:readiness('media_wait')}]};connection.hydrate(raw,{repaint:false});return {ok:true,json:async()=>({id:'approved',status:'approved',requestId:body.requestId,replayed:false})};}
    return {ok:true,json:async()=>raw};
  };
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:row=>saved.items[row.id]||=structuredClone(row.initialState),esc:String,icon:()=>'',updateDecisionReadiness(){readinessUpdates++;}});
  t.after(()=>connection.stop());data=await connection.load();connection.hydrate(undefined,{repaint:false});
  connection.review(['p']);assert.doesNotMatch(html,/data-confirm disabled/);
  raw={...snapshot,items:[{...item,mediaReadiness:readiness('media_wait')}]};
  connection.hydrate(raw,{repaint:false});
  assert.ok(readinessUpdates>=1,'partial hydration refreshes controls even without repaint');
  assert.equal(button.disabled,true,'open review disables immediately on refreshed hold');
  assert.match(mediaStatus.textContent,/Получаем контекст видео/);
  await clicks({currentTarget:button});
  assert.equal(calls.filter(call=>call.method!=='GET').length,0);assert.equal(button.disabled,true);
  connection.review(['p']);assert.match(html,/Получаем контекст видео/);assert.match(html,/data-confirm disabled/);
  raw=snapshot;connection.hydrate(raw,{repaint:false});connection.review(['p']);holdDuringApproval=true;
  await clicks({currentTarget:button});
  assert.deepEqual(calls.filter(call=>call.method!=='GET').map(call=>call.path),['/api/proposals/editorial-review'],'fresh editorial acceptance does not approve or execute automatically');
  assert.deepEqual(editorialBody.proposals,[{id:'p',revision:1}],'editorial review covers the exact displayed version');
  assert.match(control('[data-editorial-result]').innerHTML,/Проверка завершена/);
  await clicks({currentTarget:button});
  assert.deepEqual(calls.filter(call=>call.method!=='GET').map(call=>call.path),['/api/proposals/editorial-review','/api/approvals'],'a newly held valid approval is not executed');
  assert.deepEqual(calls.find(call=>call.path==='/api/approvals').body.proposals,[{id:'p',revision:1}]);
  assert.equal(button.disabled,true);assert.match(mediaStatus.textContent,/Получаем контекст видео/);
});

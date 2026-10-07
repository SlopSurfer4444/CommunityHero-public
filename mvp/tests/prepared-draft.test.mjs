import test from 'node:test';
import assert from 'node:assert/strict';
import {normalizeMvpItem,mergeMvpItemState,createMvpConnection} from '../workshop/mvp-connection.js';

const item={id:'one',revision:2,workflow:'prepared',draft:'',contextEvidenceDigest:'context',branchContextDigest:'branch',autoPreparation:{status:'prepared',reason:'Ответ подтверждён материалами'}};
const proposal={id:'proposal',itemId:'one',itemRevision:2,status:'draft',kind:'reply_and_close',text:'Предлагаемый ответ',contextEvidenceDigest:'context',branchContextDigest:'branch'};

test('current proposal appears in composer while server draft remains actual',()=>{
  const normalized=normalizeMvpItem(item,[proposal]);
  assert.equal(normalized.draft,proposal.text);
  assert.equal(normalized.initialState.draft,proposal.text);
  assert.equal(normalized.initialState._serverDraft,'');
  assert.equal(normalized.initialState._derivedDraft,proposal.text);
  assert.equal(normalized.reason,item.autoPreparation.reason);
  assert.match(normalized.initialState.note,/Решение подготовлено/);
  const human=normalizeMvpItem({...item,draft:'Мой ответ'},[proposal]);
  assert.equal(human.draft,'Мой ответ');
  assert.equal(human.initialState._derivedDraft,null);
});

test('stale proposal text remains reviewable but cancelled and unrelated proposals stay hidden',()=>{
  for(const change of [{itemRevision:1},{contextEvidenceDigest:'old'},{branchContextDigest:'old'},{status:'stale'}]){
    const normalized=normalizeMvpItem(item,[{...proposal,...change}]);
    assert.equal(normalized.draft,proposal.text);
    assert.equal(normalized.initialState._staleGenerated,true);
    assert.equal(normalized.initialState._displayedProposalId,null);
    assert.match(normalized.initialState.note,/требует проверки/);
  }
  for(const change of [{status:'cancelled'},{itemId:'other'}]){
    const normalized=normalizeMvpItem(item,[{...proposal,...change}]);
    assert.equal(normalized.draft,'');
    assert.equal(normalized.view,'attention');
    assert.match(normalized.initialState.note,/повторной подготовки/);
  }
  assert.equal(normalizeMvpItem(item,[proposal,{...proposal,text:'Stale',itemRevision:1}]).draft,proposal.text);
});

test('prepared projection distinguishes missing AI result from a manual draft or a valid close decision',()=>{
  assert.equal(normalizeMvpItem(item,[]).view,'attention');
  assert.equal(normalizeMvpItem({...item,draft:'Правка оператора'},[]).view,'prepared');
  const close=normalizeMvpItem(item,[{...proposal,kind:'close',text:''}]);
  assert.equal(close.view,'prepared');
  assert.equal(close.decision,'no_reply');
});

test('refresh replaces untouched current text and retains the persisted proposal when stale',()=>{
  const initial=normalizeMvpItem(item,[proposal]),state=structuredClone(initial.initialState);
  mergeMvpItemState(state,normalizeMvpItem(item,[{...proposal,text:'Обновлённый ответ'}]));
  assert.equal(state.draft,'Обновлённый ответ');
  mergeMvpItemState(state,normalizeMvpItem({...item,revision:3},[proposal]));
  assert.equal(state.draft,proposal.text);
  assert.equal(state._staleGenerated,true);
  assert.equal(state._serverDraft,'');
});

test('saved stale generated text is not autosaved or prepared without an explicit operator review',async t=>{
  const normalized=normalizeMvpItem({...item,workflow:'attention',autoPreparation:{status:'stale',requiresReview:true,savedProposalId:proposal.id}},[{...proposal,status:'stale'}]);
  const state=structuredClone(normalized.initialState),data={items:[normalized]},saved={items:{one:state}};
  const original=globalThis.fetch;t.after(()=>globalThis.fetch=original);
  globalThis.fetch=async()=>{throw Error('must not call network');};
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:()=>state});
  await connection.saveDraft(normalized);
  await assert.rejects(connection.prepareReply(normalized),/Сначала проверьте/);
  assert.equal(state.draft,proposal.text);connection.stop();
});

test('bootstrap begun before autosave cannot erase the acknowledged draft when another item changed',async t=>{
  const originalFetch=globalThis.fetch,originalDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=originalFetch;globalThis.document=originalDocument;});
  const base={...item,revision:1,workflow:'attention',draft:'',draftEdited:false,autoPreparation:null};
  const raw={items:[base],posts:[],branches:[],proposals:[],operations:[]};
  const stale={...raw,operations:[{id:'changed-operation-on-another-item'}]};
  const updated={...base,revision:2,draft:'Saved human text',draftEdited:true};
  let reads=0,release,data={items:[]};const saved={items:{}},renders=[];
  const gate=new Promise(resolve=>release=resolve);
  globalThis.document={activeElement:null};
  globalThis.fetch=async(path,options={})=>{
    if(options.method==='PATCH')return {ok:true,json:async()=>updated};
    reads++;
    if(reads===2){await gate;return {ok:true,json:async()=>stale};}
    return {ok:true,json:async()=>reads>=3?{...stale,items:[updated]}:raw};
  };
  const stateFor=row=>saved.items[row.id]||=structuredClone(row.initialState);
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor,persist(){},render(){renders.push({draft:saved.items.one.draft,revision:saved.items.one._serverRevision});}});
  t.after(()=>connection.stop());data=await connection.load();connection.hydrate(undefined,{repaint:false});
  const target=data.items[0],state=stateFor(target),refresh=connection.refresh();
  state.draft='Saved human text';state.manualEdited=true;
  const save=connection.saveDraft(target);await new Promise(resolve=>setImmediate(resolve));
  assert.equal(state._serverRevision,2,'PATCH acknowledged before delayed GET returns');
  release();await Promise.all([refresh,save]);
  assert.equal(state.draft,'Saved human text');assert.equal(state._serverRevision,2);
  assert.ok(renders.length);assert.ok(renders.every(row=>row.draft==='Saved human text'&&row.revision===2));
});

test('refresh preserves human typing, clearing and explicit decision override',()=>{
  for(const draft of ['Ручная правка','']){
    const state={...normalizeMvpItem(item,[proposal]).initialState,draft,manualEdited:true};
    mergeMvpItemState(state,normalizeMvpItem(item,[{...proposal,text:'New AI'}]));
    assert.equal(state.draft,draft);
  }
  const closedProposal={...proposal,kind:'close',text:''};
  const normalized=normalizeMvpItem(item,[closedProposal]);
  assert.equal(normalized.decision,'no_reply');
  const state={...normalized.initialState,decision:'reply',replyStarted:true};
  mergeMvpItemState(state,normalized);
  assert.equal(state.decision,'reply');
});

test('automatic preparation states use existing labels and notes',()=>{
  for(const [status,label] of [['running','готовит'],['needs_attention','Нужно участие'],['error','Не удалось']]){
    const normalized=normalizeMvpItem({...item,workflow:'attention',autoPreparation:{status,reason:'Причина'}},[]);
    assert.match(normalized.attentionLabel,new RegExp(label));
    assert.match(normalized.initialState.note,/Причина/);
    assert.equal(normalized.reason,'Причина');
  }
});

test('all unprepared open comments distinguish verification from queued triage regardless of age',()=>{
  const now=Date.parse('2026-09-21T15:00:00Z');
  const fresh={...item,workflow:'attention',providerStatus:'new',createdAt:'2026-09-21T12:00:00Z',autoPreparation:null};
  const normalize=(changes={},local={})=>normalizeMvpItem({...fresh,...changes},[],now,local);
  assert.equal(normalize().attentionLabel,'Проверяем статус');
  assert.equal(normalize({providerObservedAt:'2026-09-21T14:49:59.999Z'}).initialState.note,'Проверяем статус');
  assert.equal(normalize({providerObservedAt:'2026-09-21T14:50:00Z'}).attentionLabel,'Ожидает разбора');
  assert.equal(normalize({providerStatus:'inprogress',providerObservedAt:'2026-09-21T14:59:00Z'}).initialState.note,'Ожидает разбора');
  assert.equal(normalize({autoPreparation:{status:'queued'}}).initialState.note,'Ожидает разбора');
  for(const createdAt of ['2020-01-01T12:00:00Z',undefined,null,'not-a-date']){
    assert.equal(normalize({createdAt}).initialState.note,'Проверяем статус');
    assert.equal(normalize({createdAt,providerObservedAt:'2026-09-21T14:59:00Z'}).initialState.note,'Ожидает разбора');
    assert.equal(normalize({createdAt,autoPreparation:{status:'queued'}}).initialState.note,'Ожидает разбора');
  }
  for(const changes of [{draft:'Мой текст'},{workflow:'waiting'},{workflow:'closed'},{providerStatus:'closed'},{createdAt:'2026-09-21T15:00:00.001Z'}]){
    assert.equal(normalize(changes).initialState.note,'');
    assert.equal(normalize({...changes,autoPreparation:{status:'queued'}}).initialState.note,'');
  }
  assert.equal(normalize({}, {draft:'Локальная правка'}).initialState.note,'');
  assert.equal(normalize({}, {draft:'',manualEdited:true}).initialState.note,'');
});

test('saveDraft never PATCHes untouched derived AI text; edited text does save',async()=>{
  const normalized=normalizeMvpItem(item,[proposal]),state=structuredClone(normalized.initialState);
  const data={items:[normalized]},saved={items:{one:state}},calls=[];
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:()=>state});
  const originalFetch=globalThis.fetch;
  globalThis.fetch=async(path,options)=>{
    calls.push({path,...options});
    if(options.method==='PATCH')return {ok:true,json:async()=>({...item,revision:3,draft:'Ручная правка',draftEdited:true})};
    return {ok:true,json:async()=>({items:[{...item,revision:3,draft:'Ручная правка',draftEdited:true}],proposals:[proposal]})};
  };
  try{
    await connection.saveDraft(normalized);
    assert.equal(calls.length,0);
    state.draft='Ручная правка';state.manualEdited=true;
    await connection.saveDraft(normalized);
    assert.equal(calls[0].method,'PATCH');
    assert.equal(JSON.parse(calls[0].body).draft,'Ручная правка');
    assert.equal(state._serverDraft,'Ручная правка');
    assert.equal(state._derivedDraft,null);
    await connection.saveDraft(data.items[0]);
    assert.equal(calls.length,2);
  }finally{globalThis.fetch=originalFetch;connection.stop();}
});


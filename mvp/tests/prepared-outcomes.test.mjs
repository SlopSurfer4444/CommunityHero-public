import test from 'node:test';
import assert from 'node:assert/strict';
import {normalizeMvpItem,mergeMvpItemState,createMvpConnection} from '../workshop/mvp-connection.js';
import {replyReadiness,updateMediaActionControls} from '../workshop/preparation-readiness.js';
import {readFileSync} from 'node:fs';

const item={id:'synthetic-item',revision:2,workflow:'prepared',view:'attention',draft:'',draftEdited:false,
  autoPreparation:null,providerStatus:'new',contextEvidenceDigest:'context',branchContextDigest:'branch'};
const proposal={id:'synthetic-proposal',itemId:item.id,itemRevision:2,revision:1,status:'failed',kind:'reply_and_close',
  text:'Сохранённый синтетический ответ',contextEvidenceDigest:'context',branchContextDigest:'branch'};
const operation={id:'synthetic-operation',itemId:item.id,proposalId:proposal.id,status:'failed',
  evidence:{code:'fresh_context_read_failed',phase:'predispatch',providerCallAttempted:false,mutationOutcome:'not-attempted',providerRetryAllowed:false}};

test('failed exact-shape reply retains text, explains failure and cannot remain Prepared',()=>{
  const original=structuredClone({item,proposal,operation});
  const normalized=normalizeMvpItem(item,[proposal],0,{},[operation]);
  assert.equal(normalized.view,'attention');assert.equal(normalized.workflow,'prepared','display does not mutate durable rows');
  assert.equal(normalized.draft,proposal.text);assert.equal(normalized.initialState._staleGenerated,true);
  assert.equal(normalized.initialState._displayedProposalId,null);assert.equal(normalized.initialState._serverDraft,'');
  assert.equal(normalized.preparationDisposition.status,'failed');
  assert.match(normalized.preparationDisposition.detail,/актуальный комментарий/);
  assert.equal(replyReadiness(normalized,normalized.initialState).disabled,true);
  assert.deepEqual({item,proposal,operation},original);
  const unrelated={...operation,id:'other-attempt',proposalId:'other-proposal',evidence:{code:'local_context_read_failed'}};
  assert.match(normalizeMvpItem(item,[proposal],0,{},[operation,unrelated]).preparationDisposition.detail,/актуальный комментарий/);
});

test('an UNKNOWN operation from a different connection never binds to the current company action',()=>{
  const binding={id:'current',workspaceId:'workspace',accountId:'Company',connector:'native',revision:1,providerAccountId:'current'};
  const raw={...item,connectorBinding:binding};
  const foreign={...operation,status:'unknown',target:{connectorBinding:{...binding,accountId:'Other Company'}}};
  const normalized=normalizeMvpItem(raw,[{...proposal,status:'draft'}],0,{},[foreign]);
  assert.equal(normalized.preparationDisposition,null);assert.equal(normalized.view,'prepared');
});

test('failed display claims no attempt only when both server-owned no-attempt fields agree',()=>{
  for(const evidence of [undefined,{}, {mutationOutcome:'not-attempted'},{providerCallAttempted:false},
    {mutationOutcome:'not-attempted',providerCallAttempted:true},{mutationOutcome:'unknown',providerCallAttempted:false}]) {
    const normalized=normalizeMvpItem(item,[proposal],0,{},[{...operation,evidence}]);
    assert.equal(normalized.preparationDisposition.label,'Не удалось подтвердить выполнение');
    assert.doesNotMatch(normalized.preparationDisposition.label,/не отправлен|не выполнено|не выполнялось/);
  }
  assert.equal(normalizeMvpItem(item,[proposal],0,{},[operation]).preparationDisposition.label,'Действие не выполнялось');
});

test('historical UNKNOWN still blocks canonical recipient after binding revision/change or missing metadata',()=>{
  const binding={id:'current',workspaceId:'workspace',accountId:'Company',connector:'native',revision:2,providerAccountId:'current'};
  const raw={...item,connectorBinding:binding};
  for(const target of [undefined,{}, {connectorBinding:{accountId:'Company'}},
    {connectorBinding:{...binding,revision:1}},{connectorBinding:{...binding,id:'old',connector:'previous',providerAccountId:'previous'}}]) {
    const unknown={...operation,status:'unknown',target};
    const normalized=normalizeMvpItem(raw,[{...proposal,status:'draft'}],0,{},[unknown]);
    assert.equal(normalized.preparationDisposition.status,'unknown');assert.equal(normalized.view,'attention');
    assert.equal(replyReadiness(normalized,{...normalized.initialState,manualEdited:true}).disabled,true);
  }
});

test('intentional local clearing excludes saved server reply from Prepared while current close stays ready',()=>{
  const raw={...item,draft:'Previously saved reply',draftEdited:true};
  const cleared={draft:'',manualEdited:true};
  assert.equal(normalizeMvpItem(raw,[{...proposal,status:'draft'}],0,cleared).view,'attention');
  assert.equal(normalizeMvpItem(raw,[{...proposal,status:'draft',kind:'close',text:''}],0,cleared).view,'prepared');
});

test('Prepared invariant does not depend on auto preparation marker or require text for a current close',()=>{
  for(const marker of [null,undefined,{status:'prepared'},{status:'error'}]) {
    const raw={...item,autoPreparation:marker};
    assert.equal(normalizeMvpItem(raw,[]).view,'attention');
    assert.equal(normalizeMvpItem({...raw,draft:'Ручной ответ',draftEdited:true},[]).view,'prepared');
    assert.equal(normalizeMvpItem(raw,[{...proposal,status:'draft',kind:'close',text:''}]).view,'prepared');
    assert.equal(normalizeMvpItem(raw,[{...proposal,status:'draft',kind:'close',text:''}]).decision,'no_reply');
    assert.equal(normalizeMvpItem(raw,[{...proposal,status:'draft',text:' '}]).view,'attention');
  }
});

test('failed and unknown close decisions remain visible as held decisions without invented reply text',()=>{
  for(const status of ['failed','unknown']) {
    const normalized=normalizeMvpItem(item,[{...proposal,status,kind:'close',text:''}]);
    assert.equal(normalized.view,'attention');assert.equal(normalized.decision,'no_reply');
    assert.equal(normalized.draft,'');assert.equal(normalized.preparationDisposition.status,status);
    assert.equal(normalized.initialState._displayedProposalId,null);
  }
});

test('newer current proposals win over historical failed text; cancelled/succeeded never revive text',()=>{
  for(const status of ['cancelled','succeeded']) {
    const normalized=normalizeMvpItem(item,[{...proposal,status}]);
    assert.equal(normalized.draft,'');assert.equal(normalized.view,'attention');
  }
  const normalized=normalizeMvpItem(item,[proposal,{...proposal,id:'new',status:'draft',text:'Новый ответ'}],0,{},[operation]);
  assert.equal(normalized.draft,'Новый ответ');assert.equal(normalized.view,'prepared');
  assert.equal(normalized.preparationDisposition,null);assert.equal(normalized.initialState._displayedProposalId,'new');
  const stale=normalizeMvpItem(item,[{...proposal,status:'stale'}]);
  assert.equal(stale.draft,proposal.text);assert.equal(stale.initialState._staleGenerated,true);
});

test('manual edits and intentional clears survive failed/unknown transitions and newer proposals',()=>{
  for(const draft of ['Моя правка','']) {
    const state={...normalizeMvpItem(item,[{...proposal,status:'draft'}]).initialState,draft,manualEdited:true,history:['undo'],redo:['redo']};
    for(const status of ['failed','unknown','draft']) {
      mergeMvpItemState(state,normalizeMvpItem(item,[{...proposal,status,text:'Поздний ответ'}],0,state));
      assert.equal(state.draft,draft);assert.deepEqual(state.history,['undo']);assert.deepEqual(state.redo,['redo']);
    }
    const saved=normalizeMvpItem({...item,draft,draftEdited:true},[proposal]);
    assert.equal(saved.draft,draft,'server-confirmed intentional clear never revives historical text');
  }
});

test('UNKNOWN remains held through text edits, explicit confirmation and newer current proposals',()=>{
  const unknown={...operation,status:'unknown'};
  const normalized=normalizeMvpItem(item,[{...proposal,status:'unknown'},{...proposal,id:'new',status:'draft',text:'Новый ответ'}],0,{},[unknown]);
  const state={...normalized.initialState,draft:'Ручная правка',manualEdited:true,_staleGenerated:false};
  assert.equal(normalized.draft,'Новый ответ');assert.equal(normalized.view,'attention');
  assert.equal(replyReadiness(normalized,state).disabled,true);
  const send={},close={},status={dataset:{}};
  updateMediaActionControls({querySelector:selector=>({'.composer .send-button':send,'#close-comment':close,'#draft-status':status})[selector]},normalized,state);
  assert.equal(send.disabled,true);assert.equal(close.disabled,true);
});

test('held historical text never autosaves, creates proposals or retries without new operator review',async t=>{
  const previous=globalThis.fetch;let calls=0;
  globalThis.fetch=async()=>{calls++;throw Error('unexpected network');};t.after(()=>{globalThis.fetch=previous;});
  for(const status of ['failed','unknown']) {
    const normalized=normalizeMvpItem(item,[{...proposal,status}],0,{},[{...operation,status}]);
    const state=structuredClone(normalized.initialState),saved={items:{[item.id]:state}},data={items:[normalized]};
    const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:()=>state});t.after(()=>connection.stop());
    await connection.saveDraft(normalized);
    await assert.rejects(connection.prepareReply(normalized),status==='unknown'?/проверить результат/:/Сначала проверьте/);
    if(status==='unknown') {
      state.manualEdited=true;state._staleGenerated=false;state.draft='Ручной ответ';
      await assert.rejects(connection.prepareReply(normalized),/проверить результат/);
      await assert.rejects(connection.closeOne(normalized),/проверить результат/);
      await assert.rejects(connection.closeMany([normalized]),/проверить результат/);
    }
  }
  assert.equal(calls,0);
});

test('fresh operator state and old untouched derived state show the same durable held reply',()=>{
  const held=normalizeMvpItem(item,[proposal],0,{},[operation]);
  const old=structuredClone(normalizeMvpItem(item,[{...proposal,status:'draft'}]).initialState);
  mergeMvpItemState(old,held);
  assert.equal(old.draft,held.initialState.draft);assert.equal(old._staleGenerated,true);
  assert.equal(old.view,'attention');assert.deepEqual(old._preparationDisposition,held.preparationDisposition);
});

test('actual composer renders retained failure explanation and UNKNOWN disables confirmation/close',()=>{
  const source=readFileSync(new URL('../workshop/app.js',import.meta.url),'utf8');
  const renderer=source.slice(source.indexOf('function composerHtml(item)'),source.indexOf('function updateDecisionReadiness()'));
  for(const outcome of ['failed','unknown']) {
    const normalized=normalizeMvpItem(item,[{...proposal,status:outcome}],0,{},[{...operation,status:outcome}]);
    const state={...normalized.initialState,draftContext:0};
    const render=new Function('stateFor','messagesFor','contextVersion','isOpen','mediaPreparationHold','replyReadiness','replyFor','recordFor','icon','esc','emojiButton','sendButton',`${renderer};return composerHtml;`)(
      ()=>state,()=>[{id:undefined,author:'Адресат'}],()=>0,()=>true,()=>null,replyReadiness,()=>null,()=>null,()=>'',
      value=>String(value||''),()=>'',(_kind,disabled)=>`<button class="send-button" ${disabled?'disabled':''}>Отправить</button>`);
    const html=render(normalized);
    assert.match(html,new RegExp(normalized.preparationDisposition.label));
    assert.match(html,/Проверить историю действий/);assert.match(html,new RegExp(proposal.text));
    if(outcome==='failed')assert.match(html,/актуальный комментарий/);
    else {assert.match(html,/id="confirm-saved-draft" disabled/);assert.match(html,/id="close-comment"[^>]*disabled/);}
  }
});

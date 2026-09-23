import {bindEmojiPicker,emojiButton} from './emoji-picker.js';
import {assistantDraftCandidates,candidateDraftPatch} from './assistant-candidates.js';
import {startsFreshDiscussion} from './assistant-intent.js';
// Local MVP bridge for the original workshop surface. No social action is sent
// except from the explicit final button in the exact-proposal review dialog.
import {captureChatReading,restoreChatReading} from './assistant-reading.js';
import {publicationTitle} from './workspace-presentation.js';
import {mediaPreparationHold,assertMediaReady,reviewMediaHold} from './preparation-readiness.js';
import {activeInstructions,chronologicalHistory} from './active-instructions.js';
import {postTranscripts} from './post-transcripts.js';
import {deriveClosure} from './closure-outcomes.js';
import {assistantText} from './chat-text.js';
import {createAssistantRunTracker} from './assistant-run-tracker.js';
import {canRequestWorkspaceDelta,mergeWorkspaceDelta,createWorkspaceChangeTracker} from './workspace-delta.js';
const csrfHeader = token => ({'Content-Type':'application/json','X-CSRF-Token':token});
const asText = value => value == null ? '' : String(value);
const ordinaryText = value => asText(value).replace(/Angry[.\s]*Space/gi,'сервис').replace(/у провайдера/gi,'в соцсети');
const labels = {attention:'Нужно участие',prepared:'Подготовлено',waiting:'Ждём',closed:'Закрыто'};
const actionLabels = {reply_and_close:'Ответить и закрыть',close:'Закрыть без ответа'};
const jobLabels = {sync:'Синхронизация',assistant:'Ответ ассистента',materials:'Импорт материалов',media:'Обработка медиа',execute:'Выполнение',reconcile:'Сверка'};
const statusLabels = {queued:'В очереди',running:'Выполняется',completed:'Завершено',failed:'Ошибка',cancelled:'Отменено',interrupted:'Прервано',unknown:'Исход неизвестен',succeeded:'Подтверждено',dispatching:'Отправляется',approved:'Одобрено',draft:'Черновик'};
const time = value => {const date=new Date(value);return Number.isFinite(date.getTime())?date.toLocaleString('ru-RU',{dateStyle:'short',timeStyle:'short'}):'Время не указано';};
const plain = value => {
  const raw=asText(value);if(!raw)return '';
  const text=raw.replace(/<\s*br\s*\/?\s*>/gi,'\n').replace(/<\s*\/\s*(?:p|div|li)\s*>/gi,'\n').replace(/<\/?[a-z][^>]*>/gi,' ');
  const decoder=document.createElement('textarea');decoder.innerHTML=text;
  return decoder.value.replace(/\u00a0/g,' ').replace(/[\t ]+\n/g,'\n').trim();
};
const sourceUrl = value => /^https?:\/\//i.test(asText(value)) ? asText(value) : '';

export function freezeAssistantContext(context = {}) {
  return Object.freeze({...context,itemIds:Object.freeze([...new Set((context.itemIds||[context.itemId]).filter(id=>typeof id==='string'&&id))])});
}
export function currentContextProposals(snapshot, context) {
  const ids=new Set(context.itemIds),items=new Map((snapshot?.items||[]).map(item=>[item.id,item])),seen=new Set();
  return [...(snapshot?.proposals||[])].reverse().filter(proposal=>{
    const item=items.get(proposal.itemId);
    if(!ids.has(proposal.itemId)||seen.has(proposal.itemId)||!item||item.workflow==='closed'||proposal.status!=='draft'
      ||proposal.itemRevision!==item.revision||proposal.contextEvidenceDigest!==item.contextEvidenceDigest||proposal.branchContextDigest!==item.branchContextDigest)return false;
    seen.add(proposal.itemId);return true;
  }).reverse();
}
export const submitsAssistantMessage = event => event.key==='Enter'&&!event.shiftKey&&!event.isComposing&&event.keyCode!==229;
// Durable failure receipts survive a reload without reviving a submitted run.
export function discussionFailures(snapshot,convo) {
  const actor=snapshot?.operator?.id;if(!actor||!convo?.id)return [];
  const messages=new Map((convo.messages||[]).filter(message=>message.role==='user'&&typeof message.id==='string'&&typeof message.text==='string').map(message=>[message.id,message]));
  const answered=new Set((convo.messages||[]).filter(message=>message.role==='assistant').map(message=>message.prepareRunId));
  const seen=new Set(),failures=[];
  for(const job of [...(snapshot.jobs||[])].reverse()){
    if(typeof job.id!=='string'||!job.id||job.kind!=='assistant'||job.purpose!=='discussion'||job.operatorId!==actor||job.refId!==convo.id||!messages.has(job.sourceUserMessageId)||seen.has(job.sourceUserMessageId))continue;
    seen.add(job.sourceUserMessageId);
    if(['failed','interrupted','cancelled'].includes(job.status)&&!answered.has(job.id))failures.push({job,message:messages.get(job.sourceUserMessageId)});
  }
  return failures.reverse();
}
export function normalizeMvpItem(item, proposals = [], now = Date.now(), localState = {}) {
  const proposal=[...proposals].reverse().find(p=>p.itemId===item.id&&['draft','approved','dispatching'].includes(p.status)
    &&p.itemRevision===item.revision&&p.contextEvidenceDigest===item.contextEvidenceDigest&&p.branchContextDigest===item.branchContextDigest);
  const savedProposals=!proposal?[...proposals].reverse().filter(p=>p.itemId===item.id&&p.kind==='reply_and_close'&&p.text&&['stale','draft','approved','dispatching'].includes(p.status)):[];
  const previousProposal=savedProposals.find(p=>p.id===item.autoPreparation?.savedProposalId)||savedProposals[0];
  const displayedProposal=proposal||previousProposal;
  const serverWorkflow=item.workflow||item.view||'attention',serverDraft=item.draft||'';
  const stalePrepared=serverWorkflow==='prepared'&&!item.draftEdited&&item.autoPreparation?.status==='prepared'&&!serverDraft.trim()&&!proposal;
  const mediaHold=mediaPreparationHold(item);
  const workflow=stalePrepared||mediaHold&&serverWorkflow==='prepared'?'attention':serverWorkflow;
  const decision=proposal?.kind==='close'?'no_reply':'reply';
  const derivedDraft=!item.draftEdited&&!serverDraft&&displayedProposal?.kind==='reply_and_close'?displayedProposal.text||'':null;
  const staleGenerated=!!previousProposal&&derivedDraft!==null;
  const draft=serverDraft||derivedDraft||'';
  const preparation=item.autoPreparation;
  const created=Date.parse(item.createdAt),observed=Date.parse(item.providerObservedAt);
  const manualDraft=!!item.draftEdited||!!serverDraft||!!localState.manualEdited||!!localState.draft&&localState.draft!==localState._derivedDraft;
  const awaiting=!manualDraft&&workflow==='attention'&&['new','inprogress'].includes(item.providerStatus)
    &&(!Number.isFinite(created)||created<=now);
  const pendingLabel=awaiting?(!Number.isFinite(observed)||now-observed>10*60*1000?'Проверяем статус':'Ожидает разбора'):'';
  const preparationLabel=mediaHold?mediaHold.label:staleGenerated?'Сохранённый ответ требует проверки':stalePrepared?'Ответ требует повторной подготовки':preparation?.status==='queued'?(awaiting?'Ожидает разбора':''):
    ({running:'Ассистент готовит решение…',prepared:'Решение подготовлено',stale:'Сохранённое решение требует проверки',needs_attention:'Нужно участие оператора',error:'Не удалось подготовить решение'})[preparation?.status]||(!preparation?pendingLabel:'');
  const reason=ordinaryText(preparation?.reason||item.reason||item.contextNote||'Решение ещё не выбрано.');
  const note=mediaHold?mediaHold.detail:ordinaryText(staleGenerated?['Сохранённый ответ требует проверки',preparation?.sourceChangeReason||'Текст сохранён'].join(' · '):stalePrepared?'Ответ требует повторной подготовки · Обсуждение изменилось':preparationLabel?[preparationLabel,preparation?.reason].filter(Boolean).join(' · '):'');
  return {...item,view:workflow,decision,reason,contextNote:note||ordinaryText(item.contextNote),
    attentionLabel:preparationLabel?note:ordinaryText(item.attentionLabel),draft,suggestions:[],
    initialState:{view:workflow,decision,draft,_serverRevision:Number(item.revision)||0,_serverDraft:serverDraft,_serverDraftEdited:!!item.draftEdited,
      _sourceProposalId:item.draftOrigin?.sourceProposalId||item.draftOrigin?.id||displayedProposal?.origin?.id||displayedProposal?.id||null,_sourceProposalRevision:item.draftOrigin?.sourceProposalRevision??item.draftOrigin?.revision??displayedProposal?.origin?.revision??displayedProposal?.revision??null,
      _sourceProposalKind:item.draftOrigin?.kind||displayedProposal?.origin?.kind||displayedProposal?.kind||null,
      _displayedProposalId:proposal?.id||null,
      _draftSessionId:item.draftOrigin?.draftSessionId||item.draftSessionId||null,
      _derivedDraft:derivedDraft,_staleGenerated:staleGenerated,_serverDecision:decision,_preparationNote:note,note,revision:0,history:[],redo:[],chat:[],proposal:null}};
}

export function mergeMvpItemState(state, item) {
  const incoming=item.initialState;
  // A bootstrap begun before an autosave may arrive after its acknowledged write.
  // Never roll the baseline back or turn an old empty draft into a local clear.
  if(Number(incoming._serverRevision)<Number(state._serverRevision))return state;
  const oldServerDraft=state._serverDraft??incoming._serverDraft;
  const isDerived=state._derivedDraft!=null&&state.draft===state._derivedDraft&&!state.manualEdited;
  const hasLocalDraft=!isDerived&&(state.draft!==oldServerDraft||state.manualEdited&&state.draft==='');
  state._serverRevision=incoming._serverRevision;
  state._serverDraft=incoming._serverDraft;
  state._serverDraftEdited=incoming._serverDraftEdited;
  if(!hasLocalDraft){
    state.draft=item.draft;state._derivedDraft=incoming._derivedDraft;state._staleGenerated=incoming._staleGenerated;
    if(state._sourceProposalId!==incoming._sourceProposalId||state._sourceProposalRevision!==incoming._sourceProposalRevision)state._draftSessionId=incoming._draftSessionId;
    state._sourceProposalId=incoming._sourceProposalId;state._sourceProposalRevision=incoming._sourceProposalRevision;
    state._sourceProposalKind=incoming._sourceProposalKind;
    state._displayedProposalId=incoming._displayedProposalId;
  }
  if(state.decision===(state._serverDecision??state.decision))state.decision=item.decision;
  state._serverDecision=item.decision;
  state.view=item.view;
  if(!state.note||state.note===state._preparationNote)state.note=incoming.note;
  state._preparationNote=incoming.note;
  return state;
}

export function createMvpConnection(hooks) {
  const assistantRuns=createAssistantRunTracker();
  const chatReading=new Map();
  let boundChat=null, boundChatKey=null, glassInputObserver=null;
  function rememberAssistantReading(){
    if(hooks.getSaved().ai===false||document.querySelector('#shell')?.classList?.contains('assistant-morphing'))return;
    const reading=captureChatReading(boundChat);
    if(reading&&boundChatKey!==null)chatReading.set(boundChatKey,reading);
  }
  function restoreAssistantReading(){restoreChatReading(boundChat,chatReading.get(boundChatKey));}
  let snapshot=null,events=null,poll=null,refreshing=null,dialog=null,sendingAssistant=false,assistantSubmission=null,refreshReviewReadiness=null;
  let instructionCatalog=null,instructionStatus='idle',instructionPending=null,instructionRequested=false;
  let detectChanges=createWorkspaceChangeTracker(),pendingPaint=false,refreshTimer=null,burstStarted=null,pendingRefresh=false,pendingRepaint=false,stopped=false;
  let appliedWorkspaceVersion=null,versionEndpoint=true,deltaEndpoint=true,pendingForce=false;
  let projectedPostsSource=null,projectedMaterialsSource=null,projectedAccount=null,projectedPosts=null;
  let projectedBranchesSource=null,projectedBranches=null;
  const draftTimers=new Map(),draftInFlight=new Map(),candidateInFlight=new Map(),chosen=new Set();
  let assistantChatEpoch=0,actorEpoch=0,sessionInvalidated=false,assistantProgressSignature=null;
  const {esc,icon}=hooks;
  const notify = message => hooks.announce?.(ordinaryText(message));
  const selected = () => hooks.selectedItem?.() || hooks.getData()?.items?.find(item=>item.id===hooks.getSaved()?.selected);
  const uniqueId=()=>globalThis.crypto.randomUUID();
  function requireActor(epoch){if(epoch!==actorEpoch)throw new Error('Пользователь изменился. Повторите действие в новой сессии.');}
  function acceptActor(raw){
    const actorId=raw.operator?.id,saved=hooks.getSaved?.();
    const previous=snapshot?.operator?.id||saved?.mvpActorId||hooks.operator?.id;
    const changed=!!actorId&&!!previous&&actorId!==previous;
    if(changed){
      actorEpoch++;assistantChatEpoch++;sendingAssistant=false;assistantSubmission=null;
      assistantRuns.clear();assistantProgressSignature=null;
      for(const timer of draftTimers.values())clearTimeout(timer);
      draftTimers.clear();draftInFlight.clear();candidateInFlight.clear();chosen.clear();
      chatReading.clear();boundChat=null;boundChatKey=null;
      glassInputObserver?.disconnect();glassInputObserver=null;dialog?.remove();dialog=null;
      instructionCatalog=null;instructionStatus='idle';instructionPending=null;
      detectChanges=createWorkspaceChangeTracker();pendingPaint=true;
      projectedPostsSource=null;projectedMaterialsSource=null;projectedPosts=null;
      projectedBranchesSource=null;projectedBranches=null;
      // The production app is bound to one actor/storage key per page. Its hook
      // preserves that actor's edits, hides the old UI and reloads the session.
      const boundActor=snapshot?.operator?.id||hooks.operator?.id||previous;
      if(actorId!==boundActor&&hooks.onActorChange?.(raw.operator)===true){stopped=true;sessionInvalidated=true;throw new Error('Пользователь изменился. Рабочее место перезагружается.');}
      if(saved){
        for(const key of ['assistantSession','assistantContexts','mvpAiInput','mvpConversationId','mvpAssistantSubmission','mvpFeedbackSessionId','mvpFeedbackOutbox','mvpPresented',
          'retainedSelection','overviewTopic','overviewInputs','overviewInstructions','overviewPreviews','overviewArrivalAt','queueArrivals','queueScroll','overviewScroll','exercise'])delete saved[key];
        saved.items={};saved.branches={};saved.selected=null;
      }
    }
    if(actorId&&saved){saved.mvpActorId=actorId;if(changed)hooks.persist?.();}
    return changed;
  }
  function lineage(item){
    const state=hooks.stateFor(current(item)),saved=hooks.getSaved();
    saved.mvpFeedbackSessionId||=uniqueId();state._draftSessionId||=uniqueId();hooks.persist?.();
    return {sessionId:saved.mvpFeedbackSessionId,draftSessionId:state._draftSessionId,
      ...(state._sourceProposalId&&Number.isInteger(state._sourceProposalRevision)?{sourceProposalId:state._sourceProposalId,sourceProposalRevision:state._sourceProposalRevision}:{})};
  }
  let flushingFeedback=false;
  async function flushFeedback(){
    if(flushingFeedback||!snapshot?.csrfToken||stopped)return;
    flushingFeedback=true;const epoch=actorEpoch;
    try{
      const saved=hooks.getSaved();
      for(const entry of [...(saved.mvpFeedbackOutbox||[])]){
        if(epoch!==actorEpoch)return;
        try{await api('/api/feedback/events','POST',entry);}
        catch(error){if(!error.status||error.status>=500||[408,429].includes(error.status))break;}
        if(epoch!==actorEpoch)return;
        saved.mvpFeedbackOutbox=saved.mvpFeedbackOutbox.filter(row=>row.eventId!==entry.eventId);hooks.persist?.();
      }
    }finally{flushingFeedback=false;}
  }
  function trackPresented(){
    const item=selected(),editor=document.querySelector('#draft'),closeSurface=document.querySelector('.composer .no-reply');
    if(document.visibilityState!=='visible'||!item)return;
    const state=hooks.stateFor(current(item));
    // Count only an actual AI text rendered in the selected composer, never queue rows or manual drafts.
    const replyShown=editor?.getClientRects().length&&state._derivedDraft!=null&&editor.value===state._derivedDraft;
    const closeShown=state._sourceProposalKind==='close'&&state.decision==='no_reply'&&!state.editorCollapsed&&closeSurface?.getClientRects().length;
    if(state.manualEdited||state._serverDraftEdited||state._displayedProposalId!==state._sourceProposalId||(!replyShown&&!closeShown))return;
    const origin=lineage(item);if(!origin.sourceProposalId)return;
    const saved=hooks.getSaved(),key=JSON.stringify([item.id,origin.sourceProposalId,origin.sourceProposalRevision,origin.draftSessionId]);
    saved.mvpPresented||={};
    if(!saved.mvpPresented[key]){
      const eventId=uniqueId();saved.mvpPresented[key]=eventId;saved.mvpFeedbackOutbox||=[];
      saved.mvpFeedbackOutbox.push({eventId,type:'proposal_presented',itemId:item.id,...origin});hooks.persist?.();
    }
    void flushFeedback();
  }

  async function api(path,method='GET',body) {
    if(sessionInvalidated)throw new Error('Пользователь изменился. Рабочее место перезагружается.');
    const epoch=actorEpoch;
    const response=await fetch(path,{method,cache:'no-store',credentials:'same-origin',headers:method==='GET'?undefined:csrfHeader(snapshot?.csrfToken||''),...(body===undefined?{}:{body:JSON.stringify(body)})});
    const result=await response.json().catch(()=>({}));
    requireActor(epoch);
    if(!response.ok){const error=new Error(result.error||`Ошибка ${response.status}`);error.status=response.status;throw error;}
    return result;
  }
  async function loadInstructions({repaint=true}={}) {
    instructionRequested=true;
    if(instructionPending)return instructionPending;
    instructionStatus='loading';
    const epoch=actorEpoch;
    instructionPending=(async()=>{
      try{const catalog=await api('/api/knowledge/instructions');if(epoch===actorEpoch){instructionCatalog=catalog;instructionStatus='ready';}}
      catch{if(epoch===actorEpoch){instructionCatalog=null;instructionStatus='error';}}
      finally{if(epoch===actorEpoch)instructionPending=null;}
      if(repaint&&!stopped&&epoch===actorEpoch)hooks.render?.();
    })();
    return instructionPending;
  }
  function instructionContext(postId) {
    if(instructionStatus==='idle')void loadInstructions();
    const post=snapshot?.posts?.find(row=>row.id===postId);
    const branchIds=new Set((snapshot?.branches||[]).filter(branch=>branch.postId===postId).map(branch=>branch.id));
    const itemKeys=(snapshot?.items||[]).filter(item=>item.postId===postId||branchIds.has(item.branchId)).map(item=>item.postKey);
    const selected=instructionStatus==='ready'?activeInstructions(instructionCatalog,{account:snapshot?.connectorBinding?.accountId||snapshot?.account,postKeys:[post?.postKey,...itemKeys],posts:snapshot?.posts||[],connectorBinding:snapshot?.connectorBinding}):null;
    return {status:instructionStatus==='ready'&&!selected?'error':instructionStatus,...(selected||{global:[],post:[]})};
  }
  function normalized(raw) {
    const proposals=raw.proposals||[];
    const byItem=new Map();
    for(const proposal of proposals){
      if(!byItem.has(proposal.itemId))byItem.set(proposal.itemId,[]);
      byItem.get(proposal.itemId).push(proposal);
    }
    const now=Date.now();
    const localItems=hooks.getSaved?.()?.items||{};
    const items=(raw.items||[]).map(item=>normalizeMvpItem(item,byItem.get(item.id)||[],now,localItems[item.id]));
    const sourcePosts=raw.posts||[],materials=raw.materials||[];
    if(!projectedPosts||sourcePosts!==projectedPostsSource||materials!==projectedMaterialsSource||raw.account!==projectedAccount){
      projectedPosts=sourcePosts.map(post=>({...post,title:publicationTitle({title:plain(post.title),text:plain(post.text),excerpt:plain(post.excerpt)}),excerpt:plain(post.excerpt||post.text).slice(0,180),text:plain(post.text),channel:post.channel||'LikeAvto',mediaNote:post.mediaNote||''}));
      for(const post of projectedPosts)post.transcripts=postTranscripts(post,sourcePosts,materials,raw.account);
      projectedPostsSource=sourcePosts;projectedMaterialsSource=materials;projectedAccount=raw.account;
    }
    const sourceBranches=raw.branches||[];
    if(!projectedBranches||sourceBranches!==projectedBranchesSource){
      projectedBranches=sourceBranches.map(branch=>({...branch,messages:(branch.messages||[]).map(message=>({...message,parentId:message.parentId||null,author:plain(message.author)||'Автор неизвестен',text:plain(message.text),time:message.time||time(message.createdAt)}))}));
      projectedBranchesSource=sourceBranches;
    }
    const posts=projectedPosts,branches=projectedBranches;
    return {items,posts,branches,exercises:[]};
  }
  function apply(raw,{repaint=true}={}) {
    const actorChanged=acceptActor(raw);
    snapshot=raw;
    refreshReviewReadiness?.();
    restoreAssistantSubmission(raw);
    void flushFeedback();
    const data=hooks.getData?.();if(!data){const next=normalized(raw);appliedWorkspaceVersion=raw.workspaceVersion??null;return next;}
    const {dataChanged,uiChanged}=detectChanges(raw,instructionCatalog,instructionStatus);
    const previouslyPendingPaint=pendingPaint;
    let next=data;
    if(dataChanged){
    next=normalized(raw);data.items=next.items;data.posts=next.posts;data.branches=next.branches;data.exercises=[];
    const saved=hooks.getSaved();
    saved.items||={};saved.branches||={};
    if(saved.mvpConversationId===undefined){saved.mvpConversationId='';hooks.persist?.();}
    for(const item of next.items){
      const state=hooks.stateFor(item);
      mergeMvpItemState(state,item);
      if(item.workflow==='closed'){
        state.closure=deriveClosure(item,raw.branches||[],raw.operations||[]);
      }
    }
    if(saved.selected&&!next.items.some(item=>item.id===saved.selected))saved.selected=null;
    hooks.updateDecisionReadiness?.();
    }
    const run=assistantRuns.current();
    let progressChanged=false;
    let assistantNavigated=false;
    if(run){
      const job=raw.jobs?.find(row=>row.id===run.jobId);
      const progressSignature=JSON.stringify([run.jobId,job?.status,job?.toolResults,job?.error]);
      if(progressSignature!==assistantProgressSignature){assistantProgressSignature=progressSignature;progressChanged=true;}
      const convo=raw.conversations?.find(row=>row.id===run.conversationId);
      const message=convo?.messages?.find(row=>row.role==='assistant'&&row.prepareRunId===run.jobId);
      if(job?.status==='completed'&&message&&document.activeElement?.matches?.('#draft,.mvp-proposal-edit'))assistantRuns.clear();
      else {
        const destination=assistantRuns.navigation({conversationId:hooks.getSaved()?.mvpConversationId||'',operatorId:raw.operator?.id,
          navigationRevision:hooks.assistantNavigationRevision?.(),screenKey:hooks.assistantScreenKey?.(),job,message});
        if(destination&&hooks.navigateAssistant?.(destination)){assistantNavigated=true;pendingPaint=false;}
      }
    }
    pendingPaint=assistantNavigated?false:pendingPaint||dataChanged||uiChanged;
    if(!assistantNavigated&&progressChanged&&!dataChanged&&!uiChanged&&!previouslyPendingPaint&&repaint&&!actorChanged){
      if(!patchAssistantProgress())pendingPaint=true;
    }else if(progressChanged&&!assistantNavigated)pendingPaint=true;
    if(actorChanged){hooks.render?.();pendingPaint=false;}
    else if(repaint&&pendingPaint){
      const active=document.activeElement;
      if(active?.matches?.('#draft,.mvp-proposal-edit'))notify('Данные обновлены. Ваш ввод сохранён на экране.');
      else if(active?.id==='ai-input'){
        const start=active.selectionStart,end=active.selectionEnd;
        hooks.render?.({focusControl:'#ai-input'});
        pendingPaint=false;
        requestAnimationFrame(()=>{const input=document.querySelector('#ai-input');if(input){input.focus({preventScroll:true});input.setSelectionRange(start,end);}});
      }else {hooks.render?.();pendingPaint=false;}
    }
    appliedWorkspaceVersion=raw.workspaceVersion??null;
    return next;
  }
  async function load(){const raw=await api('/api/bootstrap');acceptActor(raw);snapshot=raw;restoreAssistantSubmission(raw,{recover:true});return normalized(raw);}
  function hydrate(raw=snapshot,options){if(!raw)return null;return apply(raw,options);}
  async function readWorkspace(){
    if(deltaEndpoint&&canRequestWorkspaceDelta(snapshot)){
      const base=snapshot;
      let response;
      try{response=await api(`/api/bootstrap/delta?since=${encodeURIComponent(base.workspaceVersion)}`);}
      catch(error){
        if([404,405,501].includes(error.status))deltaEndpoint=false;
        else throw error;
      }
      if(response){
        try{
          // A hydrate/load may have replaced the base while the request waited.
          if(snapshot===base)return mergeWorkspaceDelta(base,response);
        }catch{/* Invalid or stale protocol data requires a complete snapshot. */}
      }
    }
    return api('/api/bootstrap');
  }
  async function refresh(options){
    if(stopped)return;
    if(options?.background&&globalThis.document?.visibilityState==='hidden')return;
    if(refreshing){pendingRefresh=true;pendingForce ||= !options?.background;pendingRepaint ||= options?.repaint!==false;return refreshing;}
    clearTimeout(refreshTimer);refreshTimer=null;burstStarted=null;
    refreshing=(async()=>{
      let result,nextOptions=options;
      do{
        pendingRefresh=false;pendingForce=false;
        if(nextOptions?.background){
          if(globalThis.document?.visibilityState==='hidden')return result;
          if(appliedWorkspaceVersion!==null&&versionEndpoint){
            let version;
            try{version=await api('/api/workspace-version');}
            catch(error){if(error.status===404)versionEndpoint=false;else throw error;}
            if(stopped||globalThis.document?.visibilityState==='hidden')return result;
            if(version?.workspaceVersion!==undefined&&version.workspaceVersion===appliedWorkspaceVersion
              &&(!version.actorId||version.actorId===snapshot?.operator?.id)
              &&(version.csrfToken===undefined||version.csrfToken===snapshot?.csrfToken)){
              if(pendingPaint&&nextOptions.repaint!==false)result=apply(snapshot,nextOptions);
              nextOptions={repaint:pendingRepaint,background:!pendingForce};pendingRepaint=false;
              continue;
            }
          }
        }
        const raw=await readWorkspace();
        if(instructionRequested)await loadInstructions({repaint:false});
        if(stopped)return result;
        result=apply(raw,nextOptions);
        nextOptions={repaint:pendingRepaint,background:!pendingForce};pendingRepaint=false;
      }while(pendingRefresh&&!stopped);
      return result;
    })();
    try{return await refreshing;}finally{refreshing=null;if(pendingRefresh&&!stopped)scheduleRefresh();}
  }
  function scheduleRefresh(){
    if(stopped||globalThis.document?.visibilityState==='hidden')return;
    if(refreshing){pendingRefresh=true;pendingRepaint=true;return;}
    const now=Date.now();burstStarted??=now;
    clearTimeout(refreshTimer);
    refreshTimer=setTimeout(()=>{refreshTimer=null;burstStarted=null;refresh({background:true}).catch(error=>notify(error.message));},Math.min(250,Math.max(0,1000-(now-burstStarted))));
  }
  function visibilityChanged(){
    if(globalThis.document?.visibilityState==='hidden'){
      if(poll)clearInterval(poll);poll=null;
      clearTimeout(refreshTimer);refreshTimer=null;burstStarted=null;
    }else if(!stopped){
      if(!poll)poll=setInterval(scheduleRefresh,15000);
      trackPresented();scheduleRefresh();
    }
  }
  function start(){
    if(events)return;
    stopped=false;
    globalThis.document?.addEventListener?.('visibilitychange',visibilityChanged);
    events=new EventSource('/api/events');events.addEventListener('refresh',scheduleRefresh);
    if(globalThis.document?.visibilityState!=='hidden')poll=setInterval(scheduleRefresh,15000);
  }
  function stop(){assistantRuns.clear();assistantProgressSignature=null;glassInputObserver?.disconnect();glassInputObserver=null;stopped=true;globalThis.document?.removeEventListener?.('visibilitychange',visibilityChanged);events?.close();events=null;if(poll)clearInterval(poll);poll=null;clearTimeout(refreshTimer);refreshTimer=null;burstStarted=null;pendingRefresh=false;pendingRepaint=false;pendingForce=false;for(const timer of draftTimers.values())clearTimeout(timer);draftTimers.clear();}
  async function guarded(work,success){try{const result=await work();if(success)notify(success);return result;}catch(error){notify(error.message||'Операция не выполнена');throw error;}}
  function current(item){return hooks.getData()?.items?.find(entry=>entry.id===item.id)||item;}
  function serverRevision(item){return hooks.stateFor(current(item))._serverRevision??current(item).revision;}

  async function saveDraft(item){
    if(!item)return null;
    const epoch=actorEpoch;
    const replacing=candidateInFlight.get(item.id);if(replacing)await replacing;
    clearTimeout(draftTimers.get(item.id));draftTimers.delete(item.id);
    const running=draftInFlight.get(item.id);if(running)await running;
    requireActor(epoch);
    const target=current(item),state=hooks.stateFor(target),text=state.draft||'';
    if(text===state._serverDraft&&(!state.manualEdited||state._serverDraftEdited) || !state.manualEdited&&state._derivedDraft!=null&&text===state._derivedDraft)return target;
    const identity=lineage(target);
    const pending=state._pendingDraftSave;
    const body=pending?.draft===text?pending:{expectedRevision:serverRevision(target),draft:text,...identity,eventId:uniqueId()};
    state._pendingDraftSave=body;hooks.persist?.();
    const task=guarded(async()=>{
      let updated;
      try{updated=await api(`/api/items/${encodeURIComponent(item.id)}`,'PATCH',body);}
      catch(error){if(error.status>=400&&error.status<500&&![408,429].includes(error.status)){delete state._pendingDraftSave;hooks.persist?.();}throw error;}
      if(Number(updated.revision)>=Number(state._serverRevision)){
        state._serverRevision=updated.revision;state._serverDraft=updated.draft||'';state._derivedDraft=null;state._serverDraftEdited=true;state._staleGenerated=false;
      }
      delete state._pendingDraftSave;hooks.persist?.();
      await refresh({repaint:false});
      requireActor(epoch);
      return updated;
    },'Черновик сохранён');
    draftInFlight.set(item.id,task);
    try{return await task;}finally{if(epoch===actorEpoch)draftInFlight.delete(item.id);}
  }
  function scheduleDraft(item){if(!item)return;clearTimeout(draftTimers.get(item.id));draftTimers.set(item.id,setTimeout(()=>saveDraft(item).catch(()=>{}),750));}
  async function changeWorkflow(item,workflow,extra={}){
    if(!item||!['attention','prepared','waiting'].includes(workflow))return;
    const epoch=actorEpoch;
    return guarded(async()=>{await saveDraft(item);requireActor(epoch);const updated=await api(`/api/items/${encodeURIComponent(item.id)}`,'PATCH',{expectedRevision:serverRevision(item),workflow,...extra,...lineage(item),eventId:uniqueId()});await refresh();return updated;},`Комментарий в «${labels[workflow]}»`);
  }
  async function createProposal(item,kind,text){
    if(!item)return null;
    assertMediaReady(current(item));
    const epoch=actorEpoch;
    const visible=hooks.stateFor(current(item));
    if(kind==='reply_and_close'&&visible._staleGenerated&&!visible.manualEdited)throw new Error('Сначала проверьте сохранённый ответ или отредактируйте его.');
    if(kind==='reply_and_close'){await saveDraft(item);requireActor(epoch);text=hooks.stateFor(current(item)).draft||'';if(!text.trim())throw new Error('Для ответа нужен текст черновика.');}
    const target=current(item),state=hooks.stateFor(target),proposalText=kind==='close'?'':text;
    assertMediaReady(target);
    const pending=state._pendingProposal;
    const body=pending?.kind===kind&&pending.text===proposalText?pending:{itemId:target.id,kind,text:proposalText,expectedRevision:serverRevision(target),...lineage(target),eventId:uniqueId()};
    state._pendingProposal=body;hooks.persist?.();
    let proposal;
    try{proposal=await api('/api/proposals','POST',body);}
    catch(error){if(error.status>=400&&error.status<500&&![408,429].includes(error.status)){delete state._pendingProposal;hooks.persist?.();}throw error;}
    delete state._pendingProposal;hooks.persist?.();
    await refresh({repaint:false});return proposal;
  }
  async function prepareReply(item){return guarded(async()=>{const proposal=await createProposal(item,'reply_and_close');hooks.render?.();review([proposal.id]);return proposal;},'Ответ подготовлен. Проверьте его перед отправкой.');}
  async function closeOne(item){return guarded(async()=>{const proposal=await createProposal(item,'close','');hooks.render?.();review([proposal.id]);return proposal;});}
  async function closeMany(items,kind='close'){
    const targets=[...new Map(items.map(item=>[item.id,item])).values()];
    if(!targets.length)return;
    if(targets.length>50)throw new Error('За один раз можно выбрать до 50 комментариев.');
    for(const item of targets)assertMediaReady(current(item));
    const created=[];
    try{for(const item of targets)created.push(await createProposal(item,kind,kind==='reply_and_close'?hooks.stateFor(item).draft:''));}
    catch(error){await refresh();notify(`Группа подготовлена частично: ${created.length} из ${targets.length}. ${error.message}`);return;}
    hooks.render?.();review(created.map(row=>row.id));
  }
  function proposalRows(ids){return (snapshot?.proposals||[]).filter(row=>ids.includes(row.id)&&row.status==='draft');}
  function review(ids){
    dialog?.remove();dialog=document.createElement('dialog');dialog.className='closure-dialog mvp-dialog mvp-review';
    const rows=proposalRows(ids);const unique=new Set(rows.map(row=>row.itemId)).size===rows.length;
    const mediaHold=reviewMediaHold(rows,snapshot?.items||[]);
    const valid=rows.length===ids.length&&rows.length>0&&rows.length<=50&&unique&&!mediaHold;
    const cards=rows.map(row=>{const item=snapshot.items.find(i=>i.id===row.itemId);return `<article class="mvp-review-card"><div><strong>${esc(plain(item?.author||item?.title)||'Комментарий')}</strong><span>${esc(actionLabels[row.kind]||row.kind)}</span></div><p>${esc(plain(item?.text||item?.preview)||'Комментарий недоступен')}</p>${row.kind==='reply_and_close'?`<blockquote>${esc(row.text)}</blockquote>`:''}<small>${esc(time(item?.createdAt))}</small></article>`;}).join('');
    dialog.innerHTML=`<header><h2>Проверить действия</h2><button type="button" class="icon-button" data-cancel aria-label="Закрыть">${icon('X')}</button></header><p class="mvp-warning">Проверьте ответы и адресатов. После подтверждения ответы будут опубликованы, комментарии — закрыты.</p><p data-media-readiness role="status" ${mediaHold?'':'hidden'}>${mediaHold?esc(mediaHold.detail):''}</p>${valid||mediaHold?'':'<p class="mvp-error">Выберите до 50 предложений, по одному действию для каждого комментария.</p>'}<div class="mvp-review-list">${cards}</div><footer><button type="button" data-cancel>Вернуться</button><button type="button" class="primary-close" data-confirm ${valid?'':'disabled'}>Подтвердить и выполнить ${rows.length}</button></footer>`;
    const node=dialog;document.body.append(node);node.showModal();
    let reviewBusy=false;
    refreshReviewReadiness=()=>{
      if(dialog!==node)return;
      const exact=proposalRows(ids),hold=reviewMediaHold(exact,snapshot?.items||[]);
      const changed=exact.length!==rows.length||exact.some((p,i)=>p.id!==rows[i].id||p.revision!==rows[i].revision);
      const button=node.querySelector('[data-confirm]');
      if(button)button.disabled=reviewBusy||!unique||!rows.length||rows.length!==ids.length||rows.length>50||changed||!!hold;
      const status=node.querySelector('[data-media-readiness]');
      if(status){status.hidden=!hold;status.textContent=hold?.detail||'';}
    };
    node.querySelectorAll('[data-cancel]').forEach(button=>button.addEventListener('click',()=>node.close()));
    node.addEventListener('close',()=>{node.remove();if(dialog===node)dialog=null;},{once:true});
    node.querySelector('[data-confirm]')?.addEventListener('click',async event=>{
      const button=event.currentTarget;reviewBusy=true;button.disabled=true;
      try{const exact=proposalRows(ids);if(exact.length!==rows.length||exact.some((p,i)=>p.revision!==rows[i].revision))throw new Error('Предложения изменились. Проверьте их снова.');
        const hold=reviewMediaHold(exact,snapshot?.items||[]);if(hold)throw new Error(hold.detail);
        const approval=await api('/api/approvals','POST',{proposals:exact.map(row=>({id:row.id,revision:row.revision}))});
        const latestHold=reviewMediaHold(exact,snapshot?.items||[]);if(latestHold)throw new Error(latestHold.detail);
        await api(`/api/approvals/${encodeURIComponent(approval.id)}/execute`,'POST',{});
        node.close();chosen.clear();await refresh();notify('Выполнение запущено. Фактический исход появится в истории.');
      }catch(error){reviewBusy=false;refreshReviewReadiness();notify(`${error.message} Проверьте историю; автоматически действие не повторяется.`);await refresh({repaint:false}).catch(()=>{});}
    });
  }
  function conversation(){const id=hooks.getSaved().mvpConversationId;return snapshot?.conversations?.find(row=>row.id===id);}
  function assistantContext(){return freezeAssistantContext(hooks.currentAssistantContext?.()||{});}
  function persistAssistantSubmission(){
    hooks.getSaved().mvpAssistantSubmission=assistantSubmission?{...assistantSubmission}:null;
    hooks.persist?.();
  }
  function submissionReadback(raw,submission,actorId){
    const convo=raw.conversations?.find(row=>row.id===submission.conversationId);
    const job=submission.jobId&&raw.jobs?.find(row=>row.id===submission.jobId&&row.kind==='assistant'
      &&row.operatorId===actorId&&row.refId===submission.conversationId);
    const final=convo?.messages?.some(row=>row.role==='assistant'&&row.prepareRunId===submission.jobId);
    const observedUser=job?.sourceUserMessageId
      ?convo?.messages?.some(row=>row.id===job.sourceUserMessageId&&row.role==='user')
      :(convo?.messages||[]).filter(row=>row.role==='user'&&row.text===submission.text).length>Number(submission.matchingCount||0);
    return {job,final,observedUser};
  }
  function restoreAssistantSubmission(raw,{recover=false}={}){
    if(!recover&&sendingAssistant)return;
    const saved=hooks.getSaved(),stored=saved.mvpAssistantSubmission,actorId=raw.operator?.id;
    if(!stored){
      if(!recover&&assistantSubmission?.phase==='accepted'&&assistantSubmission.jobReadbackPending
        &&assistantSubmission.operatorId===actorId){
        const {job,final,observedUser}=submissionReadback(raw,assistantSubmission,actorId);
        if(job||final||observedUser)assistantSubmission.jobReadbackPending=false;
      }
      return;
    }
    if(!actorId||stored.operatorId!==actorId||typeof stored.conversationId!=='string'||typeof stored.text!=='string'){
      assistantSubmission=null;delete saved.mvpAssistantSubmission;hooks.persist?.();return;
    }
    const {job,final,observedUser}=submissionReadback(raw,stored,actorId);
    assistantSubmission={...stored,phase:recover&&stored.phase==='pending'?'unknown':stored.phase,
      jobReadbackPending:recover||job||final||observedUser?false:!!stored.jobReadbackPending};
    if(observedUser){delete saved.mvpAssistantSubmission;hooks.persist?.();}
    else if(assistantSubmission.phase!==stored.phase||assistantSubmission.jobReadbackPending!==!!stored.jobReadbackPending)persistAssistantSubmission();
  }
  async function sendAssistant(text){
    text=asText(text).trim();if(!text||sendingAssistant)return null;
    assistantRuns.clear();
    // Freeze the exact recipients before conversation creation or any other await.
    const context=assistantContext(),saved=hooks.getSaved(),epoch=actorEpoch;
    const navigationRevision=hooks.assistantNavigationRevision?.(),screenKey=hooks.assistantScreenKey?.();
    const submittedChatId=saved.mvpConversationId||'',submittedChatEpoch=assistantChatEpoch;
    let convo=startsFreshDiscussion(text)?null:conversation();
    const submittedItem=context.itemId&&hooks.getData()?.items?.find(i=>i.id===context.itemId);
    const submittedState=submittedItem&&hooks.stateFor(submittedItem);
    const displayedDraft=submittedState?Object.freeze({itemId:submittedItem.id,text:submittedState.draft||'',
      proposalId:submittedState._sourceProposalId||null,proposalRevision:submittedState._sourceProposalRevision??null}):null;
    sendingAssistant=true;
    const originalInput=asText(saved.mvpAiInput),inputCleared=originalInput.trim()===text;
    assistantSubmission={operatorId:snapshot?.operator?.id||hooks.operator?.id||'',conversationId:submittedChatId,text,phase:'pending',matchingCount:(convo?.messages||[]).filter(row=>row.role==='user'&&row.text===text).length};
    // Save the exact message before clearing the editor. A reload during an
    // uncertain POST must leave copyable text, never trigger a replay.
    persistAssistantSubmission();
    if(inputCleared){
      saved.mvpAiInput='';
      const input=globalThis.document?.querySelector?.('#ai-input');
      if(input?.value.trim()===text)input.value='';
      hooks.persist?.();
    }
    chatReading.set(submittedChatId,{top:0,follow:true});
    if(!patchAssistantProgress({follow:true}))hooks.render?.({focusControl:'#ai-input'});
    let runToken=null,messagePostStarted=false;
    try{
      if(context.itemId){const selected=hooks.getData()?.items?.find(i=>i.id===context.itemId);if(selected)await saveDraft(selected);}
      requireActor(epoch);
      if(!convo)convo=await api('/api/conversations','POST',{title:'Обсуждение',itemIds:[]});
      if(assistantChatEpoch===submittedChatEpoch&&(saved.mvpConversationId||'')===submittedChatId)saved.mvpConversationId=convo.id;
      assistantSubmission.conversationId=convo.id;
      persistAssistantSubmission();
      runToken=assistantRuns.begin({conversationId:convo.id,operatorId:snapshot?.operator?.id,navigationRevision,screenKey});
      messagePostStarted=true;
      const result=await api(`/api/conversations/${encodeURIComponent(convo.id)}/messages`,'POST',{text,itemIds:[...context.itemIds],screen:context.screen,...(displayedDraft?{displayedDraft}:{})});
      assistantRuns.bind(runToken,result.jobId);
      assistantSubmission.phase='accepted';assistantSubmission.jobId=result.jobId;assistantSubmission.jobReadbackPending=true;
      persistAssistantSubmission();
      sendingAssistant=false;
      if(assistantChatEpoch===submittedChatEpoch&&saved.mvpConversationId===convo.id){
        if(!patchAssistantProgress())hooks.render?.({focusControl:'#ai-input'});
        const button=globalThis.document?.querySelector?.('.ai-form .send-button');
        if(button)button.disabled=!asText(saved.mvpAiInput).trim();
      }
      // The POST acknowledgment is durable. A slow or failed bootstrap refresh
      // must not make the submitted message appear to have failed.
      void refresh().catch(error=>notify(`Сообщение принято, но обновление задерживается: ${error.message}`));
      return result;
    }catch(error){
      if(assistantRuns.current()?.token===runToken)assistantRuns.clear();
      if(epoch===actorEpoch){
        const sameChat=assistantChatEpoch===submittedChatEpoch&&(saved.mvpConversationId||'')===assistantSubmission?.conversationId;
        if(sameChat&&inputCleared&&!asText(saved.mvpAiInput)){
          saved.mvpAiInput=originalInput;hooks.persist?.();
          const input=globalThis.document?.querySelector?.('#ai-input');
          if(input&&!input.value)input.value=originalInput;
        }
        assistantSubmission={...assistantSubmission,phase:messagePostStarted&&!error.status?'unknown':'failed',error:error.message};
        persistAssistantSubmission();
        if(sameChat){
          if(!patchAssistantProgress())hooks.render?.({focusControl:'#ai-input'});
          const button=globalThis.document?.querySelector?.('.ai-form .send-button');
          if(button)button.disabled=!asText(saved.mvpAiInput).trim();
        }
      }
      throw error;
    }finally{if(epoch===actorEpoch)sendingAssistant=false;}
  }
  function toolResultsHtml(results){
    if(!Array.isArray(results))return '';
    return results.map(entry=>{
      const result=entry?.result||{},name=entry?.name;
      if(entry?.ok===false)return `<p class="mvp-context" role="status">${esc(ordinaryText(entry.error?.message||'Не удалось выполнить действие.'))}</p>`;
      if(name==='navigate')return `<p class="mvp-context">Переход: ${result.kind==='comment'?'комментарий':`раздел «${labels[result.workflow]||'список'}»`}.</p>`;
      if(name==='set_workflow')return `<p class="mvp-context">Перемещено комментариев: ${esc(result.items?.length||0)}. Изменения сохранены в рабочем месте.</p>`;
      if(name==='workspace_stats')return `<p class="mvp-context">В рабочем месте: ${esc(result.total??0)} комментариев.</p>`;
      if(name==='search_comments'||name==='read_comments')return `<div class="assistant-results"><small>${name==='search_comments'?'Найдено':'Прочитано'}: ${esc(result.total??result.items?.length??0)}</small>${(result.items||[]).map(item=>`<a class="assistant-search-result" href="#item/${esc(encodeURIComponent(item.id))}"><strong>${esc(item.author||'Автор')}</strong><p>${esc(ordinaryText(item.text||''))}</p><small>${esc(ordinaryText(item.title||''))}</small></a>`).join('')}${result.hasMore?'<small>Показаны первые совпадения. Можно уточнить запрос.</small>':''}</div>`;
      return '';
    }).join('');
  }
  function assistantProgressHtml(convo){
    const submission=assistantSubmission,visible=submission&&(hooks.getSaved().mvpConversationId||'')===submission.conversationId;
    const showLocal=visible&&
      (convo?.messages||[]).filter(row=>row.role==='user'&&row.text===submission.text).length<=submission.matchingCount;
    const status=submission?.phase==='unknown'?'Исход не подтверждён. Проверьте обсуждение перед повтором.':
      submission?.phase==='failed'?`Не принято: ${esc(ordinaryText(submission.error||'ошибка'))}`:'';
    const local=showLocal?`<div class="chat-entry user" role="status"><strong>Вы</strong><p>${esc(submission.text)}</p>${status?`<small>${status}</small>`:''}</div>`:'';
    const run=assistantRuns.current(),jobId=run?.jobId||(visible&&submission.phase==='accepted'?submission.jobId:null);
    const knownJob=jobId&&snapshot?.jobs?.find(row=>row.id===jobId);
    const job=jobId&&convo&&(!run||run.conversationId===convo.id&&run.operatorId===snapshot?.operator?.id)
      ?snapshot?.jobs?.find(row=>row.id===jobId&&row.refId===convo.id&&(!snapshot?.operator?.id||row.operatorId===snapshot.operator.id)
        &&(run||row.kind==='assistant'&&['queued','running'].includes(row.status))):null;
    const hasFinalMessage=convo?.messages?.some(row=>row.role==='assistant'&&row.prepareRunId===jobId);
    const typing='<span class="assistant-typing">Ассистент печатает<span class="assistant-typing-dots" aria-hidden="true"><span>.</span><span>.</span><span>.</span></span></span>';
    const acknowledgedRun=visible&&submission.phase==='accepted'&&submission.jobReadbackPending&&submission.jobId
      &&submission.operatorId===snapshot?.operator?.id&&!knownJob;
    const failures=discussionFailures(snapshot,convo);
    const failureHtml=failures.map(({job,message})=>`<div class="chat-entry assistant" role="status"><strong>Ассистент</strong><p>Не удалось получить ответ ассистента. Запрос сохранён.</p><small>Запрос: ${esc(message.text.slice(0,160))}</small><button type="button" data-restore-failed-request="${esc(job.id)}">Вернуть запрос в поле</button></div>`).join('');
    const progress=job?job.status==='queued'?'В очереди…':job.status==='running'?typing:job.status==='completed'?'Завершено. Обновляю ответ…':'Не удалось получить ответ ассистента. Запрос сохранён.':acknowledgedRun?typing:null;
    return local+failureHtml+(progress&&!hasFinalMessage&&!failures.some(failure=>failure.job.id===jobId)?`<div class="chat-entry assistant" role="status"><strong>Ассистент</strong><p>${progress}</p>${toolResultsHtml(job?.toolResults)}</div>`:'');
  }
  function restoreFailedRequest(event){
    const button=event.target?.closest?.('[data-restore-failed-request]');if(!button)return;
    const failure=discussionFailures(snapshot,conversation()).find(entry=>entry.job.id===button.dataset.restoreFailedRequest);
    if(!failure||sendingAssistant)return;
    const saved=hooks.getSaved(),input=document.querySelector('#ai-input');
    if(asText(saved.mvpAiInput).trim()||asText(input?.value).trim()){notify('В поле уже есть текст. Сохраните его перед возвратом предыдущего запроса.');return;}
    saved.mvpAiInput=failure.message.text;hooks.persist?.();
    if(input){input.value=failure.message.text;input.focus?.({preventScroll:true});}
    const submit=document.querySelector('.ai-form .send-button');if(submit)submit.disabled=!failure.message.text.trim();
    notify('Запрос возвращён в поле. Проверьте и отправьте его, когда будете готовы.');
  }
  function patchAssistantProgress({follow=false}={}){
    const convo=conversation();
    if(!boundChat?.isConnected||boundChatKey!==(convo?.id||hooks.getSaved().mvpConversationId||''))return false;
    const slot=boundChat.querySelector?.('[data-assistant-progress]');
    if(!slot)return false;
    const reading=follow?{top:0,follow:true}:captureChatReading(boundChat);
    slot.innerHTML=assistantProgressHtml(convo);
    restoreChatReading(boundChat,reading);
    if(reading)chatReading.set(boundChatKey,reading);
    return true;
  }
  function aiHtml(){
    const saved=hooks.getSaved(),convo=conversation(),context=assistantContext();
    const messages=(convo?.messages||[]).map(message=>`<div class="chat-entry ${message.role==='user'?'user':'assistant'}"><strong>${message.role==='user'?'Вы':'Ассистент'}</strong><p>${message.role==='user'?esc(message.text):assistantText(ordinaryText(message.text))}</p>${message.sources?.length?`<small>${esc(ordinaryText(message.sources.map(asText).join(' · ')))}</small>`:''}${toolResultsHtml(message.toolResults)}${message.lookupResults&&!message.toolResults?.some(row=>row.name==='search_comments')?`<div class="assistant-results">${message.lookupResults.items.map(item=>`<a class="assistant-search-result" href="#item/${esc(encodeURIComponent(item.id))}"><strong>${esc(item.author||'Автор')}</strong><p>${esc(item.text)}</p><small>${esc(item.title)}</small></a>`).join('')}${message.lookupResults.hasMore?'<small>Показаны первые совпадения. Можно уточнить запрос.</small>':''}</div>`:''}</div>`).join('');
    const progress=assistantProgressHtml(convo);
    const candidates=assistantDraftCandidates(snapshot,convo,context);
    const preview=candidates.map(p=>{
      const item=snapshot.items.find(i=>i.id===p.itemId),branch=snapshot.branches?.find(b=>b.id===item?.branchId);
      const target=branch?.messages?.find(m=>m.id===item?.targetId),post=snapshot.posts?.find(p=>p.id===(item?.postId||branch?.postId));
      const author=item?.author||target?.author||'Автор неизвестен',original=item?.text||item?.preview||target?.text||'Текст комментария недоступен';
      return `<section class="assistant-candidate"><strong>Вариант ответа · ${esc(author)}</strong><blockquote>${esc(plain(original).slice(0,480))}</blockquote>${post?.title?`<small>${esc(plain(post.title))}</small>`:''}<a href="#item/${encodeURIComponent(p.itemId)}">Открыть комментарий</a><p>${esc(p.text)}</p><button type="button" data-apply-candidate="${esc(p.id)}">Заменить черновик этим текстом</button><small>Сохранит черновик. Публикация — после отдельного подтверждения.</small></section>`;
    }).join('');
    const input=saved.mvpAiInput||'';
    return `<aside class="ai" data-context-item="${esc(context.itemId||'')}" data-assistant-scope="${esc(context.key||'')}" aria-label="Ассистент"><div class="ai-head">${icon('MessagesSquare')}<strong>Ассистент</strong><button id="close-ai" class="icon-button" aria-label="Свернуть ассистента" title="Свернуть ассистента">${icon('X')}</button></div><div class="ai-scroll"><p class="mvp-context">${esc(context.label||'Обсуждение')}</p>${messages||progress?'':'<p class="ai-intro">Что обсудим?</p>'}${messages}<div data-assistant-progress>${progress}</div>${preview}</div><form class="ai-form input-surface"><label class="sr-only" for="ai-input">Сообщение ассистенту</label><textarea id="ai-input" placeholder="Написать ассистенту…">${esc(input)}</textarea><div class="composer-actions">${emojiButton('assistant-emoji')}<button class="send-button" type="submit" aria-label="Отправить ассистенту" title="Отправить ассистенту" ${input.trim()&&!sendingAssistant?'':'disabled'}>${icon('ArrowUp')}</button></div></form></aside>`;
  }
  function bindAi(){
    const shell=document.querySelector('#shell'),saved=hooks.getSaved(),panel=shell?.querySelector('.ai');if(!panel)return;
    const chat=panel.querySelector('.ai-scroll'),chatKey=saved.mvpConversationId||'';
    glassInputObserver?.disconnect();
    const glassForm=panel.querySelector('.ai-form');
    if(glassForm&&!shell.querySelector('.editor-surface')){
      glassForm.style.removeProperty('--paired-bottom');
    }
    if(glassForm?.getBoundingClientRect && typeof ResizeObserver!=='undefined'){
      const measureCover=()=>{
        const height=Math.ceil(glassForm.getBoundingClientRect().height);
        if(!height)return;
        const cover=`${height+24}px`;
        if(panel.style.getPropertyValue('--ai-input-cover')===cover)return;
        const position=captureChatReading(chat);
        panel.style.setProperty('--ai-input-cover',cover);
        restoreChatReading(chat,position);
      };
      measureCover();
      glassInputObserver=new ResizeObserver(measureCover);
      glassInputObserver.observe(glassForm);
    }
    boundChat=chat;boundChatKey=chatKey;
    const reading=chatReading.get(chatKey);
    restoreChatReading(chat,reading);
    // The outer panel is retained across renders; keep one settlement listener.
    panel.removeEventListener?.('assistant-layout-settled',restoreAssistantReading);
    panel.addEventListener?.('assistant-layout-settled',restoreAssistantReading);
    panel.removeEventListener?.('click',restoreFailedRequest);
    panel.addEventListener?.('click',restoreFailedRequest);
    let userMoved=false;
    chat?.addEventListener('wheel',()=>{userMoved=true;},{passive:true});
    chat?.addEventListener('touchstart',()=>{userMoved=true;},{passive:true});
    chat?.addEventListener('pointerdown',()=>{userMoved=true;},{passive:true});
    chat?.addEventListener('scroll',()=>{if(chat===boundChat)rememberAssistantReading();},{passive:true});
    if(chat)requestAnimationFrame(()=>{if(chat.isConnected&&chat===boundChat&&!userMoved)restoreChatReading(chat,reading);});
    shell.querySelector('#toggle-ai')?.addEventListener('click',()=>hooks.setAssistantOpen?.(!saved.ai));
    shell.querySelector('#close-ai')?.addEventListener('click',()=>hooks.setAssistantOpen?.(false));
    const input=shell.querySelector('#ai-input'),form=shell.querySelector('.ai-form');
    panel.querySelectorAll('[data-apply-candidate]').forEach(button=>button.addEventListener('click',async()=>{
      assistantRuns.clear();
      const epoch=actorEpoch;
      const candidate=assistantDraftCandidates(snapshot,conversation(),assistantContext()).find(p=>p.id===button.dataset.applyCandidate);
      const rawItem=snapshot.items.find(i=>i.id===candidate?.itemId),item=hooks.getData()?.items?.find(i=>i.id===candidate?.itemId);
      if(!item||!candidate){notify('Предложение изменилось. Обновите обсуждение.');return;}
      button.disabled=true;
      try{
        clearTimeout(draftTimers.get(item.id));draftTimers.delete(item.id);
        if(draftInFlight.has(item.id)||candidateInFlight.has(item.id))throw Error('Сохраняется ваша правка. Дождитесь сохранения и проверьте вариант снова.');
        const state=hooks.stateFor(item),patch=candidateDraftPatch(candidate,rawItem,{...lineage(item),eventId:uniqueId()});
        const beforeDraft=state.draft,beforeContext=state.draftContext,beforeRevision=state.revision;
        const task=(async()=>{
          const updated=await api(`/api/items/${encodeURIComponent(item.id)}`,'PATCH',patch);
          const editedDuringSave=state.draft!==beforeDraft||state.revision!==beforeRevision;
          if(!editedDuringSave){state.history.push({draft:beforeDraft,context:beforeContext});state.redo=[];state.draft=updated.draft;}
          Object.assign(state,{manualEdited:true,_serverDraft:updated.draft,_serverRevision:updated.revision,_serverDraftEdited:true,_derivedDraft:null,_staleGenerated:false,
            _sourceProposalId:candidate.id,_sourceProposalRevision:candidate.revision,_sourceProposalKind:candidate.kind,_draftSessionId:patch.draftSessionId});
          hooks.persist?.();await refresh({repaint:false});hooks.render?.();
          notify(editedDuringSave?'Ваша новая правка сохранена на экране и будет записана поверх выбранного варианта.':'Новый вариант сохранён в черновик. Ничего не опубликовано.');
          return updated;
        })();
        candidateInFlight.set(item.id,task);
        try{await task;}finally{if(epoch===actorEpoch){candidateInFlight.delete(item.id);if(state.draft!==state._serverDraft)scheduleDraft(item);}}
      }catch(error){notify(error.message);button.disabled=false;}
    }));
    bindEmojiPicker(shell.querySelector('#assistant-emoji'),input);
    input?.addEventListener('input',()=>{saved.mvpAiInput=input.value;hooks.persist?.();shell.querySelector('.ai-form .send-button').disabled=sendingAssistant||!input.value.trim();});
    let composing=false;
    input?.addEventListener('compositionstart',()=>{composing=true;});
    input?.addEventListener('compositionend',()=>{composing=false;});
    input?.addEventListener('keydown',event=>{if(!composing&&submitsAssistantMessage(event)){event.preventDefault();if(!sendingAssistant&&input.value.trim())form.requestSubmit();}});
    form?.addEventListener('submit',async event=>{
      event.preventDefault();const text=input.value.trim();if(!text||sendingAssistant)return;
      saved.mvpAiInput=input.value;
      const button=shell.querySelector('.ai-form .send-button');button.disabled=true;
      try{await sendAssistant(text);rememberAssistantReading();chatReading.set(saved.mvpConversationId||'',{top:0,follow:true});}
      catch(error){button.disabled=false;notify(error.message);}
    });
    panel.onkeydown=event=>{if(event.key==='Escape'&&!dialog)hooks.setAssistantOpen?.(false);};
  }

  function modal(title,content){
    dialog?.remove();const node=document.createElement('dialog');node.className='closure-dialog mvp-dialog';node.innerHTML=`<header><h2>${esc(title)}</h2><button type="button" class="icon-button" data-close aria-label="Закрыть">${icon('X')}</button></header>${content}`;document.body.append(node);node.showModal();node.querySelector('[data-close]').addEventListener('click',()=>node.close());node.addEventListener('close',()=>{node.remove();if(dialog===node)dialog=null;},{once:true});dialog=node;return node;
  }
  function openInstructionEditor(postId) {
    const post=snapshot?.posts?.find(row=>row.id===postId);
    if(!post?.postKey){notify('Не удалось определить пост для указания.');return;}
    const node=modal('Новое указание',`<form id="instruction-editor" class="instruction-editor"><label>Где применять<select name="scope"><option value="post">Для этого поста</option><option value="global">Для всех постов аккаунта</option></select></label><p>${esc(plain(post.title)||'Выбранный пост')}</p><label>Название<input name="title" required maxlength="240"></label><label>Указание<textarea name="text" required maxlength="20000" rows="6"></textarea></label><p>После сохранения указание будет учитываться при следующей подготовке ответов. Текущие ответы могут потребовать перепроверки.</p><button class="primary-close" type="submit">Сохранить и применять</button><p id="instruction-result" role="status"></p></form>`);
    const form=node.querySelector('#instruction-editor'),result=node.querySelector('#instruction-result');
    let request=null,pending=false;
    form.addEventListener('submit',async event=>{
      event.preventDefault();if(pending||!form.reportValidity())return;
      const intent={title:form.elements.title.value.trim(),text:form.elements.text.value.trim(),...(form.elements.scope.value==='post'?{postKey:post.postKey}:{})};
      if(!intent.title||!intent.text)return;
      if(!request||JSON.stringify(request.intent)!==JSON.stringify(intent))request={intent,requestId:uniqueId(),saved:null};
      pending=true;const button=form.querySelector('[type=submit]');button.disabled=true;
      result.textContent='Сохраняем указание…';
      try{
        request.saved ||= await api('/api/knowledge/instructions','POST',{requestId:request.requestId,...intent});
        await loadInstructions({repaint:false});
        const version=request.saved.version;
        if(instructionStatus!=='ready'||!instructionCatalog?.entries?.some(entry=>entry.id===request.saved.entry?.id&&entry.currentVersionId===version?.id)||!instructionCatalog?.versions?.some(row=>row.id===version?.id&&row.status==='active'))throw Error('Не удалось подтвердить сохранение. Повторите проверку.');
        node.close();hooks.render?.();notify('Указание сохранено и действует '+(intent.postKey?'для этого поста.':'для всех постов аккаунта.'));
      }catch(error){result.textContent=ordinaryText(error.message);}
      finally{pending=false;button.disabled=false;}
    });
    form.elements.title.focus();
  }
  function openHistory(){
    const saved=hooks.getSaved(),order=saved.mvpHistoryOrder==='oldest'?'oldest':'newest';
    const operations=chronologicalHistory(snapshot?.operations||[],order),jobs=chronologicalHistory(snapshot?.jobs||[],order);
    const node=modal('История действий',`<div class="history-order"><label for="mvp-history-order">Порядок</label><select id="mvp-history-order"><option value="newest" ${order==='newest'?'selected':''}>Сначала новые</option><option value="oldest" ${order==='oldest'?'selected':''}>Сначала старые</option></select></div><div class="mvp-dialog-scroll"><h3>Операции</h3>${operations.map(op=>`<article class="mvp-history"><strong>${esc(actionLabels[op.action?.action]||'Действие')}</strong><span>${esc(statusLabels[op.status]||op.status)}</span><p>${esc(op.target?.author||op.itemId)} · ${esc(plain(op.action?.text||''))}</p><small>${esc(time(op.createdAt))}</small>${op.status==='unknown'?`<button data-reconcile="${esc(op.id)}">Сверить результат</button>`:''}</article>`).join('')||'<p>Действий пока не было.</p>'}<h3>Задачи</h3>${jobs.map(job=>`<div class="mvp-history"><strong>${esc(jobLabels[job.kind]||job.kind)}</strong><span>${esc(statusLabels[job.status]||job.status)}</span>${job.error?`<p>${esc(ordinaryText(job.error))}</p>`:''}</div>`).join('')||'<p>Задач пока нет.</p>'}</div>`);
    node.querySelector('#mvp-history-order').addEventListener('change',event=>{saved.mvpHistoryOrder=event.target.value;hooks.persist?.();openHistory();});
    node.querySelectorAll('[data-reconcile]').forEach(button=>button.addEventListener('click',async()=>{button.disabled=true;try{await api(`/api/operations/${encodeURIComponent(button.dataset.reconcile)}/reconcile`,'POST',{});node.close();await refresh();notify('Сверка запущена.');}catch(error){button.disabled=false;notify(error.message);}}));
  }
  function openSettings(){
    const sync=snapshot?.sync||{};
    const coverage=mode=>{const row=sync[mode]||{};return row.coverage?.complete?'прочитаны все доступные страницы':`ограниченная выборка${row.hasMore?' · есть ещё страницы':''}`;};
    const retrying=sync.status==='error'||sync.background?.state==='backoff';
    const node=modal('Настройки и состояние',`<div class="mvp-dialog-scroll"><p>Аккаунт: ${esc(snapshot?.account||'LikeAvto')}. Пользователь: ${esc(hooks.operator?.name||'Владелец')}.</p>${hooks.operator?.role==='operator'?'<button id="operator-logout">Выйти</button>':''}<dl><dt>Отправка ответов</dt><dd>${snapshot?.settings?.externalWritesEnabled?'После вашего подтверждения':'Публикация пока выключена'}</dd><dt>Обновление комментариев</dt><dd>Автоматически в фоне</dd><dt>Открытые</dt><dd>${esc(coverage('open'))}</dd><dt>Закрытые</dt><dd>${esc(coverage('closed'))}</dd></dl>${retrying?'<p class="mvp-sync-note" role="status">Связь временно недоступна. Повторяем автоматически.</p>':''}<button id="mvp-backup">Создать резервную копию</button><p id="mvp-setting-result" role="status"></p></div>`);
    node.querySelector('#operator-logout')?.addEventListener('click',()=>hooks.logout?.());
    node.querySelector('#mvp-backup').addEventListener('click',async event=>{event.currentTarget.disabled=true;try{const result=await api('/api/backup','POST',{});node.querySelector('#mvp-setting-result').textContent=`Копия: ${result.path}`;}catch(error){node.querySelector('#mvp-setting-result').textContent=ordinaryText(error.message);}finally{event.currentTarget.disabled=false;}});
  }
  function openMaterials(){
    const node=modal('Материалы',`<div class="mvp-materials"><div class="mvp-material-list"><label>Поиск<input type="search" id="mvp-material-search" placeholder="Название или текст"></label><div id="mvp-material-results"></div><button id="mvp-material-new">Новый материал</button><button id="mvp-material-import">Импортировать</button></div><form id="mvp-material-form"><h3 id="mvp-material-heading">Новый материал</h3><label>Название<input name="title" required></label><label>Содержание<textarea name="text" required></textarea></label><label>Ссылка на источник<input name="sourceUrl" type="url"></label><button class="primary-close" type="submit">Сохранить</button><p id="mvp-material-status" role="status"></p></form></div>`);
    const results=node.querySelector('#mvp-material-results'),form=node.querySelector('#mvp-material-form'),search=node.querySelector('#mvp-material-search');let active=null;
    function renderList(){const q=search.value.toLocaleLowerCase('ru');const rows=(snapshot.materials||[]).filter(m=>`${plain(m.title)} ${plain(m.text)}`.toLocaleLowerCase('ru').includes(q));results.innerHTML=rows.map(m=>`<button data-material="${esc(m.id)}" class="${active===m.id?'is-current':''}"><small>${esc(({knowledge:'Знание',transcript:'Расшифровка',ocr:'Текст с изображения'})[m.kind]||m.kind)}</small><strong>${esc(plain(m.title))}</strong><span>${esc(plain(m.text).slice(0,90))}</span></button>`).join('')||'<p>Ничего не найдено.</p>';results.querySelectorAll('[data-material]').forEach(button=>button.addEventListener('click',()=>{if(form.dataset.dirty==='yes'&&!confirm('Несохранённый текст будет потерян. Продолжить?'))return;active=button.dataset.material;const m=snapshot.materials.find(x=>x.id===active);form.elements.title.value=m.title;form.elements.text.value=m.text;form.elements.sourceUrl.value=m.sourceUrl||'';form.elements.sourceUrl.disabled=true;form.dataset.dirty='';node.querySelector('#mvp-material-heading').textContent='Редактировать материал';renderList();}));}
    search.addEventListener('input',renderList);renderList();
    form.addEventListener('input',()=>{form.dataset.dirty='yes';});
    node.querySelector('#mvp-material-new').addEventListener('click',()=>{if(form.dataset.dirty==='yes'&&!confirm('Несохранённый текст будет потерян. Продолжить?'))return;active=null;form.reset();form.elements.sourceUrl.disabled=false;form.dataset.dirty='';node.querySelector('#mvp-material-heading').textContent='Новый материал';renderList();});
    form.addEventListener('submit',async event=>{event.preventDefault();const target=event.currentTarget,button=target.querySelector('[type=submit]');button.disabled=true;try{let result;if(active){const old=snapshot.materials.find(x=>x.id===active);result=await api(`/api/materials/${encodeURIComponent(active)}`,'PATCH',{expectedRevision:old.revision,title:target.elements.title.value,text:target.elements.text.value});}else result=await api('/api/materials','POST',{title:target.elements.title.value,text:target.elements.text.value,kind:'knowledge',...(target.elements.sourceUrl.value?{sourceUrl:target.elements.sourceUrl.value}:{})});await refresh({repaint:false});active=result.id;form.dataset.dirty='';node.querySelector('#mvp-material-status').textContent='Материал сохранён.';renderList();}catch(error){node.querySelector('#mvp-material-status').textContent=ordinaryText(error.message);}finally{button.disabled=false;}});
    node.querySelector('#mvp-material-import').addEventListener('click',async event=>{const button=event.currentTarget;button.disabled=true;try{await api('/api/materials/import','POST',{});node.querySelector('#mvp-material-status').textContent='Импорт запущен. Список обновится после завершения.';}catch(error){node.querySelector('#mvp-material-status').textContent=ordinaryText(error.message);}finally{button.disabled=false;}});
  }
  function bindExtras(){
    requestAnimationFrame(trackPresented);
    const nav=document.querySelector('#primary-navigation');
    if(!nav)return;
    // Use the workshop's own nav-view component, widths and collapse animation.
    if(!nav.querySelector('.mvp-extra-links'))nav.insertAdjacentHTML('beforeend',`<div class="mvp-extra-links" role="group" aria-label="Рабочие разделы"><a class="nav-view" href="#settings" data-mvp-view="settings" aria-label="Настройки" title="Настройки">${icon('UserRound')}<span class="nav-text">Настройки</span></a></div>`);
    // The original overview history link now opens the real operation ledger.
    const history=nav.querySelector('a[href="#overview/history"]');
    if(history){history.dataset.mvpView='history';history.title='История действий';}
    nav.querySelectorAll('[data-mvp-view]').forEach(button=>button.addEventListener('click',event=>{event.preventDefault();event.stopPropagation();const action=button.dataset.mvpView;if(action==='history')openHistory();else if(action==='settings')openSettings();}));
  }

  return {load,hydrate,refresh,start,stop,aiHtml,bindAi,rememberAssistantReading,restoreAssistantReading,sendAssistant,saveDraft,scheduleDraft,prepareReply,closeOne,closeMany,changeWorkflow,bindExtras,openMaterials,openHistory,openSettings,review,trackPresented,flushFeedback,instructionContext,loadInstructions,openInstructionEditor};
}

export const ARRIVAL_MAX_HOLD_MS = 5 * 60 * 1000;

const terminalPreparation = new Set(['prepared','needs_attention','error','stale']);
const pendingPreparation = new Set(['queued','running']);

const itemFor = record => record?.item || record || {};
const stateFor = record => record?.state || {};
const idFor = record => String(itemFor(record).id || '');

function hasOperatorOrSavedWork(record, selectedId) {
  const item=itemFor(record),state=stateFor(record),preparation=item.autoPreparation||{};
  if(item.id===selectedId)return true;
  if((state.view||item.workflow||item.view)!=='attention')return true;
  if(item.draftEdited||String(item.draft||'').trim())return true;
  if(state.manualEdited||state._serverDraftEdited||state.replyStarted||state.localRecovery||String(state.draft||'').trim())return true;
  if(state.proposal||state._pendingProposal||state._sourceProposalId||state._displayedProposalId||state._staleGenerated)return true;
  if(item.draftOrigin||preparation.requiresReview||preparation.savedProposalId||preparation.humanOverrideAt)return true;
  if(item.autoRevalidation&&typeof item.autoRevalidation==='object')return true;
  return false;
}

export function arrivalIsProcessing(record) {
  const item=itemFor(record),state=stateFor(record),status=item.autoPreparation?.status;
  if((state.view||item.workflow||item.view)!=='attention')return false;
  if(terminalPreparation.has(status))return false;
  if(pendingPreparation.has(status))return true;
  if(status)return false;
  return Boolean(item.preparationMediaWait)
    || ['new','inprogress'].includes(item.providerStatus);
}

function firstSeen(value) {
  const raw=typeof value==='object'&&value ? value.firstSeenAt : value;
  const parsed=typeof raw==='number'?raw:Date.parse(raw);
  return Number.isFinite(parsed)?parsed:null;
}

export function projectQueueArrivals(records, {
  arrivals={}, candidateIds=[], selectedId='', now=Date.now(), maxHoldMs=ARRIVAL_MAX_HOLD_MS,
  establishBaseline=false
}={}) {
  const next=Object.fromEntries(Object.entries(arrivals).filter(([,entry])=>!establishBaseline||entry?.afterBaseline===true));
  const candidates=new Set(candidateIds),visible=[],processing=[],admittedIds=[];
  let nextReleaseAt=null;
  for(const record of records){
    const id=idFor(record);if(!id)continue;
    if(!Object.hasOwn(next,id)&&candidates.has(id)&&arrivalIsProcessing(record))next[id]={firstSeenAt:now,afterBaseline:true};
    if(!Object.hasOwn(next,id)){visible.push(record);if(candidates.has(id))admittedIds.push(id);continue;}
    const seen=firstSeen(next[id]);
    const release=hasOperatorOrSavedWork(record,selectedId)||!arrivalIsProcessing(record)
      ||seen===null||now-seen>=maxHoldMs;
    if(release){delete next[id];visible.push(record);admittedIds.push(id);}
    else {
      processing.push(record);
      const deadline=seen+maxHoldMs;
      nextReleaseAt=nextReleaseAt===null?deadline:Math.min(nextReleaseAt,deadline);
    }
  }
  return {visible,processing,admittedIds:[...new Set(admittedIds)],arrivals:next,nextReleaseAt};
}

export function arrivalGenerationSignature(items=[]) {
  return JSON.stringify([...items].map(item=>[
    item.id,item.revision,item.workflow,item.draft,item.draftEdited,
    item.autoPreparation?.status,item.autoPreparation?.requiresReview,
    item.autoPreparation?.savedProposalId,item.autoPreparation?.humanOverrideAt,
    item.preparationMediaWait?.until,item.preparationMediaWait?.status,Boolean(item.autoRevalidation),item.autoRevalidation?.status,
    item.draftOrigin?.sourceProposalId||item.draftOrigin?.id,
    item.initialState?._sourceProposalId,item.initialState?._displayedProposalId,
    item.initialState?._staleGenerated,item.initialState?._derivedDraft
  ]).sort((a,b)=>String(a[0]).localeCompare(String(b[0]))));
}

export function arrivalAnimationIds(projection, {
  remoteChanged=false, reducedMotion=false, inQueueView=true, listedIds=[]
}={}) {
  if(!remoteChanged||reducedMotion||!inQueueView)return [];
  const listed=new Set(listedIds);
  return (projection?.admittedIds||[]).filter(id=>listed.has(id));
}

export function arrivalCandidateIds(items=[], {initial=false,knownIds=[]}={}) {
  if(initial)return [];
  const known=new Set(knownIds);
  return items.map(item=>item?.id).filter(id=>id&&!known.has(id));
}

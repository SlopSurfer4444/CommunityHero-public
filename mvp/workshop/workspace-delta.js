// Transport snapshots stay separate from mutable editor/selection state.
const collectionKeys=new Set(['items','posts','branches','proposals','operations','materials','conversations','jobs','approvals']);
const unsafeKeys=new Set(['__proto__','prototype','constructor']);
const object=value=>value!==null&&typeof value==='object'&&!Array.isArray(value);
const identity=value=>typeof value==='string'&&value.length>0;
const own=(value,key)=>Object.prototype.hasOwnProperty.call(value,key);
const invalid=()=>{throw new Error('Invalid workspace delta');};

function ids(rows){
  if(!Array.isArray(rows))invalid();
  const result=new Set();
  for(const row of rows){if(!object(row)||!identity(row.id)||result.has(row.id))invalid();result.add(row.id);}
  return result;
}
function uniqueStrings(values){
  if(!Array.isArray(values)||values.some(value=>!identity(value))||new Set(values).size!==values.length)invalid();
  return new Set(values);
}
function topLevelKey(key){return identity(key)&&!unsafeKeys.has(key)&&(!collectionKeys.has(key)||key==='approvals')&&key!=='workspaceVersion';}

export function canRequestWorkspaceDelta(snapshot){
  return object(snapshot)&&identity(snapshot.workspaceVersion)&&identity(snapshot.operator?.id);
}

// Validate the entire response before publishing anything. Unchanged rows may
// share references; neither input snapshot nor response is ever mutated.
export function mergeWorkspaceDelta(snapshot,response){
  if(!object(response))invalid();
  if(response.kind==='full'){
    const full=response.snapshot;
    if(!canRequestWorkspaceDelta(full)||!Array.isArray(full.items))invalid();
    // Approvals retain atomic legacy fallback when IDs cannot be keyed.
    for(const key of collectionKeys)if(key!=='approvals'&&own(full,key))ids(full[key]);
    return full;
  }
  if(response.kind!=='delta'||!canRequestWorkspaceDelta(snapshot)
    ||response.baseVersion!==snapshot.workspaceVersion||!identity(response.workspaceVersion)
    ||response.actorId!==snapshot.operator.id||!object(response.collections)||!object(response.set))invalid();
  const removed=uniqueStrings(response.remove);
  for(const key of removed)if(!topLevelKey(key)||own(response.set,key)||key==='operator')invalid();
  for(const key of Object.keys(response.set))if(!topLevelKey(key))invalid();
  if(own(response.set,'operator')&&response.set.operator?.id!==response.actorId)invalid();
  const next={...snapshot,...response.set,workspaceVersion:response.workspaceVersion};
  for(const key of removed)delete next[key];
  for(const [key,change] of Object.entries(response.collections)){
    if(!collectionKeys.has(key)||own(response.set,key)||removed.has(key)||!object(change)||Object.keys(change).some(field=>!['upsert','remove','order'].includes(field)))invalid();
    const old=snapshot[key]??[],oldIds=ids(old),upsertIds=ids(change.upsert),removeIds=uniqueStrings(change.remove);
    for(const id of removeIds)if(!oldIds.has(id)||upsertIds.has(id))invalid();
    const rows=new Map(old.filter(row=>!removeIds.has(row.id)).map(row=>[row.id,row]));
    for(const row of change.upsert)rows.set(row.id,row);
    if(own(change,'order')){
      const orderIds=uniqueStrings(change.order);
      if(orderIds.size!==rows.size||change.order.some(id=>!rows.has(id)))invalid();
      next[key]=change.order.map(id=>rows.get(id));
    }else{
      if(removeIds.size||[...upsertIds].some(id=>!oldIds.has(id)))invalid();
      next[key]=[...rows.values()];
    }
  }
  return next;
}

// Delta merge preserves untouched row references. Cache per-row semantics so a
// changed collection never serializes every unchanged branch/message again.
function coveragePresentationFields(row){
  if(!row||typeof row!=='object')return row;
  const accounting=row.accounting;
  return {scope:row.scope,done:row.done,traversalComplete:row.traversalComplete,contextComplete:row.contextComplete,
    coverageComplete:row.coverageComplete,unknownDates:row.unknownDates,invalidatedAt:row.invalidatedAt,
    accounting:accounting&&{version:accounting.version,trackedUnique:accounting.trackedUnique,importedUnique:accounting.importedUnique,
      unresolvedUnique:accounting.unresolvedUnique,unverifiedPages:accounting.unverifiedPages,overflow:accounting.overflow}};
}
export function createWorkspaceChangeTracker(){
  const dataClocks=new Set(['providerObservedAt','statusObservedAt','providerStatusObservedAt','contextObservedAt']);
  const uiClocks=new Set(['updatedAt','lastAttemptAt','lastSuccessAt','startedAt','finishedAt','scannedAt']);
  const slots=new Map(),dataCache=new WeakMap(),uiCache=new WeakMap(),jobCache=new WeakMap();
  function fingerprint(value,cache,ignored,project=value=>value){
    if(value&&typeof value==='object'&&cache.has(value))return cache.get(value);
    const result=JSON.stringify(project(value),(key,value)=>ignored.has(key)?undefined:value);
    if(value&&typeof value==='object')cache.set(value,result);
    return result;
  }
  function changed(key,value,cache,ignored,project){
    const previous=slots.get(key);
    if(previous&&previous.reference===value)return false;
    const array=Array.isArray(value);
    const signatures=array?value.map(row=>fingerprint(row,cache,ignored,project)):fingerprint(value,cache,ignored,project);
    const differs=!previous||previous.array!==array||(array
      ?previous.signatures.length!==signatures.length||signatures.some((signature,index)=>signature!==previous.signatures[index])
      :previous.signatures!==signatures);
    slots.set(key,{reference:value,array,signatures});return differs;
  }
  return (raw,instructionCatalog,instructionStatus)=>{
    let dataChanged=false,uiChanged=false;
    for(const key of ['items','posts','branches','proposals','operations','materials'])dataChanged=changed(`data:${key}`,raw[key],dataCache,dataClocks)||dataChanged;
    for(const key of ['conversations','materials','account','settings'])uiChanged=changed(`ui:${key}`,raw[key],uiCache,uiClocks)||uiChanged;
    uiChanged=changed('ui:instructions',instructionCatalog,uiCache,uiClocks)||uiChanged;
    uiChanged=changed('ui:instruction-status',instructionStatus,uiCache,uiClocks)||uiChanged;
    uiChanged=changed('ui:jobs',raw.jobs,jobCache,uiClocks,(row)=>row&&({id:row.id,kind:row.kind,status:row.status,error:row.error}))||uiChanged;
    uiChanged=changed('ui:sync',raw.sync,uiCache,uiClocks,sync=>sync&&({status:sync.status,lastError:sync.lastError,
      openCoverage:coveragePresentationFields(sync.openCoverage),closedCoverage:coveragePresentationFields(sync.scan?.closed),
      invalidatedAt:sync.scan?.invalidatedAt,backgroundState:sync.background?.state,
      ...Object.fromEntries(['open','closed'].map(mode=>[mode,sync[mode]&&{hasMore:sync[mode].hasMore,cursor:sync[mode].cursor,complete:sync[mode].coverage?.complete}]))}))||uiChanged;
    return {dataChanged,uiChanged};
  };
}

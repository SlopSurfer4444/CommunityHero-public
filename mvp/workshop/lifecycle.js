// Local synthetic workflow only. No connector calls or public mutations.
export const isOpen = state => ['attention','prepared','waiting'].includes(state.view);
export const replyFor = record => record.messages.find(message => message.role === 'brand' && message.parentId === record.item.targetId && !message.deleted);
// Refresh corrected synthetic defaults only; explicit operator work wins.
export function reconcileFixtureState({item,state}) {
  if(!item.fixtureStateRevision || state.fixtureStateRevision===item.fixtureStateRevision) return state;
  if(!isOpen(state) || state.decision!==item.decision || state.draft!==item.draft || state.manualEdited || state.replyStarted ||
    state.revision || state.history?.length || state.redo?.length || state.chat?.length || state.events?.length || state.aiInput || state.proposal) return state;
  return {...state,...structuredClone(item.initialState),view:item.view,fixtureStateRevision:item.fixtureStateRevision};
}
export function signature(record) {
  return JSON.stringify([record.state.view, record.state.decision, record.state.revision,
    record.state.draft, record.state.manualEdited, record.context, replyFor(record)?.id,
    record.state.closure, record.state.deletion]);
}
export function closeRecord(record, at) {
  if (!isOpen(record.state)) return null;
  const state = structuredClone(record.state);
  const reply = replyFor(record);
  state.previousView = state.view;
  state.view = 'closed';
  state.closure = {at, actor:'Оператор', outcome:reply ? 'reply' : 'no_reply', replyId:reply?.id || null, context:record.context};
  state.events = [...(state.events || []), {type:'closed', ...state.closure}];
  state.proposal = null;
  return state;
}
export function reopenRecord(record, at) {
  if (record.state.view !== 'closed') return null;
  const state = structuredClone(record.state);
  state.view = record.context !== state.closure?.context ? 'attention' :
    (['attention','prepared','waiting'].includes(state.previousView) ? state.previousView : 'attention');
  state.events = [...(state.events || []), {type:'reopened',at,actor:'Оператор'}];
  return state;
}
export function previewClosure(records, options) {
  const candidates = records.filter(record => isOpen(record.state)
    && (!options.ids || options.ids.includes(record.item.id))
    && (!options.postId || record.postId === options.postId)
    && (!options.before || record.createdAt.slice(0,10) <= options.before));
  const selected = [], excluded = [];
  for (const record of candidates) {
    const state = record.state;
    const reason = options.decision !== 'all' && state.decision !== options.decision ? 'Другое решение' :
      options.keepAttention && state.view === 'attention' ? 'Нужно участие' :
      options.keepWaiting && state.view === 'waiting' ? 'Ждём' :
      options.keepEdits && (state.manualEdited || state.history?.length) ? 'Есть правки' : null;
    if (reason) excluded.push({id:record.item.id,reason});
    else selected.push({id:record.item.id,signature:signature(record)});
  }
  return {selected,excluded,total:candidates.length};
}
export function applyClosure(records, plan, at) {
  const byId = new Map(records.map(record => [record.item.id,record]));
  const updates = [], skipped = [];
  for (const candidate of plan.selected) {
    const record = byId.get(candidate.id);
    if (!record || signature(record) !== candidate.signature || !isOpen(record.state)) {
      skipped.push(candidate.id); continue;
    }
    updates.push({id:candidate.id,state:closeRecord(record,at)});
  }
  return {updates,skipped};
}

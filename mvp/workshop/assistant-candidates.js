// Only proposals admitted for this conversation can be applied from its panel.
// Applying writes a draft; this module never approves or executes social actions.
export function assistantDraftCandidates(snapshot, conversation, context) {
  const allowed=new Set(context?.itemIds||[]);
  const runs=new Set((conversation?.messages||[]).filter(m=>m.role==='assistant').map(m=>m.prepareRunId).filter(Boolean));
  const items=new Map((snapshot?.items||[]).map(item=>[item.id,item]));
  const latest=new Map();
  for(const candidate of snapshot?.proposals||[]) {
    const item=items.get(candidate.itemId);
    if(!runs.has(candidate.prepareRunId)||!allowed.has(candidate.itemId)||!item
      ||candidate.status!=='draft'||candidate.kind!=='reply_and_close'||!candidate.text?.trim()
      ||['closed','deleted'].includes(item.workflow)||candidate.itemRevision!==item.revision
      ||candidate.contextEvidenceDigest!==item.contextEvidenceDigest||candidate.branchContextDigest!==item.branchContextDigest)continue;
    latest.set(candidate.itemId,candidate);
  }
  return [...latest.values()];
}

export function candidateDraftPatch(candidate,item,identity={}) {
  if(!candidate||!item||candidate.itemId!==item.id||candidate.itemRevision!==item.revision
    ||candidate.status!=='draft'||candidate.kind!=='reply_and_close'||!candidate.text?.trim()
    ||candidate.contextEvidenceDigest!==item.contextEvidenceDigest||candidate.branchContextDigest!==item.branchContextDigest
    ||['closed','deleted'].includes(item.workflow))throw new Error('Предложение изменилось. Обновите обсуждение и проверьте текст снова.');
  if(!identity.eventId||!identity.draftSessionId)throw new Error('Не удалось сохранить идентификатор правки.');
  return {expectedRevision:item.revision,draft:candidate.text,draftEdited:true,
    sourceProposalId:candidate.id,sourceProposalRevision:candidate.revision,
    eventId:identity.eventId,draftSessionId:identity.draftSessionId,...(identity.sessionId?{sessionId:identity.sessionId}:{})};
}

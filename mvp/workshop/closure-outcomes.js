const id = value => typeof value === 'string' && value.length ? value : null;
const sameScope = (row, item) => !id(row?.providerObjectId) || row.providerObjectId === (item.providerObjectId || item.objectId);

// A published brand message is evidence only for its explicit direct recipient.
// Text, mentions, conversation membership and queue pagination prove no edge.
function directReply(message, item, target) {
  if (message.role !== 'brand' || message.deleted || message.unavailable || message.id === target.id || !sameScope(message, item)) return false;
  const targetProviderId = id(item.providerItemId) || id(item.itemId) || id(target.providerItemId);
  const providerParent = id(message.replyToProviderItemId);
  const localParent = id(message.parentId);
  if (providerParent) {
    return !!targetProviderId && providerParent === targetProviderId
      && (!localParent || localParent === target.id);
  }
  return localParent === target.id;
}

export function deriveClosure(item, branches = [], operations = []) {
  if ((item.workflow || item.view) !== 'closed') return null;
  // Preserve confirmed local operations as the authoritative outcome. Most
  // recent admitted successful close wins if historical operations coexist.
  const op = operations.filter(entry => entry.itemId === item.id && entry.status === 'succeeded'
    && ['close', 'reply_and_close'].includes(entry.action?.action)).at(-1);
  if (op) return {at:op.updatedAt || op.createdAt || null, actor:'LikeAvto',
    outcome:op.action.action === 'reply_and_close' ? 'reply' : 'no_reply', source:'operation', operationId:op.id || null};

  // Import observations do not provide the time at which the operator closed it.
  const result = {at:item.closedAt || null, actor:'LikeAvto', outcome:'unknown', source:'provider_context', replyId:null};
  const matches = branches.filter(branch => branch.id === item.branchId);
  if (matches.length !== 1) return {...result, reason:'missing_or_ambiguous_branch'};
  const branch = matches[0], messages = Array.isArray(branch.messages) ? branch.messages : [];
  if (item.postId && branch.postId !== item.postId) return {...result, reason:'branch_scope_mismatch'};
  const targets = messages.filter(message => message.id === item.targetId);
  if (targets.length !== 1 || !sameScope(targets[0], item)) return {...result, reason:'missing_or_ambiguous_target'};
  const target = targets[0], targetProviderId = id(item.providerItemId) || id(item.itemId);
  if (targetProviderId && id(target.providerItemId) && targetProviderId !== target.providerItemId) return {...result, reason:'target_identity_mismatch'};
  // Duplicate IDs cannot establish an unambiguous relation.
  const ids = new Set(messages.map(message => message.id));
  if (ids.size !== messages.length) return {...result, reason:'ambiguous_message_identity'};
  const providerIds = messages.filter(message => sameScope(message, item)).map(message => id(message.providerItemId)).filter(Boolean);
  if (new Set(providerIds).size !== providerIds.length) return {...result, reason:'ambiguous_provider_identity'};
  const reply = messages.find(message => directReply(message, item, target));
  if (reply) return {...result, outcome:'reply', replyId:reply.id, reason:'direct_brand_reply'};

  // contextComplete is a branch guarantee, unlike sync.coverage.complete (which
  // covers queue pages). The current provider deliberately never supplies it.
  const complete = branch.contextComplete === true && branch.contextTruncated !== true
    && !(branch.missingParentIds?.length) && !branch.unavailableReason
    && !(Number.isFinite(branch.knownMessageCount) && branch.knownMessageCount > messages.length)
    && messages.every(message => id(message.id) && sameScope(message, item) && !message.unavailable && !message.deleted
      && (!id(message.parentId) || ids.has(message.parentId))
      && (message.role !== 'brand' || id(message.parentId))
      && (!id(message.replyToProviderItemId) || messages.some(parent => parent.id === message.parentId && parent.providerItemId === message.replyToProviderItemId)));
  return {...result, outcome:complete ? 'no_reply' : 'unknown', reason:complete ? 'complete_branch_without_direct_reply' : 'incomplete_reply_evidence'};
}

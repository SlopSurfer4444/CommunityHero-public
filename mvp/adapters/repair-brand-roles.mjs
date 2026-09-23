// Offline, pure repair: callers own backup, transaction and applying the result.
// Exact GET /v1/items/{sampleItemId} observations on 2026-09-22; each official=1.
export const verifiedBrandAuthors = Object.freeze([
  {objectId:'11341',authorId:'provider:vk_-135891342',sampleItemId:'6aad6798e38a2a6b20e03d4d'},
  {objectId:'11389',authorId:'provider:youtube_UCSwrR_qTcXvjKVxgrO1v1pQ',sampleItemId:'6aaeaacb6aa20d243282a55b'},
  {objectId:'11391',authorId:'provider:instagram_likeavto_import',sampleItemId:'6aad69526aa20d2432aa00d7'},
]);
const binding={accountId:'LikeAvto',connector:'angryspace',id:'angryspace-likeavto-v1',providerAccountId:'likeavto',revision:1,workspaceId:'local-pilot'};
const sameBinding=value=>value && Object.entries(binding).every(([key,expected])=>value[key]===expected);
export function repairBrandRoles(workspace) {
  if(workspace?.account!=='LikeAvto'||!sameBinding(workspace.connectorBinding))throw new Error('BRAND_REPAIR_SCOPE_MISMATCH');
  const result=structuredClone(workspace), changes=[];
  const proof=new Map(verifiedBrandAuthors.map(e=>[`${e.objectId}\n${e.authorId}`,e]));
  for(const branch of result.branches??[]){
    const owners=(result.items??[]).filter(i=>i.branchId===branch.id&&i.postId===branch.postId);
    if(!owners.length||owners.some(i=>!sameBinding(i.connectorBinding??result.connectorBinding)))continue;
    const objects=new Set(owners.map(i=>i.providerObjectId??i.objectId));
    if(objects.size!==1)continue;
    const objectId=[...objects][0];
    for(const field of ['messages','observedMessages'])for(const message of branch[field]??[]){
      // Missing author/provider identity is left for exact provider refresh.
      if(message.providerObjectId!==objectId||typeof message.providerItemId!=='string'||!message.providerItemId)continue;
      const evidence=proof.get(`${objectId}\n${message.authorId}`);
      if(!evidence)continue;
      if(message.role==='brand'&&message.providerOfficial===true)continue;
      changes.push({branchId:branch.id,field,messageId:message.id,providerItemId:message.providerItemId,objectId,previousRole:message.role,sampleItemId:evidence.sampleItemId});
      message.role='brand';message.providerOfficial=true;message.roleEvidence='verified-provider-author';
    }
  }
  return {workspace:result,changes};
}

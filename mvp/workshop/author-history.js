// Identity is platform/account scoped. A display name never establishes identity.
const clean = value => typeof value==='string'?value.trim():'';
export function authorKey(item, message={}) {
  const id=clean(message.authorId)||clean(item.authorId);
  const scope=clean(item.objectId)||clean(item.providerObjectId)||clean(message.providerObjectId);
  return id&&scope?JSON.stringify([scope,id]):null;
}
export function authorHistory(data, selectedItem, selectedMessage) {
  const target=selectedMessage||(data.branches||[]).find(b=>b.id===selectedItem.branchId)?.messages?.find(m=>m.id===selectedItem.targetId)||{};
  const key=authorKey(target.id===selectedItem.targetId?selectedItem:{objectId:selectedItem.objectId,providerObjectId:selectedItem.providerObjectId},target), entries=new Map();
  const items=new Map((data.items||[]).map(i=>[i.targetId,i]));
  const owners=new Map((data.items||[]).map(i=>[i.branchId,i]));
  const posts=new Map((data.posts||[]).map(p=>[p.id,p]));
  for(const branch of data.branches||[]) {
    const owner=owners.get(branch.id);
    if(!owner)continue;
    for(const message of branch.messages||[]) {
      if(message.role==='brand')continue;
      const direct=items.get(message.id), scope=direct||{objectId:owner.objectId,providerObjectId:owner.providerObjectId};
      const isTarget=message.id===target.id;
      if(key?authorKey(scope,message)!==key:!isTarget)continue;
      const id=JSON.stringify([message.providerObjectId||owner.objectId,message.providerItemId||message.id]);
      const replies=(branch.messages||[]).filter(m=>m.role==='brand'&&m.parentId===message.id);
      const existing=entries.get(id);
      if(existing){for(const reply of replies)if(!existing.replies.some(r=>r.id===reply.id))existing.replies.push(reply);continue;}
      entries.set(id,{...message,itemId:direct?.id||owner.id,post:posts.get(branch.postId||owner.postId),replies,
        createdAt:message.createdAt||direct?.createdAt||null});
    }
  }
  return {identified:!!key,author:target.author||selectedItem.author||'Автор',entries:[...entries.values()].sort((a,b)=>(Date.parse(b.createdAt)||0)-(Date.parse(a.createdAt)||0)||String(a.id).localeCompare(String(b.id)))};
}

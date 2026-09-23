// Freeze screen references at send time. Screens are context, never action authority.
export const ASSISTANT_SCREEN_LIMIT = 20;
const kinds = new Set(['comment','queue','topic','post','analytics','history','discussions']);
const text = (value,max=240) => typeof value==='string'?value.slice(0,max):'';
export function buildAssistantContext(input = {}) {
  const kind = kinds.has(input.kind)?input.kind:(input.itemId?'comment':'queue');
  const itemId=text(input.itemId,200)||null;
  const candidates=[...(itemId?[itemId]:[]),...(Array.isArray(input.itemIds)?input.itemIds:[])];
  const all=[...new Set(candidates.filter(id=>typeof id==='string'&&id&&id.length<=200))];
  const itemIds=Object.freeze(all.slice(0,ASSISTANT_SCREEN_LIMIT));
  const key=text(input.key),label=text(input.label);
  const filters=Object.freeze(Object.fromEntries(['channel','postId','period','dateField','from','to','outcome','workflow']
    .filter(k=>typeof input.filters?.[k]==='string').map(k=>[k,text(input.filters[k])])));
  const totalCount=Number.isSafeInteger(input.totalCount)&&input.totalCount>=0?input.totalCount:all.length;
  const screen=Object.freeze({kind,key,label,selectedItemId:itemId,itemIds,
    postId:text(input.postId,200),topicKey:text(input.topicKey),query:text(input.query,300),
    order:input.order==='oldest'?'oldest':'newest',filters,totalCount,
    visibleItemCount:all.length,truncated:all.length>itemIds.length||totalCount>itemIds.length});
  return Object.freeze({itemId,itemIds,key,label,postId:screen.postId,topicKey:screen.topicKey,screen});
}

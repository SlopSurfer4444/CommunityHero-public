// Literal repeats from already loaded comments, never category buckets or backfill.
// Deliberately sparse: semantic paraphrases require a separate, verified classifier.
const clean = value => String(value || '').replace(/<[^>]*>/g, ' ').replace(/https?:\/\/\S+/g, ' ').replace(/@[\p{L}\p{N}_.-]+/gu, ' ').replace(/\s+/g, ' ').trim();
const normalized = value => clean(value).normalize('NFKC').toLocaleLowerCase('ru').replaceAll('ё','е');
const courtesy = new Set(['пожалуйста', 'подскажите', 'скажите', 'здравствуйте', 'привет', 'спасибо']);
const filler = new Set(['а', 'ну', 'же']);
const substantive = new Set(['какой','какая','какое','какие','это','там','тут','очень','все','вам','вас','за','и','в','на','для']);
function wording(text) {
  // Keep word order, negation, models, quantities and units. Do not stem words:
  // overlap alone cannot distinguish different objects or opposite assertions.
  return (normalized(text).match(/[\p{L}\p{N}]+/gu) || []).filter(w=>!courtesy.has(w)&&!filler.has(w));
}
function question(text, tokens) {
  return /\?/u.test(text) || /^(как|какой|какая|какое|какие|сколько|почему|зачем|когда|где)$/u.test(tokens[0]||'') || tokens.includes('ли');
}
function similar(a,b) {
  return a.isQuestion===b.isQuestion && a.signature===b.signature;
}
function authorKey(target) {
  const id=target.authorId || target.author?.id;
  if(id) return 'id:'+id;
  const name=typeof target.author==='string'?normalized(target.author):'';
  return name && !['автор неизвестен','неизвестный автор','unknown'].includes(name) ? 'name:'+name : null;
}
function repeatedParticipants(entries) {
  const known=new Set(entries.map(e=>e.author).filter(Boolean));
  return known.size>=2 || entries.some(e=>!e.author);
}
function hash(text) { let h=2166136261;for(const ch of text){h^=ch.codePointAt(0);h=Math.imul(h,16777619);}return (h>>>0).toString(36); }
const isOpen = r => ['attention','prepared','waiting'].includes(r.state.view);

export function buildPostTopics(records, posts) {
  const byPost = new Map();
  for(const record of records) {
    if(record.state.view==='deleted')continue;
    const target=record.messages.find(m=>m.id===record.item.targetId);
    if(!target||target.textUnavailable||target.deleted||target.role==='brand')continue;
    const text=clean(target.text),tokens=wording(text);
    if(tokens.length<2 || !tokens.some(w=>!substantive.has(w)))continue;
    if(!byPost.has(record.postId))byPost.set(record.postId,new Map());
    byPost.get(record.postId).set(record.item.id,{record,text,signature:tokens.join(' '),isQuestion:question(text,tokens),author:authorKey(target)});
  }
  const groups=[];
  for(const post of posts) {
    const entries=[...(byPost.get(post.id)?.values()||[])].sort((a,b)=>a.record.item.id.localeCompare(b.record.item.id));
    const clusters=[];
    for(const entry of entries){
      // Complete-link: every member must repeat the same concrete wording.
      const cluster=clusters.find(c=>c.every(other=>similar(entry,other)));
      if(cluster)cluster.push(entry);else clusters.push([entry]);
    }
    const topics=clusters.filter(c=>c.length>=2&&repeatedParticipants(c)).map(entries=>{
      const quote=entries[0].text, id='repeat-'+hash(entries[0].signature+':'+entries[0].isQuestion);
      const title=quote.length>76?quote.slice(0,73)+'…':quote;
      const topicRecords=entries.map(e=>e.record), open=topicRecords.filter(isOpen);
      return {id,key:post.id+':'+id,title,label:title,records:topicRecords,open,total:topicRecords.length,
        itemIds:open.map(r=>r.item.id),attention:open.filter(r=>r.state.view==='attention').length,
        example:quote,method:'literal-repeat-v2'};
    }).filter(t=>t.open.length).sort((a,b)=>b.open.length-a.open.length||b.total-a.total||a.id.localeCompare(b.id));
    if(topics.length)groups.push({post,topics,total:topics.reduce((n,t)=>n+t.total,0),count:topics.reduce((n,t)=>n+t.open.length,0)});
  }
  return groups.sort((a,b)=>b.count-a.count||a.post.id.localeCompare(b.post.id));
}

// A publication stays visible even when no repeated wording or open work exists.
export function buildPostDiscussions(records, posts, order='newest') {
  const topicsByPost=new Map(buildPostTopics(records,posts).map(group=>[group.post.id,group.topics]));
  const byPost=new Map();
  for(const record of records){
    if(record.state.view==='deleted')continue;
    if(!byPost.has(record.postId))byPost.set(record.postId,new Map());
    byPost.get(record.postId).set(record.item.id,record);
  }
  const groups=posts.map(post=>{
    const rows=[...(byPost.get(post.id)?.values()||[])];
    const dates=rows.flatMap(record=>[record.createdAt,...record.messages.filter(message=>!message.deleted).map(message=>message.createdAt)]).map(Date.parse).filter(Number.isFinite);
    return {post,topics:topicsByPost.get(post.id)||[],records:rows,total:rows.length,count:rows.filter(isOpen).length,lastActivity:dates.length?new Date(Math.max(...dates)).toISOString():null};
  });
  return sortDiscussions(groups,order);
}

export function sortDiscussions(groups, order='newest') {
  return [...groups].sort((a,b)=>{
    const left=Date.parse(a.lastActivity),right=Date.parse(b.lastActivity);
    // Undated/quiet imports stay below dated discussions in either direction.
    if(!Number.isFinite(left))return Number.isFinite(right)?1:a.post.id.localeCompare(b.post.id);
    if(!Number.isFinite(right))return -1;
    return (order==='oldest'?left-right:right-left)||a.post.id.localeCompare(b.post.id);
  });
}


// LikeAvto owner permits exact normalized video titles as a sharing key.
// Similar titles, generic captions and foreign accounts remain separate.
export function mediaUrlIdentity(value) {
  try {
    const url=new URL(value);
    if(!['http:','https:'].includes(url.protocol)||url.username||url.password||url.port)return null;
    const host=url.hostname.toLowerCase(),path=url.pathname;
    if(['youtube.com','www.youtube.com','m.youtube.com','music.youtube.com','youtu.be'].includes(host)){
      const key=host==='youtu.be'?/^\/([^/]+)\/?$/.exec(path)?.[1]:path==='/watch'&&url.searchParams.getAll('v').length===1?url.searchParams.get('v'):/^\/(?:shorts|embed)\/([^/]+)\/?$/.exec(path)?.[1];
      return /^[A-Za-z0-9_-]{11}$/.test(key||'')?'yt:'+key:null;
    }
    if(['vk.com','www.vk.com','m.vk.com','vkvideo.ru','www.vkvideo.ru'].includes(host)){
      const key=/^\/video(-?\d+_\d+)\/?$/.exec(path)?.[1];return key?'vk:'+key:null;
    }
    if(['instagram.com','www.instagram.com'].includes(host)){
      const key=/^\/(?:reel|reels|p)\/([A-Za-z0-9_-]+)\/?$/.exec(path)?.[1];return key?'ig:'+key:null;
    }
  }catch{}
  return null;
}

const foreignAccount=(record,account)=>[record.account,record.accountId,record.scope?.account,record.sourceMediaScope?.account,record.connectorBinding?.accountId].some(value=>value&&value!==account);
export function mediaIdentities(record={},account) {
  const foreign=foreignAccount(record,account);
  if(foreign)return new Set();
  const ids=new Set();
  const add=(row,attachment=false)=>{
    if(foreignAccount(row,account))return;
    for(const field of ['contentSha256','mediaSha256'])if(/^[a-fA-F0-9]{64}$/.test(row[field]||''))ids.add('sha:'+row[field].toLowerCase());
    if(typeof row.canonicalMediaId==='string'&&row.canonicalMediaId.trim())ids.add('canonical:'+row.canonicalMediaId);
    for(const field of attachment?['sourceUrl','source_url','url']:['sourceUrl']){const key=mediaUrlIdentity(row[field]);if(key)ids.add(key);}
  };
  add(record);
  for(const attachment of record.attachments||[])if(attachment?.type==='video')add(attachment,true);
  return ids;
}

export function mediaTitle(record={}) {
  const generic=value=>!value||/^(?:none|null|video|видео|публикация(?: likeavto)?|video by .+|clip by .+)$/i.test(value.trim());
  let title=typeof record.title==='string'?record.title.trim():'';
  if(generic(title))title=String(record.text||record.body||'').replace(/<br\s*\/?\s*>/gi,'\n').split(/\r?\n/).find(line=>line.trim())?.trim()||'';
  const words=title.toLowerCase().split(/\s+/).filter(Boolean);
  while(words.at(-1)?.startsWith('#'))words.pop();
  title=words.join(' ');
  return generic(title)||!/[\p{L}\p{N}]/u.test(title)?'':title;
}

export function mediaTitleConflict(left={},right={},account='LikeAvto') {
  const a=mediaIdentities(left,account),b=mediaIdentities(right,account);
  for(const prefix of ['sha:','canonical:']){
    const x=[...a].filter(s=>s.startsWith(prefix)),y=[...b].filter(s=>s.startsWith(prefix));
    if(x.length&&y.length&&!x.some(s=>y.includes(s)))return true;
  }
  return false;
}

export function sameMediaTitle(left={},right={},account='LikeAvto') {
  if(account!=='LikeAvto'||foreignAccount(left,account)||foreignAccount(right,account))return false;
  const video=record=>record.attachments?.some(a=>a.type==='video')||mediaIdentities(record,account).size>0;
  if(!video(left)||!video(right))return false;
  const title=mediaTitle(left);
  return !!title&&title===mediaTitle(right)&&!mediaTitleConflict(left,right,account);
}

export function equivalentMediaPosts(post,posts=[],account='LikeAvto') {
  if(foreignAccount(post,account))return [];
  const target=mediaIdentities(post,account),title=mediaTitle(post);
  const peers=posts.filter(p=>!foreignAccount(p,account)&&title&&mediaTitle(p)===title);
  const conflict=peers.some((p,index)=>peers.slice(index+1).some(q=>mediaTitleConflict(p,q,account)));
  return posts.filter(p=>p!==post&&(!post.postKey||p.postKey!==post.postKey)&&!foreignAccount(p,account)&&(
    [...mediaIdentities(p,account)].some(id=>target.has(id))||!conflict&&sameMediaTitle(post,p,account)
  ));
}

// Unknown completeness is never promoted to full. Source history stays intact;
// only the one canonical reading/context transcript is chosen here.
export function compareTranscripts(a,b) {
  const coverage=m=>[m.transcription?.partial===false?1:0,Number.isFinite(m.transcription?.maxAudioSeconds)?Math.max(0,m.transcription.maxAudioSeconds):0,Date.parse(m.updatedAt||m.sourceDate||m.createdAt||'')||0];
  const x=coverage(a),y=coverage(b);
  for(let i=0;i<x.length;i++)if(x[i]!==y[i])return y[i]-x[i];
  const aid=String(a.id||a.sourceMaterialId||''),bid=String(b.id||b.sourceMaterialId||'');
  return aid<bid?-1:aid>bid?1:0;
}

export function postTranscripts(post,posts=[],materials=[],account='LikeAvto') {
  if(foreignAccount(post,account))return [];
  const target=mediaIdentities(post,account),byKey=new Map(posts.map(row=>[row.postKey,row]));
  const title=mediaTitle(post),peers=posts.filter(p=>!foreignAccount(p,account)&&title&&mediaTitle(p)===title);
  const titleConflict=peers.some((p,index)=>peers.slice(index+1).some(q=>mediaTitleConflict(p,q,account)));
  const selected=[];
  for(const material of materials){
    if(material.kind!=='transcript'||!String(material.text||'').trim())continue;
    if(foreignAccount(material,account))continue;
    const direct=!!post.postKey&&material.postKey===post.postKey;
    const origin=byKey.get(material.postKey);
    const tokens=new Set([...mediaIdentities(material,account),...mediaIdentities(origin,account)]);
    const proof=[...target].find(token=>tokens.has(token));
    const titleProof=!titleConflict&&origin&&sameMediaTitle(post,origin,account)&&!mediaTitleConflict(post,material,account);
    if(!direct&&!proof&&!titleProof)continue;
    selected.push({...material,shared:!direct,match:direct?'postKey':proof||'title:'+mediaTitle(post)});
  }
  return selected.sort(compareTranscripts).slice(0,1);
}

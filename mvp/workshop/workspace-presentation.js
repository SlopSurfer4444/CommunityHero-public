import {filterList,presentedOutcome} from './list-filters.js';
import {equivalentMediaPosts} from './post-transcripts.js';
import {mediaPreparationHold} from './preparation-readiness.js';

export function publicationTitle(post={}) {
  const clean=value=>String(value||'').replace(/#[\p{L}\p{N}_]+/gu,'').replace(/\s+/g,' ').trim();
  const title=clean(post.title);
  const generic=!title||/^публикация(?:\s+like\s*avto)?$/iu.test(title)||/^video by\b/i.test(title);
  if(!generic)return title;
  const text=clean(post.text||post.excerpt);
  if(!text)return 'Пост без заголовка';
  return text.length>160?text.slice(0,157)+'…':text;
}

export function outcomeCounts(records, options) {
  const matching=filterList(records,{...options,outcome:'all'});
  const outcome=record=>presentedOutcome(record.state);
  return {all:matching.length,reply:matching.filter(r=>outcome(r)==='reply').length,no_reply:matching.filter(r=>outcome(r)==='no_reply').length};
}

export function queueTags(state,item,{stale=false,currentView=null,currentOutcome=null}={}) {
  const outcome=presentedOutcome(state);
  let status;
  const mediaHold=mediaPreparationHold(item);
  if(mediaHold&&!['closed','deleted'].includes(state.view))status={label:mediaHold.label,tone:mediaHold.status==='media_wait'?'working':'attention'};
  else if(stale)status={label:'Перепроверить',tone:'attention'};
  else if(state.view==='prepared'||state.view==='closed')status={label:outcome==='no_reply'?'Без ответа':'С ответом',tone:outcome==='no_reply'?'quiet':'reply'};
  else if(state.view==='deleted')status={label:'Удалён',tone:'quiet'};
  else if(state.view==='waiting')status={label:'Ждём уточнение',tone:'attention'};
  const preparation={stale:['Нужна проверка','attention'],queued:['В очереди','quiet'],running:['Готовится','working'],prepared:['Перепроверить','attention'],needs_attention:['Нужно участие','attention'],error:['Ошибка разбора','error']}[item.autoPreparation?.status];
  status ||= preparation?{label:preparation[0],tone:preparation[1]}:{label:state.draft?.trim()?'Черновик':'Новый',tone:state.draft?.trim()?'working':'quiet'};
  const categories={complaint:['Жалоба','error'],needs_fact:['Нужен факт','attention'],moderation:['Модерация','attention'],missing_context:['Мало контекста','attention'],purchase:['Покупка','reply'],question:['Вопрос','working'],feedback:['Отклик','reply']};
  const source=item.triageTags||item.autoPreparation?.tags||[];
  const structured=[...new Set(Array.isArray(source)?source:[])].filter(key=>categories[key]);
  const reason=String(item.autoPreparation?.reason||item.reason||'').toLocaleLowerCase('ru');
  const inferred=[];
  if(!structured.length) {
    if(/(?:нужн|требует).{0,40}(?:подтвержд|провер.{0,12}факт|точн.{0,12}данн)|нет подтвержд.{0,15}(?:данн|факт)|нужен факт/u.test(reason))inferred.push('needs_fact');
    if(/нецензур|матерн|брань|модераци/u.test(reason))inferred.push('moderation');
    if(/жалоб|претензи/u.test(reason))inferred.push('complaint');
  }
  const tags=(structured.length?structured:inferred).slice(0,2).map(key=>({label:categories[key][0],tone:categories[key][1],inferred:!structured.length}));
  const redundantAttention=currentView==='attention'&&status.label==='Нужно участие';
  const redundantOutcome=!stale&&currentView===state.view&&['prepared','closed'].includes(currentView)&&['reply','no_reply'].includes(currentOutcome)&&currentOutcome===outcome;
  return redundantAttention||redundantOutcome?tags:[status,...tags];
}

const safeImage=value=>typeof value==='string' && /^(https?:\/\/|\/(?!\/))/i.test(value) && !/\.(?:mp4|webm|mov|m3u8)(?:[?#]|$)/i.test(value)?value:null;
const youtubeId=value=>{
  try {
    const url=new URL(value);
    if(!['http:','https:'].includes(url.protocol))return null;
    const host=url.hostname.toLowerCase().replace(/^www\./,'');
    const id=host==='youtu.be'?url.pathname.split('/')[1]:['youtube.com','m.youtube.com','music.youtube.com','youtube-nocookie.com'].includes(host)?url.pathname==='/watch'?url.searchParams.get('v'):/^\/(?:shorts|embed|live)\/([^/]+)/.exec(url.pathname)?.[1]:null;
    return id&&/^[A-Za-z0-9_-]{11}$/.test(id)?id:null;
  }catch{return null;}
};
const ownThumbnailCandidates=post=>{
  const candidates=[],add=value=>{const safe=safeImage(value);if(safe&&!candidates.includes(safe))candidates.push(safe);};
  const direct=[post.thumbnailUrl,post.thumbnail_url,post.previewUrl,post.preview_url];
  direct.forEach(add);
  for(const media of Array.isArray(post.attachments)?post.attachments:[]) {
    if(!media || typeof media!=='object')continue;
    const previews=[media.thumbnailUrl,media.thumbnail_url,media.previewUrl,media.preview_url,media.preview?.url,media.thumbnail?.url,typeof media.preview==='string'?media.preview:null];
    if(['photo','image'].includes(String(media.type||media.kind).toLowerCase()))previews.push(media.url,media.src);
    previews.forEach(add);
  }
  const mediaSources=[post.sourceUrl,post.attachmentSourceUrl];
  for(const media of Array.isArray(post.attachments)?post.attachments:[])if(media&&typeof media==='object')mediaSources.push(media.sourceUrl,media.source_url,media.url);
  for(const value of mediaSources){const id=youtubeId(value);if(id){add(`https://i.ytimg.com/vi/${id}/hqdefault.jpg`);add(`https://i.ytimg.com/vi/${id}/mqdefault.jpg`);}}
  return candidates;
};
export function postThumbnailCandidates(post={},posts=[],account='LikeAvto') {
  const candidates=ownThumbnailCandidates(post);
  for(const sibling of equivalentMediaPosts(post,posts,account))for(const value of ownThumbnailCandidates(sibling))if(!candidates.includes(value))candidates.push(value);
  return candidates;
}
export function postThumbnail(post={},posts=[],account='LikeAvto') {
  return postThumbnailCandidates(post,posts,account)[0]||null;
}

export function initializeOverviewPeriod(saved) {
  if(saved.overviewAllPostsDefaultVersion===1)return;
  if(!saved.overviewPeriod || !saved.overviewPeriodExplicit&&!saved.overviewFrom&&!saved.overviewTo)saved.overviewPeriod='all';
  saved.overviewAllPostsDefaultVersion=1;
}

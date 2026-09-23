import {accountDefinition,reject,validateScope} from './config.mjs';
import dns from 'node:dns/promises';
import https from 'node:https';
import {isIP} from 'node:net';

// This is the current connector's projection of a public media source. A
// replacement connector owns its own URL admission and source discovery.
const HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru','youtube.com','youtu.be','instagram.com','tiktok.com']);
const VK_HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru']);
const VK_MEDIA_HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru','vkuseraudio.net','vkuser.net','userapi.com','vk-cdn.net']);
function hostIn(host,names){return [...names].some(name=>host===name||host.endsWith(`.${name}`));}
function publicAddress(address){
  if(isIP(address)!==4)return false;
  const [a,b,c]=address.split('.').map(Number);
  return !(a===0||a===10||a===127||a>=224||a===169&&b===254||a===172&&b>=16&&b<=31||
    a===192&&(b===168||b===0||b===2)||a===100&&b>=64&&b<=127||
    a===198&&(b===18||b===19||b===51&&c===100)||a===203&&b===0&&c===113);
}
function allowedUrl(raw,hosts){
  if(typeof raw!=='string'||raw.length>8192||/[\x00-\x20\x7f]/.test(raw))return null;
  try{const url=new URL(raw);const host=url.hostname.toLowerCase().replace(/\.$/,'');
    if(url.protocol!=='https:'||url.username||url.password||(url.port&&url.port!=='443')||isIP(host)||!hostIn(host,hosts))return null;
    return url;
  }catch{return null;}
}
async function vkPage(url,{lookup=dns.lookup,request=https.request,signal=AbortSignal.timeout(45000)}={}){
  let current=allowedUrl(url,VK_HOSTS);
  if(!current)throw new Error('VK page URL is invalid');
  for(let hop=0;hop<=2;hop++){
    signal.throwIfAborted();
    const answers=await new Promise((resolve,reject)=>{
      const aborted=()=>reject(new Error('VK page DNS timeout'));
      signal.addEventListener('abort',aborted,{once:true});
      Promise.resolve().then(()=>lookup(current.hostname,{all:true,family:4})).then(resolve,reject)
        .finally(()=>signal.removeEventListener('abort',aborted));
    });
    if(!answers.length||answers.some(entry=>!publicAddress(entry.address)))throw new Error('VK page host is not public');
    const result=await new Promise((resolve,reject)=>{
      const req=request(current,{method:'GET',agent:false,signal,headers:{'User-Agent':'Mozilla/5.0',Accept:'text/html'},
        lookup:(_host,options,cb)=>cb(null,options.all?[{address:answers[0].address,family:4}]:answers[0].address,4)},res=>{
        if([301,302,303,307,308].includes(res.statusCode)){res.resume();resolve({redirect:res.headers.location});return;}
        if(res.statusCode!==200){res.resume();reject(new Error('VK page unavailable'));return;}
        if(Number(res.headers['content-length'])>12*1024*1024){res.destroy();reject(new Error('VK page too large'));return;}
        const parts=[];let size=0;
        res.on('data',chunk=>{size+=chunk.length;if(size>12*1024*1024){res.destroy();reject(new Error('VK page too large'));}else parts.push(chunk);});
        res.on('end',()=>resolve({html:Buffer.concat(parts).toString('utf8')}));res.on('error',reject);
      });
      req.setTimeout(45000,()=>req.destroy(new Error('VK page timeout')));req.on('error',reject);req.end();
    });
    if(result.redirect){current=allowedUrl(new URL(result.redirect,current).href,VK_HOSTS);if(!current)throw new Error('VK page redirected outside provider');continue;}
    return result.html;
  }
  throw new Error('VK page redirect limit');
}
function vkFallback(html){
  for(const pattern of [/<meta[^>]+property=["']og:video(?::url)?["'][^>]+content=["']([^"']+)/i,
    /["']contentUrl["']\s*:\s*["']([^"']+)/i,/["']url(?:720|480|360|240)["']\s*:\s*["']([^"']+)/i]){
    const match=pattern.exec(html);if(!match)continue;
    const candidate=match[1].replaceAll('&amp;','&').replaceAll('\\/','/').replaceAll('\\u0026','&');
    const url=allowedUrl(candidate,VK_MEDIA_HOSTS);
    if(url)return url.href;
  }
  return '';
}
function publicVideoUrl(raw){
  if(typeof raw!=='string'||raw.length>4096)return '';
  try{
    const url=new URL(raw);
    const host=url.hostname.toLowerCase().replace(/\.$/,'');
    if(url.protocol!=='https:'||url.username||url.password||(url.port&&url.port!=='443')||
       ![...HOSTS].some(name=>host===name||host.endsWith(`.${name}`)))return '';
    if((host==='youtube.com'||host.endsWith('.youtube.com'))&&url.pathname==='/watch'){
      const values=url.searchParams.getAll('v');
      if(values.length!==1||!(/^[A-Za-z0-9_-]{6,64}$/).test(values[0]))return '';
      url.search=`?v=${values[0]}`;
    }else url.search='';
    url.hash='';
    return url.toString();
  }catch{return '';}
}

export async function runMediaSource(req,{validateScopeFn=validateScope,fetchVkPageFn=vkPage}={}){
  const definition=accountDefinition(req?.account);
  const {binding}=await validateScopeFn(req.account);
  if(binding.accountKey!==definition.accountKey||binding.displayName!==definition.displayName)
    reject('ACCOUNT_SCOPE_MISMATCH');
  const post=req.post;
  if(!post||typeof post!=='object'||Array.isArray(post)||typeof req.postId!=='string'||
     req.postId!==post.id||typeof post.postKey!=='string'||!post.postKey||post.postKey.length>256||
     typeof post.objectId!=='string'||!binding.objectIds.includes(post.objectId))reject('MEDIA_POST_INVALID');
  const sources=[];
  if(Array.isArray(post.attachments))for(const entry of post.attachments.slice(0,20)){
    if(entry&&typeof entry==='object'&&!Array.isArray(entry)&&
       (['video','reel','clip'].includes(entry.type)||String(entry.mimeType||'').startsWith('video/'))&&
       (!entry.account||entry.account===binding.displayName))
      sources.push(entry.sourceUrl,entry.source_url,entry.url);
  }
  sources.push(post.attachmentSourceUrl,post.sourceUrl,post.url);
  const sourceUrl=sources.map(publicVideoUrl).find(Boolean);
  if(!sourceUrl)reject('MEDIA_SOURCE_MISSING');
  const title=String(post.mediaTitle||post.title||post.text||post.postKey).trim().slice(0,240)||post.postKey;
  let fallbackUrl='';
  if(allowedUrl(sourceUrl,VK_HOSTS)){
    try{fallbackUrl=vkFallback(await fetchVkPageFn(sourceUrl));}catch{}
  }
  return {sourceUrl,fallbackUrl,title,postKey:post.postKey,account:binding.displayName};
}

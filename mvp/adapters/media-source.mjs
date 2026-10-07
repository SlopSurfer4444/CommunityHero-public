import {accountDefinition,reject,validateScope} from './config.mjs';
import dns from 'node:dns/promises';
import https from 'node:https';
import {isIP} from 'node:net';
import {createHash} from 'node:crypto';

// This is the current connector's projection of a public media source. A
// replacement connector owns its own URL admission and source discovery.
const HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru','youtube.com','youtu.be','instagram.com','tiktok.com']);
const VK_HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru']);
const VK_MEDIA_HOSTS=new Set(['vk.com','vk.ru','vkvideo.ru','vkuseraudio.net','vkuser.net','userapi.com','vk-cdn.net']);
const DISCOVERY_CATEGORIES=new Set(['vk_discovery_timeout','vk_discovery_cancelled','vk_discovery_network','vk_discovery_tls',
  'vk_discovery_auth_required','vk_discovery_http_forbidden','vk_discovery_rate_limited','vk_discovery_unavailable','vk_discovery_http_failed',
  'vk_discovery_invalid_source','vk_discovery_redirect_rejected','vk_discovery_redirect_limit','vk_discovery_too_large',
  'vk_discovery_incomplete_response','vk_discovery_invalid_response','vk_discovery_unknown']);
const DISCOVERY_STAGES=new Set(['source_admission','deadline','dns','http','redirect','locator_parse','unknown']);
function discoveryCategory(error){
  if(DISCOVERY_CATEGORIES.has(error?.code))return error.code;
  if(error?.name==='TimeoutError'||['ETIMEDOUT','ESOCKETTIMEDOUT'].includes(error?.code))return 'vk_discovery_timeout';
  if(error?.name==='AbortError')return 'vk_discovery_cancelled';
  if(['ENOTFOUND','EAI_AGAIN','ECONNRESET','ECONNREFUSED','EHOSTUNREACH','ENETUNREACH'].includes(error?.code))return 'vk_discovery_network';
  if(['CERT_HAS_EXPIRED','DEPTH_ZERO_SELF_SIGNED_CERT','UNABLE_TO_VERIFY_LEAF_SIGNATURE','ERR_TLS_CERT_ALTNAME_INVALID','UNABLE_TO_GET_ISSUER_CERT_LOCALLY'].includes(error?.code))return 'vk_discovery_tls';
  return 'vk_discovery_unknown';
}
const discoveryError=(code,stage)=>Object.assign(new Error('VK source discovery failed'),{code,discoveryStage:stage});
function discoveryFailure(error){return {schemaVersion:1,platform:'vk',status:'failed',category:discoveryCategory(error).replace(/^vk_discovery_/,''),
  stage:DISCOVERY_STAGES.has(error?.discoveryStage)?error.discoveryStage:'unknown'};}
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
export async function vkPage(url,{lookup=dns.lookup,request=https.request,signal=AbortSignal.timeout(45000)}={}){
  let current=allowedUrl(url,VK_HOSTS);
  if(!current)throw discoveryError('vk_discovery_invalid_source','source_admission');
  for(let hop=0;hop<=2;hop++){
    if(signal.aborted)throw discoveryError(discoveryCategory(signal.reason),'deadline');
    const answers=await new Promise((resolve,reject)=>{
      const aborted=()=>reject(signal.reason);
      signal.addEventListener('abort',aborted,{once:true});
      Promise.resolve().then(()=>lookup(current.hostname,{all:true,family:4})).then(resolve,reject)
        .finally(()=>signal.removeEventListener('abort',aborted));
    }).catch(error=>{throw discoveryError(discoveryCategory(signal.aborted?signal.reason:error),'dns');});
    if(!answers.length||answers.some(entry=>!publicAddress(entry.address)))throw discoveryError('vk_discovery_invalid_source','dns');
    const result=await new Promise((resolve,reject)=>{
      const req=request(current,{method:'GET',agent:false,signal,headers:{'User-Agent':'Mozilla/5.0',Accept:'text/html'},
        lookup:(_host,options,cb)=>cb(null,options.all?[{address:answers[0].address,family:4}]:answers[0].address,4)},res=>{
        if([301,302,303,307,308].includes(res.statusCode)){res.resume();resolve({redirect:res.headers.location,isRedirect:true});return;}
        if(res.statusCode!==200){res.resume();const status=res.statusCode;
          reject(discoveryError(status===401?'vk_discovery_auth_required':status===403?'vk_discovery_http_forbidden':status===429?'vk_discovery_rate_limited':status===404||status===410?'vk_discovery_unavailable':status>=500?'vk_discovery_network':'vk_discovery_http_failed','http'));return;}
        if(Number(res.headers['content-length'])>12*1024*1024){res.destroy();reject(discoveryError('vk_discovery_too_large','http'));return;}
        const parts=[];let size=0;
        res.on('data',chunk=>{size+=chunk.length;if(size>12*1024*1024){res.destroy();reject(discoveryError('vk_discovery_too_large','http'));}else parts.push(chunk);});
        res.on('aborted',()=>reject(discoveryError('vk_discovery_incomplete_response','http')));
        res.on('end',()=>resolve({html:Buffer.concat(parts).toString('utf8')}));res.on('error',reject);
      });
      req.setTimeout(45000,()=>req.destroy(discoveryError('vk_discovery_timeout','http')));req.on('error',reject);req.end();
    }).catch(error=>{throw discoveryError(discoveryCategory(signal.aborted?signal.reason:error),'http');});
    if(result.isRedirect){
      try{current=typeof result.redirect==='string'&&result.redirect?allowedUrl(new URL(result.redirect,current).href,VK_HOSTS):null;}catch{current=null;}
      if(!current)throw discoveryError('vk_discovery_redirect_rejected','redirect');continue;
    }
    return result.html;
  }
  throw discoveryError('vk_discovery_redirect_limit','redirect');
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
    if(hostIn(host,VK_HOSTS)&&url.pathname==='/video_ext.php'){
      // yt-dlp's VK embed extractor needs oid/id and, for some public embeds,
      // the access hash. Keep only those validated fields, never page tracking.
      const oid=url.searchParams.getAll('oid');
      const id=url.searchParams.getAll('id');
      const hash=url.searchParams.getAll('hash');
      if(oid.length!==1||id.length!==1||hash.length>1||
         !/^-?\d{1,20}$/.test(oid[0])||!/^[0-9]{1,20}$/.test(id[0])||
         (hash.length===1&&!/^[A-Za-z0-9_-]{8,128}$/.test(hash[0])))return '';
      url.search=`?oid=${oid[0]}&id=${id[0]}${hash.length?`&hash=${hash[0]}`:''}`;
    }else if((host==='youtube.com'||host.endsWith('.youtube.com'))&&url.pathname==='/watch'){
      const values=url.searchParams.getAll('v');
      if(values.length!==1||!(/^[A-Za-z0-9_-]{6,64}$/).test(values[0]))return '';
      url.search=`?v=${values[0]}`;
    }else url.search='';
    url.hash='';
    return url.toString();
  }catch{return '';}
}

const PIN_FIELDS=['schemaVersion','contract','companyId','connectorBinding','postId','postKey','sourceVersion','attachmentIndex','attachmentIdentity'];
const BINDING_FIELDS=['id','workspaceId','accountId','connector','revision','providerAccountId'];
const object=value=>value!==null&&typeof value==='object'&&!Array.isArray(value);
const exact=(value,fields)=>object(value)&&Object.keys(value).length===fields.length&&fields.every(key=>Object.hasOwn(value,key));
const sha=value=>typeof value==='string'&&/^[0-9a-f]{64}$/.test(value);
const bounded=value=>typeof value==='string'&&value.length>0&&Buffer.byteLength(value,'utf8')<=256;
const nativeVideo=entry=>object(entry)&&['video','reel','clip'].includes(entry.type);
const legacyVideo=entry=>object(entry)&&(nativeVideo(entry)||String(entry.mimeType||'').startsWith('video/'));
function sameBinding(left,right){return exact(right,BINDING_FIELDS)&&BINDING_FIELDS.every(key=>left[key]===right[key]);}
function selectedAsset(req,post,scope,definition){
  const pin=req.assetPin;
  if(!exact(pin,PIN_FIELDS)||pin.schemaVersion!==1||pin.contract!=='VideoSpeechAssetPin.v1'||
     !['BAW Russia','LikeAvto'].includes(pin.companyId)||!bounded(pin.postId)||!bounded(pin.postKey)||
     !sha(pin.sourceVersion)||!sha(pin.attachmentIdentity)||!Number.isSafeInteger(pin.attachmentIndex)||
     pin.attachmentIndex<0||pin.attachmentIndex>10000||!exact(pin.connectorBinding,BINDING_FIELDS)||
     !['id','workspaceId','accountId','connector','providerAccountId'].every(key=>bounded(pin.connectorBinding[key]))||
     !Number.isSafeInteger(pin.connectorBinding.revision)||pin.connectorBinding.revision<1)reject('MEDIA_ASSET_INVALID');
  const connector=pin.connectorBinding;
  // The provider scope certifies company/account/object mapping. Native alone
  // certifies workspace/id/revision and the opaque full sourceVersion before
  // and after dispatch; never invent that authority from provider config.
  if(pin.companyId!==scope.displayName||connector.accountId!==scope.displayName||connector.connector!=='angryspace'||
     connector.providerAccountId!==scope.providerAccountId||connector.providerAccountId!==definition.providerAccountId||
     (post.connectorBinding!=null&&!sameBinding(connector,post.connectorBinding)))reject('ACCOUNT_SCOPE_MISMATCH');
  if(pin.postId!==req.postId||pin.postId!==post.id||pin.postKey!==post.postKey||post.attachmentsState==='unknown'||
     !Array.isArray(post.attachments))reject('MEDIA_ASSET_INVALID');
  const attachment=post.attachments[pin.attachmentIndex];
  if(!nativeVideo(attachment))reject('MEDIA_ASSET_INVALID');
  for(const source of [post,attachment])for(const key of ['account','accountId']){
    if(source[key]!=null&&source[key]!==scope.displayName)reject('ACCOUNT_SCOPE_MISMATCH');
  }
  const strings=['type','sourceUrl','source_url','url','id','canonicalMediaId'].map(key=>typeof attachment[key]==='string'?attachment[key]:null);
  const identity=createHash('sha256').update(JSON.stringify(strings),'utf8').digest('hex');
  if(identity!==pin.attachmentIdentity)reject('MEDIA_ASSET_INVALID');
  return {pin:structuredClone(pin),attachment};
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
  let sources=[],assetPin;
  if(Object.hasOwn(req,'assetPin')){
    const selected=selectedAsset(req,post,binding,definition);assetPin=selected.pin;
    sources=[selected.attachment.sourceUrl,selected.attachment.source_url,selected.attachment.url];
  }else{
    let videos=0;
    if(Array.isArray(post.attachments))for(const entry of post.attachments){
      if(!legacyVideo(entry))continue;
      if(++videos>1)reject('MEDIA_ASSET_AMBIGUOUS');
      if(!entry.account||entry.account===binding.displayName)sources.push(entry.sourceUrl,entry.source_url,entry.url);
    }
    sources.push(post.attachmentSourceUrl,post.sourceUrl,post.url);
  }
  const sourceUrl=sources.map(publicVideoUrl).find(Boolean);
  if(!sourceUrl)reject(assetPin?'MEDIA_SELECTED_SOURCE_MISSING':'MEDIA_SOURCE_MISSING');
  const title=String(post.mediaTitle||post.title||post.text||post.postKey).trim().slice(0,240)||post.postKey;
  let fallbackUrl='',sourceDiscovery;
  if(allowedUrl(sourceUrl,VK_HOSTS)){
    try{
      const html=await fetchVkPageFn(sourceUrl);
      if(typeof html!=='string')throw discoveryError('vk_discovery_invalid_response','locator_parse');
      fallbackUrl=vkFallback(html);
      sourceDiscovery={schemaVersion:1,platform:'vk',status:fallbackUrl?'fallback_resolved':'no_supported_locator',category:null,stage:'locator_parse'};
    }catch(error){sourceDiscovery=discoveryFailure(error);}
  }
  return {sourceUrl,fallbackUrl,title,postKey:post.postKey,account:binding.displayName,
    ...(assetPin?{assetPin}:{}),...(sourceDiscovery?{sourceDiscovery}:{})};
}

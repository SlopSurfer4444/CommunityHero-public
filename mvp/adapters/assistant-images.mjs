import fs from 'node:fs/promises';
import {serializeAssistantModelInput} from './assistant-model-context.mjs';
import path from 'node:path';
import https from 'node:https';
import dns from 'node:dns/promises';
import {isIP} from 'node:net';
import {createHash} from 'node:crypto';

// This is our per-run admission budget, not a claimed provider image limit.
// The same pinned Codex CLI image transport is already used for up to 32
// frames in media vision. Keep the previous worst-case byte/pixel envelope
// (8 images x their individual limits) while admitting photo-heavy posts.
const MAX_BYTES=8*1024*1024, MAX_IMAGES=16;
const MAX_TOTAL_BYTES=8*MAX_BYTES, MAX_TOTAL_PIXELS=8*24_000_000;
const IMAGE_CONCURRENCY=4, SOURCE_TIMEOUT_MS=15000, TOTAL_TIMEOUT_MS=45000;
const fail=(message,mediaCause)=>Object.assign(new Error(message),{code:'ASSISTANT_MEDIA_UNAVAILABLE',...(mediaCause?{mediaCause}:{})});
const IMAGE_FAILURES=new Set(['image_network','image_tls','image_rate_limited','image_auth_required','image_http_forbidden','image_unavailable','image_http_failed','image_source_timeout','image_total_timeout','image_cancelled','image_invalid_source','image_too_large','image_integrity','image_unsupported_format','image_redirect_limit','image_unknown']);
const imageError=(message,imageCategory,extra={})=>Object.assign(fail(message),{imageCategory,...extra});
function imageFailure(error) {
  let category=IMAGE_FAILURES.has(error?.imageCategory)?error.imageCategory:'image_unknown';
  if(category==='image_unknown'){
    if(['ENOTFOUND','EAI_AGAIN','ECONNREFUSED','ECONNRESET','ETIMEDOUT','EHOSTUNREACH','ENETUNREACH'].includes(error?.code))category='image_network';
    else if(['CERT_HAS_EXPIRED','DEPTH_ZERO_SELF_SIGNED_CERT','UNABLE_TO_VERIFY_LEAF_SIGNATURE','ERR_TLS_CERT_ALTNAME_INVALID','UNABLE_TO_GET_ISSUER_CERT_LOCALLY'].includes(error?.code))category='image_tls';
  }
  return {category,...(category==='image_rate_limited'&&Number.isSafeInteger(error?.retryAfterSeconds)&&error.retryAfterSeconds>=0&&error.retryAfterSeconds<=86400?{retryAfterSeconds:error.retryAfterSeconds}:{})};
}
function cooldown(value) {
  if(typeof value!=='string'||value.length>128)return {};
  const seconds=/^\d+$/.test(value.trim())?Number(value.trim()):/ GMT$/.test(value)?Math.ceil((Date.parse(value)-Date.now())/1000):NaN;
  return Number.isFinite(seconds)&&seconds>=0&&seconds<=86400?{retryAfterSeconds:seconds}:{};
}

const boundedOption=(value,maximum)=>Number.isFinite(value)&&value>=1?Math.min(maximum,Math.floor(value)):maximum;

// A task always settles, including an injected downloader that rejects late.
// The real downloader also receives cancellation to close its request/socket.
async function fetchImage(candidate,download,parentSignal,timeoutMs) {
  const controller=new AbortController();
  const signal=AbortSignal.any([parentSignal,controller.signal]);
  const timer=setTimeout(()=>controller.abort(imageError('Image source deadline exceeded','image_source_timeout')),timeoutMs);
  let aborted,stage='not_started';
  try {
    signal.throwIfAborted();
    const cancelled=new Promise((_,reject)=>{
      aborted=()=>reject(signal.reason);
      signal.addEventListener('abort',aborted,{once:true});
    });
    const downloaded=await Promise.race([
      Promise.resolve().then(()=>{signal.throwIfAborted();stage='acquisition';return download(candidate.url,{signal,candidate});}),
      cancelled,
    ]);
    signal.throwIfAborted();
    stage='validation';
    return {downloaded,format:validateImage(downloaded.bytes,downloaded.mime)};
  } catch(error) {
    const failure=imageFailure(signal.aborted?signal.reason:error);
    if(signal.aborted&&failure.category==='image_unknown')failure.category='image_cancelled';
    return {failed:true,failure:{...failure,stage}};
  } finally {
    clearTimeout(timer);
    if(aborted)signal.removeEventListener('abort',aborted);
  }
}

// URL presence is not proof that the model received the bytes. Unknown remains
// distinct from an explicitly supplied empty attachment list.
export function attachmentEvidence(record) {
  const raw=record.attachments??record.commentAttachments;
  const state=['present','none','unknown'].includes(record.attachmentsState)?record.attachmentsState:null;
  if(raw===undefined)return {attachmentStatus:record.commentAttachmentsPresent===true||state==='present'?'unavailable':'unknown'};
  if(!Array.isArray(raw)||raw.length>20)throw fail('Invalid or excessive comment attachments');
  const attachments=raw.map(a=>{
    if(!a||typeof a!=='object'||Array.isArray(a))throw fail('Invalid comment attachment');
    const result={type:['photo','image','sticker','video'].includes(a.type)?a.type:'unsupported'};
    for(const key of ['url','source_url','preview_url','title'])if(typeof a[key]==='string'){
      if(a[key].length>8192)throw fail('Comment attachment field exceeds limit');
      result[key]=a[key];
    }
    return result;
  });
  return {attachmentStatus:attachments.length?'present':record.commentAttachmentsPresent===true||state==='present'?'unavailable':state==='unknown'?'unknown':'none',attachments};
}

// Posts use the same bounded attachment projection, but never inherit the
// comment-specific compatibility fields from imported comment records.
export function postAttachmentEvidence(record) {
  return attachmentEvidence({attachments:record.attachments,attachmentsState:record.attachmentsState});
}

// An absent selector preserves previously captured request semantics. New
// text-first captures supply an explicit empty selector; pixels are requested
// only for an exact recipient/source question, never from keyword matching.
export function validateVisualSelection(raw,payload) {
  if(raw===undefined)return undefined;
  const invalid=()=>{throw Object.assign(new Error('Invalid exact visual selection'),{code:'ASSISTANT_INVALID_REQUEST'});};
  const exact=(value,keys)=>value&&typeof value==='object'&&!Array.isArray(value)
    &&Object.keys(value).length===keys.length&&keys.every(key=>Object.hasOwn(value,key));
  if(!exact(raw,['version','postImages'])||raw.version!==1||!Array.isArray(raw.postImages)
    ||raw.postImages.length>100||!Array.isArray(payload?.items)||!Array.isArray(payload?.posts))invalid();
  const seen=new Set();
  const postImages=raw.postImages.map(row=>{
    if(!exact(row,['itemId','postId','attachmentIndices','reason'])||typeof row.itemId!=='string'
      ||typeof row.postId!=='string'||typeof row.reason!=='string'||!row.reason.trim()
      ||row.reason.length>500||/[\x00-\x1f\x7f]/.test(row.reason)
      ||!Array.isArray(row.attachmentIndices)||!row.attachmentIndices.length||row.attachmentIndices.length>20
      ||seen.has(row.itemId))invalid();
    seen.add(row.itemId);
    const items=payload.items.filter(item=>item.id===row.itemId);
    if(items.length!==1)invalid();
    let post;try {post=linkedPost(items[0],payload.posts,payload.branches??[]);}catch {invalid();}
    if(!post||post.id!==row.postId)invalid();
    const indices=new Set();
    for(const index of row.attachmentIndices){
      if(!Number.isSafeInteger(index)||index<0||index>=20||indices.has(index)
        ||!['photo','image'].includes(post.attachments?.[index]?.type))invalid();
      indices.add(index);
    }
    return {itemId:row.itemId,postId:row.postId,attachmentIndices:[...indices].sort((a,b)=>a-b),reason:row.reason};
  });
  return {version:1,postImages};
}

function visualSelection(payload) {return validateVisualSelection(payload.visualSelection,payload);}
function selectedPostIndices(selection,item,post) {
  return selection===undefined?(post?.attachments??[]).flatMap((a,index)=>['photo','image'].includes(a.type)?[index]:[])
    :selection.postImages.find(row=>row.itemId===item.id)?.attachmentIndices??[];
}
function postImageObservations(payload,selection,manifest=[]) {
  if(selection===undefined)return undefined;
  return payload.items.flatMap(item=>{
    let post;try {post=linkedPost(item,payload.posts??[],payload.branches??[]);}catch {}
    const postId=post?.id??item.postId??(payload.branches??[]).find(branch=>branch.id===item.branchId)?.postId;
    if(!postId)return [];
    const availableAttachmentIndices=(post?.attachments??[]).flatMap((a,index)=>['photo','image'].includes(a.type)?[index]:[]);
    const selectedAttachmentIndices=selectedPostIndices(selection,item,post);
    const observedAttachmentIndices=manifest.filter(image=>image.origin==='post_attachment'&&image.postId===postId
      &&selectedAttachmentIndices.includes(image.attachmentIndex)
      &&(image.itemId===item.id||image.itemIds?.includes(item.id)))
      .map(image=>image.attachmentIndex).sort((a,b)=>a-b);
    const sourceStatus=!post?'unavailable':['present','none','unknown','unavailable'].includes(post.attachmentStatus)
      ?post.attachmentStatus:Array.isArray(post.attachments)?post.attachments.length?'present':'none':'unknown';
    return [{itemId:item.id,postId,selectionStatus:selectedAttachmentIndices.length?'requested':'not_requested',
      observationStatus:observedAttachmentIndices.length?selectedAttachmentIndices.some(index=>!observedAttachmentIndices.includes(index))?'partial':'observed':'not_observed',
      sourceStatus,availableAttachmentIndices,selectedAttachmentIndices,observedAttachmentIndices}];
  });
}

function selectedPostSourceGaps(payload,selection) {
  if(selection===undefined)return postMediaSourceGaps(payload.items,payload.posts??[],payload.branches??[]);
  return selection.postImages.flatMap(row=>{
    const post=payload.posts.find(post=>post.id===row.postId);
    const reason=post.attachmentStatus==='unavailable'?'missing_post_attachment_metadata'
      :row.attachmentIndices.some(index=>!post.attachments[index].url)?'missing_original_post_image_url':undefined;
    return reason?[{itemId:row.itemId,postId:row.postId,reason}]:[];
  });
}

// Bound the JSON added by stageImages without fetching media. This uses the
// already projected request: actual successes are a subset of these source
// slots, and every possible unavailable recipient is represented below.
// It deliberately overcounts mixed success/failure outcomes.
export function conservativeImageEvidenceBytes(prepared) {
  const payload=prepared?.payload;
  if(!payload||!Array.isArray(payload.items)||!Array.isArray(payload.posts)||!Array.isArray(payload.branches))
    throw fail('Invalid prepared image context');
  const items=payload.items,posts=payload.posts,branches=payload.branches;
  const selection=visualSelection(payload);
  const candidates=new Map(),atRisk=new Set();
  const linked=new Map();
  for(const item of items){
    try {linked.set(item.id,linkedPost(item,posts,branches));}
    catch {linked.set(item.id,null);atRisk.add(item.id);}
  }
  for(const item of items){
    const attachments=item.attachments??[];
    if(attachments.length||item.attachmentStatus==='unavailable'
      ||item.attachmentStatus==='present'&&!attachments.length)atRisk.add(item.id);
    for(const [index,attachment] of attachments.entries()){
      if(!['photo','image','sticker'].includes(attachment.type))continue;
      const source=payload.commentPhotoSources?.find(source=>source.itemId===item.id&&source.attachmentIndex===index);
      candidates.set(JSON.stringify(['comment',item.id,index]),{
        imageNumber:16,itemId:item.id,attachmentIndex:index,origin:'comment_attachment',
        sha256:'0'.repeat(64),mime:'image/jpeg',width:12000,height:12000,
        ...(source?{bytes:MAX_BYTES,sourceRole:source.sourceRole,sourceVersion:source.sourceVersion,acquisitionReceiptSha256:source.acquisitionReceiptSha256}:{})});
    }
    const post=linked.get(item.id);
    const requested=selection===undefined||selection.postImages.some(row=>row.itemId===item.id);
    if(!post){
      if(requested&&(item.postId||branches.some(branch=>branch.id===item.branchId&&branch.postId)))atRisk.add(item.id);
      continue;
    }
    if(requested&&(post.attachmentStatus==='unavailable'||post.attachmentStatus==='present'&&!post.attachments?.length))
      atRisk.add(item.id);
    for(const index of selectedPostIndices(selection,item,post)){
      const recipients=[item.id,...items.filter(other=>other.id!==item.id&&linked.get(other.id)?.id===post.id
        &&selectedPostIndices(selection,other,post).includes(index)).map(other=>other.id)];
      for(const id of recipients)atRisk.add(id);
      const key=JSON.stringify(['post',post.id,index]);
      if(candidates.has(key))continue;
      candidates.set(key,{imageNumber:16,itemId:item.id,itemIds:recipients,postId:post.id,
        attachmentIndex:index,origin:'post_attachment',sha256:'0'.repeat(64),
        mime:'image/jpeg',width:12000,height:12000});
    }
  }
  // A failed image slot can block any recipient it names, even if another
  // source image succeeds. All rows use maximal fixed diagnostic field sizes.
  const unavailableItems=items.filter(item=>atRisk.has(item.id)).map(item=>({
    itemId:item.id,reason:'x'.repeat(64),category:'x'.repeat(32),stage:'x'.repeat(16),retryAfterSeconds:86400}));
  const images=[...candidates.values()].sort((left,right)=>Buffer.byteLength(JSON.stringify(right))-Buffer.byteLength(JSON.stringify(left))).slice(0,16);
  const evidence={status:'no_images_attached',images,branchImagesAttached:false,
    ...(unavailableItems.length?{unavailableItems}:{}),
    ...(selection===undefined?{}:{postImageObservations:postImageObservations(payload,selection,images)
      .map(row=>({...row,observationStatus:'not_observed'}))})};
  return Buffer.byteLength(',"imageEvidence":')+Buffer.byteLength(JSON.stringify(evidence));
}

function linkedPost(item,posts,branches) {
  const matchingBranches=branches.filter(branch=>branch.id===item.branchId);
  if(matchingBranches.length>1)throw fail('Ambiguous branch for comment','source_unavailable');
  const branch=matchingBranches[0];
  if(item.postId&&branch?.postId&&item.postId!==branch.postId)
    throw fail('Comment and branch refer to different posts','source_unavailable');
  const postId=item.postId||branch?.postId;
  const matchingPosts=postId?posts.filter(post=>post.id===postId):[];
  if(matchingPosts.length>1)throw fail('Ambiguous post for comment','source_unavailable');
  return matchingPosts[0];
}

export function postMediaSourceGaps(items,posts=[],branches=[]) {
  if(!Array.isArray(items)||!Array.isArray(posts)||!Array.isArray(branches))throw fail('Invalid post evidence');
  const gaps=[];
  for(const item of items){
    const post=linkedPost(item,posts,branches);
    if(!post){
      const postId=item.postId||branches.find(branch=>branch.id===item.branchId)?.postId;
      if(postId)gaps.push({itemId:item.id,postId,reason:'missing_post_record'});
      continue;
    }
    const attachments=post.attachments??[];
    if(!Array.isArray(attachments))throw fail('Invalid post attachments');
    let reason;
    if(post.attachmentStatus==='unavailable'||post.attachmentStatus==='present'&&!attachments.length)
      reason='missing_post_attachment_metadata';
    else for(const attachment of attachments){
      if(!attachment||typeof attachment!=='object'||Array.isArray(attachment))throw fail('Invalid post attachment');
      if(['photo','image'].includes(attachment.type)&&!attachment.url){reason='missing_original_post_image_url';break;}
    }
    if(reason)gaps.push({itemId:item.id,postId:post.id,reason});
  }
  return gaps;
}

// These gaps are proven by the supplied comment record, before any network
// request. A failed download or invalid image is a separate condition.
export function commentMediaSourceGaps(items) {
  if(!Array.isArray(items))throw fail('Invalid comment items');
  const gaps=[];
  for(const item of items){
    const attachments=item.attachments??[];
    if(!Array.isArray(attachments))throw fail('Invalid comment attachments');
    let reason;
    if(item.attachmentStatus==='unavailable'||item.attachmentStatus==='present'&&!attachments.length)
      reason='missing_attachment_metadata';
    else for(const attachment of attachments){
      if(!attachment||typeof attachment!=='object'||Array.isArray(attachment))throw fail('Invalid comment attachment');
      if(attachment.type==='unsupported'&&!attachment.url&&!attachment.source_url&&!attachment.preview_url)
        reason='missing_attachment_locator';
      else if(['photo','image','sticker'].includes(attachment.type)&&!attachment.url)
        reason='missing_original_image_url';
      if(reason)break;
    }
    if(reason)gaps.push({itemId:item.id,reason});
  }
  return gaps;
}

export function imageUrl(value) {
  if(typeof value!=='string'||value.length>8192||/[\x00-\x20\x7f]/.test(value))throw fail('Invalid image URL');
  let u;try{u=new URL(value);}catch{throw fail('Invalid image URL');}
  if(u.protocol!=='https:'||u.username||u.password||u.port&&u.port!=='443'||isIP(u.hostname)||!u.hostname.includes('.')||u.hostname.endsWith('.local')||u.hostname.endsWith('.localhost')||u.hostname.includes(':'))throw fail('Image URL must use public HTTPS');
  if([...u.searchParams.keys()].some(k=>/^(access_token|refresh_token|authorization|password|cookie)$/i.test(k)))throw fail('Image URL contains credentials');
  u.hash='';return u;
}
export function publicAddress(address) {
  if(isIP(address)!==4)return false;
  const [a,b,c]=address.split('.').map(Number);
  return !(a===0||a===10||a===127||a>=224||a===169&&b===254||a===172&&b>=16&&b<=31||a===192&&(b===168||b===0||b===2)||a===100&&b>=64&&b<=127||a===198&&(b===18||b===19||b===51&&c===100)||a===203&&b===0&&c===113);
}

// Resolve every redirect, reject mixed private/public answers, and pin the
// actual socket lookup to that answer. No ambient proxy, cookies or auth.
export async function downloadImage(value,{lookup=dns.lookup,request=https.request,signal=AbortSignal.timeout(15000)}={}) {
  let url;try{url=imageUrl(value);}catch{throw imageError('Invalid image source','image_invalid_source');}
  for(let hop=0;hop<=2;hop++){
    signal?.throwIfAborted();
    const answers=await new Promise((resolve,reject)=>{
      const aborted=()=>{const category=imageFailure(signal.reason).category;
        reject(imageError('Image DNS lookup aborted',category!=='image_unknown'?category:signal.reason?.name==='TimeoutError'?'image_source_timeout':'image_cancelled'));};
      signal.addEventListener('abort',aborted,{once:true});
      Promise.resolve().then(()=>lookup(url.hostname,{all:true,family:4})).then(resolve,reject)
        .finally(()=>signal.removeEventListener('abort',aborted));
    });
    if(!answers.length||answers.some(a=>!publicAddress(a.address)))throw imageError('Image host is not public','image_invalid_source');
    const response=await new Promise((resolve,reject)=>{
      const req=request(url,{method:'GET',agent:false,signal,headers:{Accept:'image/png,image/jpeg,image/webp'},lookup:(_host,options,cb)=>cb(null,options.all?[{address:answers[0].address,family:4}]:answers[0].address,4)},res=>{
        if([301,302,303,307,308].includes(res.statusCode)){res.resume();resolve({redirect:res.headers.location});return;}
        if(res.statusCode!==200){res.resume();const status=res.statusCode;
          reject(imageError('Image download failed',status===429?'image_rate_limited':status===401?'image_auth_required':status===403?'image_http_forbidden':status===404||status===410?'image_unavailable':status>=500?'image_network':'image_http_failed',status===429?cooldown(res.headers['retry-after']):{}));return;}
        if(Number(res.headers['content-length'])>MAX_BYTES){res.destroy();reject(imageError('Image is too large','image_too_large'));return;}
        const parts=[];let size=0;
        res.on('data',chunk=>{size+=chunk.length;if(size>MAX_BYTES){res.destroy();reject(imageError('Image is too large','image_too_large'));}else parts.push(chunk);});
        res.on('aborted',()=>reject(imageError('Image response incomplete','image_integrity')));
        res.on('end',()=>{const expected=res.headers['content-length'];
          if(expected!==undefined&&(!/^\d+$/.test(String(expected))||Number(expected)!==size)){reject(imageError('Image response incomplete','image_integrity'));return;}
          resolve({bytes:Buffer.concat(parts),mime:String(res.headers['content-type']||'').split(';')[0].toLowerCase()});});
        res.on('error',reject);
      });
      req.setTimeout(15000,()=>req.destroy(imageError('Image download timed out','image_source_timeout')));
      req.on('error',reject);req.end();
    });
    if(response.redirect){try{url=imageUrl(new URL(response.redirect,url).href);}catch{throw imageError('Invalid image redirect','image_invalid_source');}continue;}
    return response;
  }
  throw imageError('Image redirect limit exceeded','image_redirect_limit');
}

const CRC_TABLE=Uint32Array.from({length:256},(_,i)=>{let c=i;for(let bit=0;bit<8;bit++)c=c&1?0xedb88320^(c>>>1):c>>>1;return c>>>0;});
function crc32(bytes,start,end){let c=0xffffffff;for(let at=start;at<end;at++)c=CRC_TABLE[(c^bytes[at])&255]^(c>>>8);return (c^0xffffffff)>>>0;}
const corrupt=()=>{throw imageError('Invalid or incomplete image container','image_integrity');};
function pngDimensions(bytes){
  if(bytes.length<8||!bytes.subarray(0,8).equals(Buffer.from('89504e470d0a1a0a','hex')))return corrupt();
  let at=8,width,height,color,seenPalette=false,seenData=false,dataBytes=0,dataEnded=false;
  while(at+12<=bytes.length){
    const size=bytes.readUInt32BE(at),end=at+12+size;if(end>bytes.length)return corrupt();
    const kind=bytes.toString('ascii',at+4,at+8);
    if(!/^[A-Za-z]{4}$/.test(kind)||crc32(bytes,at+4,end-4)!==bytes.readUInt32BE(end-4))return corrupt();
    if(at===8){
      if(kind!=='IHDR'||size!==13)return corrupt();width=bytes.readUInt32BE(at+8);height=bytes.readUInt32BE(at+12);color=bytes[at+17];
      const depth=bytes[at+16];if(!({0:[1,2,4,8,16],2:[8,16],3:[1,2,4,8],4:[8,16],6:[8,16]}[color]?.includes(depth))||bytes[at+18]!==0||bytes[at+19]!==0||bytes[at+20]>1)return corrupt();
    }else if(kind==='IHDR')return corrupt();
    else if(kind==='PLTE'){if(seenPalette||seenData||size===0||size%3||size>768)return corrupt();seenPalette=true;}
    else if(kind==='IDAT'){if(dataEnded||color===3&&!seenPalette)return corrupt();seenData=true;dataBytes+=size;}
    else if(kind==='IEND'){if(size!==0||!dataBytes||end!==bytes.length)return corrupt();return {width,height};}
    else if(kind[0]===kind[0].toUpperCase())return corrupt();
    if(seenData&&kind!=='IDAT')dataEnded=true;
    at=end;
  }
  return corrupt();
}
function jpegDimensions(bytes){
  if(bytes.length<4||bytes[0]!==255||bytes[1]!==216)return corrupt();
  let at=2,width,height,seenScan=false;const components=new Set();
  while(at<bytes.length){
    if(bytes[at++]!==255)return corrupt();while(bytes[at]===255)at++;
    const marker=bytes[at++];if(marker===217){if(!seenScan||!width||at!==bytes.length)return corrupt();return {width,height};}
    if(marker===undefined||marker===0||marker===216||marker>=208&&marker<=215)return corrupt();
    if(marker===1)continue;
    if(at+2>bytes.length)return corrupt();const size=bytes.readUInt16BE(at),end=at+size;
    if(size<2||end>bytes.length)return corrupt();
    if([192,193,194,195,197,198,199,201,202,203,205,206,207].includes(marker)){
      if(width||size<11)return corrupt();const count=bytes[at+7];
      if(!count||count>4||size!==8+3*count||![8,12,16].includes(bytes[at+2]))return corrupt();
      height=bytes.readUInt16BE(at+3);width=bytes.readUInt16BE(at+5);
      for(let i=0;i<count;i++){const id=bytes[at+8+3*i],sampling=bytes[at+9+3*i];if(components.has(id)||!(sampling>>4)||!(sampling&15)||(sampling>>4)>4||(sampling&15)>4)return corrupt();components.add(id);}
    }
    if(marker===218){
      const count=bytes[at+2];if(!width||!height||!count||size!==6+2*count||count>components.size)return corrupt();
      const scanIds=new Set();for(let i=0;i<count;i++){const id=bytes[at+3+2*i];if(!components.has(id)||scanIds.has(id))return corrupt();scanIds.add(id);}
      at=end;let entropy=0;
      while(at<bytes.length){
        if(bytes[at]!==255){entropy++;at++;continue;}
        const start=at;while(bytes[at]===255)at++;
        if(bytes[at]===0||bytes[at]>=208&&bytes[at]<=215){entropy++;at++;continue;}
        at=start;break;
      }
      if(!entropy)return corrupt();seenScan=true;continue;
    }
    at=end;
  }
  return corrupt();
}
function webpBitstream(bytes,start,size,kind){
  if(kind==='VP8 '){
    if(size<11||!bytes.subarray(start+3,start+6).equals(Buffer.from([157,1,42])))return corrupt();
    const tag=bytes.readUIntLE(start,3);if(tag&1||(tag>>>5)>size-10)return corrupt();
    return {width:bytes.readUInt16LE(start+6)&16383,height:bytes.readUInt16LE(start+8)&16383};
  }
  if(kind==='VP8L'){
    if(size<6||bytes[start]!==47)return corrupt();const bits=bytes.readUInt32LE(start+1);if(bits>>>29)return corrupt();
    return {width:(bits&16383)+1,height:((bits>>>14)&16383)+1};
  }
  return corrupt();
}
function webpDimensions(bytes){
  if(bytes.length<20||bytes.toString('ascii',0,4)!=='RIFF'||bytes.toString('ascii',8,12)!=='WEBP'||bytes.readUInt32LE(4)+8!==bytes.length)return corrupt();
  let at=12,canvas,image,flags=0,animation=false,frames=0;
  while(at+8<=bytes.length){
    const kind=bytes.toString('ascii',at,at+4),size=bytes.readUInt32LE(at+4),start=at+8,end=start+size+(size&1);
    if(end>bytes.length||size&1&&bytes[end-1]!==0)return corrupt();
    if(kind==='VP8X'){
      if(at!==12||size!==10||bytes[start]&0xc1||bytes.readUIntLE(start+1,3)!==0)return corrupt();flags=bytes[start];
      canvas={width:bytes.readUIntLE(start+4,3)+1,height:bytes.readUIntLE(start+7,3)+1};
    }else if(kind==='VP8 '||kind==='VP8L'){
      if(image||frames||flags&2)return corrupt();image=webpBitstream(bytes,start,size,kind);
    }else if(kind==='ANIM'){
      if(!canvas||!(flags&2)||animation||size!==6||frames)return corrupt();animation=true;
    }else if(kind==='ANMF'){
      if(!animation||size<24||bytes[start+15]&0xfc)return corrupt();
      const x=2*bytes.readUIntLE(start,3),y=2*bytes.readUIntLE(start+3,3),width=bytes.readUIntLE(start+6,3)+1,height=bytes.readUIntLE(start+9,3)+1;
      if(x+width>canvas.width||y+height>canvas.height)return corrupt();
      let sub=start+16,frame;
      while(sub+8<=start+size){const subKind=bytes.toString('ascii',sub,sub+4),subSize=bytes.readUInt32LE(sub+4),subEnd=sub+8+subSize+(subSize&1);
        if(subEnd>start+size||subSize&1&&bytes[subEnd-1]!==0)return corrupt();
        if(subKind==='VP8 '||subKind==='VP8L'){if(frame)return corrupt();frame=webpBitstream(bytes,sub+8,subSize,subKind);}
        else if(subKind!=='ALPH')return corrupt();sub=subEnd;
      }
      if(sub!==start+size||!frame||frame.width!==width||frame.height!==height)return corrupt();frames++;
    }else if(!canvas||!['ALPH','ICCP','EXIF','XMP '].includes(kind))return corrupt();
    at=end;
  }
  if(at!==bytes.length||((flags&2)?!frames||image:!image))return corrupt();
  if(canvas&&image&&(canvas.width!==image.width||canvas.height!==image.height))return corrupt();
  return canvas??image;
}
// Structural admission rejects incomplete containers. It does not claim full
// pixel/entropy decoding; the model image transport remains a separate consumer.
export function validateImage(bytes,mime) {
  if(!Buffer.isBuffer(bytes)||!bytes.length)throw imageError('Invalid image size','image_integrity');
  if(bytes.length>MAX_BYTES)throw imageError('Image is too large','image_too_large');
  let dimensions,extension;
  if(mime==='image/png'){dimensions=pngDimensions(bytes);extension='png';}
  else if(mime==='image/jpeg'){dimensions=jpegDimensions(bytes);extension='jpg';}
  else if(mime==='image/webp'){dimensions=webpDimensions(bytes);extension='webp';}
  else throw imageError('Unsupported image format','image_unsupported_format');
  const {width,height}=dimensions;
  if(!width||!height)corrupt();
  if(width>12000||height>12000||width*height>24000000)throw imageError('Image dimensions exceed limit','image_too_large');
  return {extension,width,height};
}

// This is diagnostic provenance, never media delivery or retry authorization.
export function imageFailureMetadata(value=[]){
  if(!Array.isArray(value)||value.length>MAX_IMAGES)throw fail('Invalid image failure provenance');
  return value.map(row=>{
    const post=row?.origin==='post_attachment';
    if(!row||!['comment_attachment','post_attachment'].includes(row.origin)||typeof row.itemId!=='string'||!row.itemId||row.itemId.length>500
      ||!Number.isInteger(row.attachmentIndex)||row.attachmentIndex<0||row.attachmentIndex>=20||!IMAGE_FAILURES.has(row.category)
      ||!['not_started','acquisition','validation'].includes(row.stage)
      ||post&&(typeof row.postId!=='string'||!row.postId.trim()||row.postId.length>500
        ||!Array.isArray(row.itemIds)||!row.itemIds.length||row.itemIds.length>100||row.itemIds[0]!==row.itemId
        ||row.itemIds.some(id=>typeof id!=='string'||!id||id.length>500)||new Set(row.itemIds).size!==row.itemIds.length))throw fail('Invalid image failure provenance');
    const clean={itemId:row.itemId,attachmentIndex:row.attachmentIndex,origin:row.origin,category:row.category,stage:row.stage};
    if(post){clean.postId=row.postId;clean.itemIds=[...row.itemIds];}
    if(row.retryAfterSeconds!==undefined){if(row.category!=='image_rate_limited'||!Number.isSafeInteger(row.retryAfterSeconds)||row.retryAfterSeconds<0||row.retryAfterSeconds>86400)throw fail('Invalid image failure cooldown');clean.retryAfterSeconds=row.retryAfterSeconds;}
    return clean;
  });
}

async function stageImages(prepared,home,{download=downloadImage,loadPostImage,signal:callerSignal,
  concurrency=IMAGE_CONCURRENCY,sourceTimeoutMs=SOURCE_TIMEOUT_MS,totalTimeoutMs=TOTAL_TIMEOUT_MS}={}) {
  const sourceGaps=commentMediaSourceGaps(prepared.payload.items);
  const selection=visualSelection(prepared.payload);
  const postGaps=selectedPostSourceGaps(prepared.payload,selection);
  if(prepared.triage!==false&&prepared.payload.items.length===1&&(sourceGaps.length||postGaps.length))
    throw Object.assign(fail('Image media unavailable at source','source_unavailable'),{unavailableItems:[...sourceGaps,...postGaps]});
  const blocked=new Map([...sourceGaps,...postGaps].map(gap=>[gap.itemId,gap.reason]));
  const failures=new Map();
  const candidates=new Map();
  for(const item of prepared.payload.items){
    if(blocked.has(item.id))continue;
    const needed=[];
    try {
      for(const [index,a] of (item.attachments??[]).entries()){
        if(!['photo','image','sticker'].includes(a.type))throw fail('Comment media requires separate preparation','unsupported_capability');
        const source=prepared.payload.mandatoryMaterialContract==='mandatory_post_materials_v1'
          ?prepared.payload.commentPhotoSources?.find(source=>source.itemId===item.id&&source.attachmentIndex===index):undefined;
        const materialPhoto=source?.photo;
        if(prepared.payload.mandatoryMaterialContract==='mandatory_post_materials_v1'&&!materialPhoto)throw fail('Mandatory comment photo receipt is missing','mandatory_photo_receipt_missing');
        if(materialPhoto&&!loadPostImage)throw fail('Mandatory comment photo loader is missing','mandatory_photo_loader_missing');
        if(!materialPhoto)imageUrl(a.url);
        needed.push({key:JSON.stringify(['comment',item.id,index]),itemId:item.id,itemIds:[item.id],
          attachmentIndex:index,url:a.url,origin:'comment_attachment',...(materialPhoto?{materialPhoto,commentSource:source}:{})});
      }
      const post=linkedPost(item,prepared.payload.posts??[],prepared.payload.branches??[]);
      for(const index of selectedPostIndices(selection,item,post)){
        const a=post.attachments[index];
        const materialPhoto=prepared.payload.mandatoryMaterialContract==='mandatory_post_materials_v1'
          ?prepared.payload.postContextBundle?.members?.find(m=>m.canonicalPostId===post.id)?.assets?.find(v=>v.modality==='photo'&&v.attachmentIndex===index)?.photo:undefined;
        if(materialPhoto&&!loadPostImage)throw fail('Mandatory photo loader is missing','mandatory_photo_loader_missing');
        if(!materialPhoto)imageUrl(a.url);
        // Equal URLs on different posts are distinct sources.
        // Legacy captures bind every attached recipient, including one held
        // for their own attachment. Selected captures bind only the exact
        // recipients that requested this slot; unrelated text stays independent.
        const recipients=[item.id,...prepared.payload.items.filter(other=>other.id!==item.id
          &&linkedPost(other,prepared.payload.posts??[],prepared.payload.branches??[])?.id===post.id
          &&selectedPostIndices(selection,other,post).includes(index)).map(other=>other.id)];
        needed.push({key:JSON.stringify(['post',post.id,index]),itemId:item.id,itemIds:recipients,postId:post.id,
          attachmentIndex:index,url:a.url,origin:'post_attachment',...(materialPhoto?{materialPhoto}:{})});
      }
    } catch(error) {
      if(error.code!=='ASSISTANT_MEDIA_UNAVAILABLE')throw error;
      blocked.set(item.id,error.mediaCause??'media_unavailable');
      if(!error.mediaCause)failures.set(item.id,{category:'image_invalid_source'});
      continue;
    }
    const extra=needed.filter(candidate=>!candidates.has(candidate.key)).length;
    if(candidates.size+extra>MAX_IMAGES){blocked.set(item.id,'image_budget_exceeded');continue;}
    for(const candidate of needed){
      const shared=candidates.get(candidate.key);
      if(shared)continue;
      else candidates.set(candidate.key,candidate);
    }
  }
  const paths=[],manifest=[],failureEvidence=[];
  let totalBytes=0,totalPixels=0;
  const controller=new AbortController();
  const signal=callerSignal?AbortSignal.any([callerSignal,controller.signal]):controller.signal;
  const timer=setTimeout(()=>controller.abort(imageError('Image staging deadline exceeded','image_total_timeout')),boundedOption(totalTimeoutMs,TOTAL_TIMEOUT_MS));
  const ordered=[...candidates.values()],pending=new Map();
  const windowSize=boundedOption(concurrency,IMAGE_CONCURRENCY);
  let next=0;
  const fillWindow=()=>{
    // Do not refill on download completion: a slow first image must not leave
    // the remaining batch buffered. At most four 8 MiB results are retained,
    // plus bounded stream/concat working buffers in downloadImage.
    while(next<ordered.length&&pending.size<windowSize){
      const index=next++,candidate=ordered[index];
      pending.set(index,candidate.itemIds.every(itemId=>blocked.has(itemId))
        ?Promise.resolve({skipped:true})
        :signal.aborted?Promise.resolve({failed:true,failure:{category:controller.signal.aborted?'image_total_timeout':'image_cancelled',stage:'not_started'}})
        :fetchImage(candidate,candidate.materialPhoto?(_url,opts)=>loadPostImage(candidate,opts):download,signal,boundedOption(sourceTimeoutMs,SOURCE_TIMEOUT_MS)));
    }
  };
  try {
    for(const [index,candidate] of ordered.entries()){
      fillWindow();
      const result=await pending.get(index);
      pending.delete(index);
      const failure=!result.skipped&&(result.failed||signal.aborted)
        ?result.failure??{category:controller.signal.aborted?'image_total_timeout':'image_cancelled',stage:'validation'}:undefined;
      if(failure)failureEvidence.push({itemId:candidate.itemId,attachmentIndex:candidate.attachmentIndex,origin:candidate.origin,
        ...(candidate.postId?{postId:candidate.postId,itemIds:[...candidate.itemIds]}:{}),...failure});
      // Recheck after earlier ordered failures/budget holds. Speculative fetches
      // never change source admission, recipient binding or image numbering.
      if(candidate.itemIds.every(itemId=>blocked.has(itemId)))continue;
      if(failure){
        for(const itemId of candidate.itemIds){blocked.set(itemId,'image_unavailable');failures.set(itemId,failure);}
        continue;
      }
      const {downloaded,format}=result;
      const pixels=format.width*format.height;
      if(totalBytes+downloaded.bytes.length>MAX_TOTAL_BYTES||totalPixels+pixels>MAX_TOTAL_PIXELS){
        for(const itemId of candidate.itemIds)blocked.set(itemId,'image_budget_exceeded');
        continue;
      }
      const file=path.join(home,`${candidate.origin==='post_attachment'?'post':'comment'}-image-${paths.length+1}.${format.extension}`);
      await fs.writeFile(file,downloaded.bytes,{flag:'wx',mode:0o600});paths.push(file);
      totalBytes+=downloaded.bytes.length;totalPixels+=pixels;
      manifest.push({imageNumber:paths.length,itemId:candidate.itemId,...(candidate.postId?{itemIds:candidate.itemIds,postId:candidate.postId}:{}),
        attachmentIndex:candidate.attachmentIndex,origin:candidate.origin,
        sha256:createHash('sha256').update(downloaded.bytes).digest('hex'),mime:downloaded.mime,width:format.width,height:format.height,
        ...(candidate.commentSource?{bytes:downloaded.bytes.length,sourceRole:candidate.commentSource.sourceRole,sourceVersion:candidate.commentSource.sourceVersion,
          acquisitionReceiptSha256:candidate.commentSource.acquisitionReceiptSha256}:{})});
    }
  } finally {
    clearTimeout(timer);
    controller.abort(fail('Image staging finished'));
    await Promise.allSettled(pending.values());
    pending.clear();
  }
  const unavailableItems=[...blocked].map(([itemId,reason])=>({itemId,reason,...failures.get(itemId)}));
  if(prepared.triage!==false&&prepared.payload.items.length===1&&unavailableItems.length)
    throw Object.assign(fail('Image media unavailable for recipient',unavailableItems[0].reason==='unsupported_capability'?'unsupported_capability':undefined),{unavailableItems,failureEvidence:imageFailureMetadata(failureEvidence)});
  prepared.payload.imageEvidence={status:unavailableItems.length?(manifest.length?'partial':'unavailable'):manifest.length?'attached':'no_images_attached',
    images:manifest,branchImagesAttached:false,...(unavailableItems.length?{unavailableItems}:{}),
    ...(selection===undefined?{}:{postImageObservations:postImageObservations(prepared.payload,selection,manifest)})};
  prepared.input=serializeAssistantModelInput(prepared.payload);
  return {paths,manifest,blockedItemIds:[...blocked.keys()],unavailableItems,failureEvidence:imageFailureMetadata(failureEvidence)};
}

export async function stageAssistantImages(prepared,home,options={}) {
  try{return await stageImages(prepared,home,options);}
  catch(error){
    // Interactive discussion/search remains useful without pixels. Preparation
    // stays strict; discussion can explain the gap but cannot produce an action
    // for a recipient whose supplied media was not delivered.
    if(prepared.triage!==false||error.code!=='ASSISTANT_MEDIA_UNAVAILABLE')throw error;
    const selection=visualSelection(prepared.payload);
    const sourceBlocked=new Set((error.unavailableItems??[]).map(entry=>entry.itemId));
    const blockedItemIds=prepared.payload.items.filter(item=>
      sourceBlocked.has(item.id)||item.attachmentStatus==='present'||item.attachmentStatus==='unavailable'
      ||item.attachments?.length||(()=>{
        try {
          const post=linkedPost(item,prepared.payload.posts??[],prepared.payload.branches??[]);
          return post&&(selection===undefined||selectedPostIndices(selection,item,post).length)
            &&(post.attachmentStatus==='present'||post.attachmentStatus==='unavailable'
            ||post.attachments?.some(a=>['photo','image'].includes(a.type)));
        } catch {return selection===undefined||selection.postImages.some(row=>row.itemId===item.id);}
      })()).map(item=>item.id);
    prepared.payload.imageEvidence={status:'unavailable',images:[],branchImagesAttached:false,
      unavailableItems:blockedItemIds.map(itemId=>({itemId,reason:'Comment or post image could not be attached; visual contents are unknown. Do not propose any action for this item.'})),
      ...(selection===undefined?{}:{postImageObservations:postImageObservations(prepared.payload,selection)})};
    prepared.input=serializeAssistantModelInput(prepared.payload);
    return {paths:[],manifest:[],blockedItemIds,failureEvidence:imageFailureMetadata(error.failureEvidence)};
  }
}

export function admitImageDependentProposals(result,images) {
  const blocked=new Set(images.blockedItemIds??[]);
  if(!blocked.size)return result;
  const reason='Вложение комментария или фото публикации не удалось прочитать. Содержимое не проверено; нужна ручная проверка.';
  const budgetReason='Фото не были переданы ассистенту из-за лимита изображений в этом пакете. Нужна подготовка меньшим пакетом; содержимое не проверено.';
  const budgetBlocked=new Set((images.unavailableItems??[]).filter(item=>item.reason==='image_budget_exceeded').map(item=>item.itemId));
  return {...result,proposals:result.proposals.filter(proposal=>!blocked.has(proposal.itemId)),
    ...(Array.isArray(result.assessments)?{assessments:result.assessments.map(assessment=>blocked.has(assessment.itemId)
      ?{itemId:assessment.itemId,outcome:'needs_attention',reason:budgetBlocked.has(assessment.itemId)?budgetReason:reason,tags:['missing_context']}:assessment)}:{}),
    ...(Array.isArray(result.evidence)?{evidence:result.evidence.filter(source=>!blocked.has(source.itemId))}:{}),
    text:result.text+'\n\n'+(budgetBlocked.size===blocked.size
      ?'Лимит изображений пакета не позволил передать все фото ассистенту. Для затронутых комментариев нужна подготовка меньшим пакетом.'
      :budgetBlocked.size
        ?'Часть фото не передана из-за лимита пакета, другие вложения недоступны. Причина указана у каждого затронутого комментария.'
        :'Вложения комментариев или фото публикации не удалось прочитать. Предложения действий для затронутых комментариев не подготовлены; обсуждение и поиск доступны.')};
}

import fs from 'node:fs/promises';
import path from 'node:path';
import https from 'node:https';
import dns from 'node:dns/promises';
import {isIP} from 'node:net';
import {createHash} from 'node:crypto';

const MAX_BYTES=8*1024*1024, MAX_IMAGES=8;
const fail=(message,mediaCause)=>Object.assign(new Error(message),{code:'ASSISTANT_MEDIA_UNAVAILABLE',...(mediaCause?{mediaCause}:{})});

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
  let url=imageUrl(value);
  for(let hop=0;hop<=2;hop++){
    signal?.throwIfAborted();
    const answers=await new Promise((resolve,reject)=>{
      const aborted=()=>reject(fail('Image DNS lookup timed out'));
      signal.addEventListener('abort',aborted,{once:true});
      Promise.resolve().then(()=>lookup(url.hostname,{all:true,family:4})).then(resolve,reject)
        .finally(()=>signal.removeEventListener('abort',aborted));
    });
    if(!answers.length||answers.some(a=>!publicAddress(a.address)))throw fail('Image host is not public');
    const response=await new Promise((resolve,reject)=>{
      const req=request(url,{method:'GET',agent:false,signal,headers:{Accept:'image/png,image/jpeg,image/webp'},lookup:(_host,options,cb)=>cb(null,options.all?[{address:answers[0].address,family:4}]:answers[0].address,4)},res=>{
        if([301,302,303,307,308].includes(res.statusCode)){res.resume();resolve({redirect:res.headers.location});return;}
        if(res.statusCode!==200){res.resume();reject(fail('Image download failed'));return;}
        if(Number(res.headers['content-length'])>MAX_BYTES){res.destroy();reject(fail('Image is too large'));return;}
        const parts=[];let size=0;
        res.on('data',chunk=>{size+=chunk.length;if(size>MAX_BYTES){res.destroy();reject(fail('Image is too large'));}else parts.push(chunk);});
        res.on('end',()=>resolve({bytes:Buffer.concat(parts),mime:String(res.headers['content-type']||'').split(';')[0].toLowerCase()}));
        res.on('error',reject);
      });
      req.setTimeout(15000,()=>req.destroy(fail('Image download timed out')));
      req.on('error',reject);req.end();
    });
    if(response.redirect){url=imageUrl(new URL(response.redirect,url).href);continue;}
    return response;
  }
  throw fail('Image redirect limit exceeded');
}

export function validateImage(bytes,mime) {
  if(!Buffer.isBuffer(bytes)||!bytes.length||bytes.length>MAX_BYTES)throw fail('Invalid image size');
  let width=0,height=0,extension;
  if(mime==='image/png'&&bytes.length>=33&&bytes.subarray(0,8).equals(Buffer.from('89504e470d0a1a0a','hex'))&&bytes.toString('ascii',12,16)==='IHDR'){
    width=bytes.readUInt32BE(16);height=bytes.readUInt32BE(20);extension='png';
  }else if(mime==='image/jpeg'&&bytes[0]===255&&bytes[1]===216){
    extension='jpg';let at=2;
    while(at+4<bytes.length){
      if(bytes[at++]!==255)break;while(bytes[at]===255)at++;const marker=bytes[at++];
      if(marker===217||marker===218)break;if(marker===1||marker>=208&&marker<=215)continue;
      const size=bytes.readUInt16BE(at);if(size<2||at+size>bytes.length)break;
      if([192,193,194,195,197,198,199,201,202,203,205,206,207].includes(marker)&&size>=7){height=bytes.readUInt16BE(at+3);width=bytes.readUInt16BE(at+5);break;}at+=size;
    }
  }else if(mime==='image/webp'&&bytes.length>=30&&bytes.toString('ascii',0,4)==='RIFF'&&bytes.toString('ascii',8,12)==='WEBP'){
    extension='webp';const kind=bytes.toString('ascii',12,16);
    if(kind==='VP8X'){width=bytes.readUIntLE(24,3)+1;height=bytes.readUIntLE(27,3)+1;}
    else if(kind==='VP8 '&&bytes.subarray(23,26).equals(Buffer.from([157,1,42]))){width=bytes.readUInt16LE(26)&16383;height=bytes.readUInt16LE(28)&16383;}
    else if(kind==='VP8L'&&bytes[20]===47){const bits=bytes.readUInt32LE(21);width=(bits&16383)+1;height=((bits>>>14)&16383)+1;}
  }
  if(!extension||!width||!height||width>12000||height>12000||width*height>24000000)throw fail('Unsupported, invalid or oversized image dimensions');
  return {extension,width,height};
}

async function stageImages(prepared,home,{download=downloadImage}={}) {
  const sourceGaps=commentMediaSourceGaps(prepared.payload.items);
  if(sourceGaps.length)throw Object.assign(fail('Comment media unavailable at source','source_unavailable'),{unavailableItems:sourceGaps});
  const candidates=[];
  for(const item of prepared.payload.items){
    for(const [index,a] of (item.attachments??[]).entries()){
      if(!['photo','image','sticker'].includes(a.type))throw fail('Comment media requires separate preparation','unsupported_capability');
      imageUrl(a.url);candidates.push({itemId:item.id,attachmentIndex:index,url:a.url});
    }
  }
  if(candidates.length>MAX_IMAGES)throw fail('Attach at most eight comment images per model run');
  const paths=[],manifest=[];
  const signal=AbortSignal.timeout(45000);
  for(const candidate of candidates){
    let downloaded;try{downloaded=await download(candidate.url,{signal});}catch{throw fail('Comment image could not be retrieved safely');}
    const format=validateImage(downloaded.bytes,downloaded.mime);
    const file=path.join(home,`comment-image-${paths.length+1}.${format.extension}`);
    await fs.writeFile(file,downloaded.bytes,{flag:'wx',mode:0o600});paths.push(file);
    manifest.push({imageNumber:paths.length,itemId:candidate.itemId,attachmentIndex:candidate.attachmentIndex,origin:'comment_attachment',sha256:createHash('sha256').update(downloaded.bytes).digest('hex'),mime:downloaded.mime,width:format.width,height:format.height});
  }
  prepared.payload.imageEvidence={status:manifest.length?'attached':'no_images_attached',images:manifest,branchImagesAttached:false};
  prepared.input=JSON.stringify(prepared.payload);
  return {paths,manifest};
}

export async function stageAssistantImages(prepared,home,options={}) {
  try{return await stageImages(prepared,home,options);}
  catch(error){
    // Interactive discussion/search remains useful without pixels. Preparation
    // stays strict; discussion can explain the gap but cannot produce an action
    // for a recipient whose supplied media was not delivered.
    if(prepared.triage!==false||error.code!=='ASSISTANT_MEDIA_UNAVAILABLE')throw error;
    const blockedItemIds=prepared.payload.items.filter(item=>
      item.attachmentStatus==='present'||item.attachmentStatus==='unavailable'
      ||item.attachments?.length).map(item=>item.id);
    prepared.payload.imageEvidence={status:'unavailable',images:[],branchImagesAttached:false,
      unavailableItems:blockedItemIds.map(itemId=>({itemId,reason:'Comment media could not be attached; visual contents are unknown. Do not propose any action for this item.'}))};
    prepared.input=JSON.stringify(prepared.payload);
    return {paths:[],manifest:[],blockedItemIds};
  }
}

export function admitImageDependentProposals(result,images) {
  const blocked=new Set(images.blockedItemIds??[]);
  if(!blocked.size)return result;
  return {...result,proposals:result.proposals.filter(proposal=>!blocked.has(proposal.itemId)),
    text:result.text+'\n\nВложения части комментариев не удалось прочитать. Предложения действий для этих комментариев не подготовлены; обсуждение и поиск доступны.'};
}

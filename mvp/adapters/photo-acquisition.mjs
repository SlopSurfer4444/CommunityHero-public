import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {accountDefinition,reject} from './config.mjs';
import {attachmentEvidence,postAttachmentEvidence,stageAssistantImages,validateImage} from './assistant-images.mjs';

// Native-only, source-derived acquisition. This is bytes provenance, never
// model-seen evidence or an editorial decision. Native owns durable replay,
// current-source readback, CAS persistence, and eventual staging retirement.
const invalid=()=>reject('PHOTO_ACQUISITION_INVALID_REQUEST');
const sha=value=>typeof value==='string'&&/^[a-f0-9]{64}$/.test(value);
const id=value=>typeof value==='string'&&value.length>0&&value.length<=500&&!/[\x00-\x1f\x7f]/.test(value);
const exact=(value,keys)=>value&&typeof value==='object'&&!Array.isArray(value)
  &&Object.keys(value).length===keys.length&&keys.every(key=>Object.hasOwn(value,key));

export function validatePhotoAcquisitionRequest(req) {
  const account=accountDefinition(req?.account).accountKey;
  // App::bridge injects operation+account. No client path, replacement URL,
  // imageEvidence, purpose override, or assistant prepared payload is accepted.
  const operation=req?.op??req?.operation;
  if(operation!=='photo_acquire_only'||req?.op!==undefined&&req?.operation!==undefined&&req.op!==req.operation
    ||Object.keys(req).some(key=>!['op','operation','account','request'].includes(key)))invalid();
  if(req.request?.version===2){
    const request=req.request,comment=request.sourceComment;
    if(!exact(request,['version','purpose','receiptId','sourceDigest','sourceComment'])||request.purpose!=='photo_acquire_only'
      ||typeof request.receiptId!=='string'||!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(request.receiptId)
      ||!sha(request.sourceDigest)||!exact(comment,['id','sourceVersion','sourceRole','attachments'])||!id(comment.id)||!sha(comment.sourceVersion)
      ||!exact(comment.sourceRole,['role','messageId','roleEvidence'])||!['customer','brand','unknown'].includes(comment.sourceRole.role)
      ||!id(comment.sourceRole.messageId)||comment.sourceRole.roleEvidence!==null&&(typeof comment.sourceRole.roleEvidence!=='string'||comment.sourceRole.roleEvidence.length>160))invalid();
    let evidence;try{evidence=attachmentEvidence({attachments:comment.attachments});}catch{invalid();}
    const indices=evidence.attachments?.flatMap((a,index)=>['photo','image','sticker'].includes(a.type)?[index]:[])??[];
    if(!indices.length||indices.length>16||indices.some(index=>!evidence.attachments[index].url))invalid();
    return {account,request,comment:{...comment,...evidence},indices};
  }
  if(!exact(req.request,['version','purpose','receiptId','sourceDigest','sourcePost','items','branches']))invalid();
  const request=req.request,post=request.sourcePost;
  if(request.version!==1||request.purpose!=='photo_acquire_only'
    ||typeof request.receiptId!=='string'||!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(request.receiptId)
    ||!sha(request.sourceDigest)||!exact(post,['id','postKey','sourceVersion','attachments'])
    ||!id(post.id)||!id(post.postKey)||!sha(post.sourceVersion)
    ||!Array.isArray(post.attachments)||!post.attachments.length||post.attachments.length>20
    ||!Array.isArray(request.items)||!request.items.length||request.items.length>100
    ||!Array.isArray(request.branches)||request.branches.length)invalid();
  const ids=new Set();
  for(const item of request.items){
    if(!item||typeof item!=='object'||Array.isArray(item)||!id(item.id)||ids.has(item.id)
      ||item.postId!==post.id||item.postKey!==post.postKey)invalid();
    ids.add(item.id);
  }
  let evidence;try{evidence=postAttachmentEvidence(post);}catch{invalid();}
  const indices=evidence.attachments.flatMap((attachment,index)=>['photo','image'].includes(attachment.type)?[index]:[]);
  if(!indices.length||indices.length>16)invalid();
  return {account,request,post:{...post,...evidence},indices};
}

// Every existing ancestor must be an ordinary directory. The configured root
// and local OS ACLs remain a native deployment responsibility; no untrusted
// local writer may race this private staging tree.
async function directoryNoLinks(directory) {
  const resolved=path.resolve(directory),root=path.parse(resolved).root;
  let current=root;
  for(const part of resolved.slice(root.length).split(path.sep).filter(Boolean)){
    current=path.join(current,part);
    const stat=await fs.lstat(current);
    if(!stat.isDirectory()||stat.isSymbolicLink())reject('PHOTO_ACQUISITION_INVALID_STAGING');
  }
  const real=await fs.realpath(resolved);
  if((process.platform==='win32'?real.toLowerCase():real)!==(process.platform==='win32'?resolved.toLowerCase():resolved))
    reject('PHOTO_ACQUISITION_INVALID_STAGING');
  return resolved;
}

async function stagingDirectory(account,receiptId,env) {
  const configured=env.COMMUNITYHERO_MEDIA_SCRATCH_DIR;
  if(typeof configured!=='string'||!path.isAbsolute(configured))reject('PHOTO_ACQUISITION_STAGING_UNAVAILABLE');
  const root=await directoryNoLinks(configured),acquisitionBase=path.join(root,'photo-acquisition'),base=path.join(acquisitionBase,account);
  for(const directory of [acquisitionBase,base]){
    try{await fs.mkdir(directory,{mode:0o700});}catch(error){if(error.code!=='EEXIST')throw error;}
    await directoryNoLinks(directory);
  }
  const directory=path.join(base,receiptId);
  // An earlier retained attempt is never overwritten, deleted, or blind-retried.
  try{await fs.mkdir(directory,{mode:0o700});}catch(error){
    if(error.code==='EEXIST')reject('PHOTO_ACQUISITION_STAGING_EXISTS');throw error;
  }
  await directoryNoLinks(directory);
  return directory;
}

export async function runPhotoAcquisition(req,{env=process.env,download,signal,sourceTimeoutMs,totalTimeoutMs}={}) {
  const {account,request,post,comment,indices}=validatePhotoAcquisitionRequest(req);
  const directory=await stagingDirectory(account,request.receiptId,env);
  // A distinct INTERNAL recipient per slot makes failures independent while
  // retaining the existing downloader, concurrency, byte/pixel and HTTPS gates.
  // These internal IDs are discarded and never enter native/model metadata.
  const items=indices.map(index=>comment?{id:`photo-slot-${index}`,attachments:[comment.attachments[index]],attachmentStatus:'present'}:{id:`photo-slot-${index}`,postId:post.id,attachments:[],attachmentStatus:'none'});
  const prepared=comment?{triage:false,payload:{items,posts:[],branches:[]}}:{triage:false,payload:{items,posts:[post],branches:[],
    visualSelection:{version:1,postImages:indices.map(index=>({itemId:`photo-slot-${index}`,postId:post.id,
      attachmentIndices:[index],reason:'Acquire exact source bytes only.'}))}}};
  // Discussion-mode staging keeps per-slot failure evidence for one photo too;
  // its assistant projection is internal and is discarded on return.
  const staged=await stageAssistantImages(prepared,directory,{...(download?{download}:{}),signal,sourceTimeoutMs,totalTimeoutMs});
  const images=[];
  for(const [index,row] of staged.manifest.entries()){
    const sourceIndex=comment?indices.find(slot=>row.itemId===`photo-slot-${slot}`):row.attachmentIndex;
    if(comment?(row.origin!=='comment_attachment'||row.attachmentIndex!==0||!indices.includes(sourceIndex)):(row.origin!=='post_attachment'||row.postId!==post.id||!indices.includes(sourceIndex)))
      reject('PHOTO_ACQUISITION_INVALID_RESULT');
    const stagedPath=staged.paths[index],target=path.join(directory,`photo-${sourceIndex}.image`);
    const stat=await fs.lstat(stagedPath);
    if(!stat.isFile()||stat.isSymbolicLink()||stat.nlink!==1)reject('PHOTO_ACQUISITION_INVALID_RESULT');
    const bytes=await fs.readFile(stagedPath),format=validateImage(bytes,row.mime);
    if(format.width!==row.width||format.height!==row.height
      ||createHash('sha256').update(bytes).digest('hex')!==row.sha256)reject('PHOTO_ACQUISITION_INVALID_RESULT');
    // The target is in a newly created private directory; exclusive linking
    // refuses any preexisting filename. Originals remain if publication fails.
    await fs.link(stagedPath,target);
    await fs.unlink(stagedPath);
    images.push({attachmentIndex:sourceIndex,...(comment?{itemId:comment.id,sourceRole:comment.sourceRole}:{postId:post.id}),sha256:row.sha256,
      bytes:bytes.length,mime:row.mime,width:row.width,height:row.height});
  }
  const failures=indices.filter(index=>!images.some(row=>row.attachmentIndex===index)).map(index=>{
    const row=staged.failureEvidence?.find(row=>comment?row.itemId===`photo-slot-${index}`:row.postId===post.id&&row.attachmentIndex===index);
    const unavailable=staged.unavailableItems?.find(row=>row.itemId===`photo-slot-${index}`);
    const category=row?.category??unavailable?.category??(unavailable?.reason==='image_budget_exceeded'?'image_too_large'
      :unavailable?.reason==='missing_original_post_image_url'?'image_invalid_source':'image_unknown');
    return {attachmentIndex:index,...(comment?{itemId:comment.id,sourceRole:comment.sourceRole}:{postId:post.id}),category,stage:row?.stage??'not_started'};
  });
  return {version:1,account,receiptId:request.receiptId,sourceDigest:request.sourceDigest,images,failures};
}

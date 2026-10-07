// Native material authority is validated before model work; CAS pixels are
// staged byte-for-byte. This module does not acquire sources or call a model.
import fs from 'node:fs/promises';
import {constants as fsConstants} from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {stageAssistantImages,validateImage} from './assistant-images.mjs';
import {serializeAssistantModelInput} from './assistant-model-context.mjs';
import {currentTraceRecorder} from '../cli/trace-recorder.mjs';
import {ACCOUNT_KEYS,accountDefinition} from './config.mjs';
export const MANDATORY_MATERIAL_CONTRACT='mandatory_post_materials_v1';
const rows=v=>Array.isArray(v)?v:[];
const sha=v=>typeof v==='string'&&/^[a-f0-9]{64}$/.test(v);
const hash=v=>createHash('sha256').update(typeof v==='string'||Buffer.isBuffer(v)?v:JSON.stringify(canonical(v))).digest('hex');
const canonical=v=>Array.isArray(v)?v.map(canonical):v&&typeof v==='object'?Object.fromEntries(Object.keys(v).sort().map(k=>[k,canonical(v[k])])):v;
const fail=reason=>Object.assign(new Error(reason),{code:'ASSISTANT_MEDIA_UNAVAILABLE',mediaCause:reason});
export const mandatoryMaterialsEnabled=req=>req?.mandatoryMaterialContract===MANDATORY_MATERIAL_CONTRACT;
export function validateMandatoryMaterials(req){
  if(!mandatoryMaterialsEnabled(req)){if(req?.mandatoryMaterialContract!==undefined)throw fail('unsupported_material_contract');return undefined;}
  const bundle=req.postContextBundle;
  const profile=ACCOUNT_KEYS.map(accountDefinition).find(p=>req.account===p.accountKey||req.account===p.displayName);
  if(!profile||!bundle||bundle.schemaVersion!==1||bundle.companyId!==profile.displayName
    ||req.connectorBinding?.accountId!==profile.displayName||!sha(bundle.contentSha256))throw fail('material_bundle_binding_invalid');
  const unsigned={...bundle};delete unsigned.contentSha256;
  if(hash(unsigned)!==bundle.contentSha256||JSON.stringify(canonical(bundle.readiness))!==JSON.stringify(canonical(req.materialReadiness)))throw fail('material_bundle_digest_invalid');
  if(bundle.readiness?.status!=='ready'||!rows(bundle.readiness?.requirements).length||rows(bundle.readiness.requirements).some(r=>r.status!=='ready'))throw fail('mandatory_material_not_ready');
  const members=rows(bundle.members),posts=rows(req.posts),needed=new Set(rows(req.items).map(i=>i.postId??rows(req.branches).find(b=>b.id===i.branchId)?.postId));
  if(!members.length||new Set(members.map(m=>m.canonicalPostId)).size!==members.length||[...needed].some(id=>!members.some(m=>m.canonicalPostId===id)))throw fail('material_member_coverage_invalid');
  for(const member of members){
    const post=posts.find(p=>p.id===member.canonicalPostId);
    if(!post||!sha(member.postSourceVersion)||member.connectorBinding?.accountId!==profile.displayName||!rows(bundle.connectionBindings).some(b=>JSON.stringify(canonical(b))===JSON.stringify(canonical(member.connectorBinding))))throw fail('material_member_binding_invalid');
    for(const key of ['title','text','body','caption'])if(member.fields[key]!==null&&member.fields[key]!==undefined&&member.fields[key]!==post[key])throw fail('mandatory_post_text_changed');
    if(!['title','text','body'].some(k=>typeof member.fields[k]==='string')||!Array.isArray(member.fields.attachments))throw fail('mandatory_post_text_or_metadata_missing');
    if(JSON.stringify(canonical(member.fields.attachments))!==JSON.stringify(canonical(post.attachments)))throw fail('material_attachment_changed');
    const assets=rows(member.assets),attachments=member.fields.attachments;
    for(const [index,a] of attachments.entries()){
      const modality=['photo','image'].includes(a.type)?'photo':['video','clip','reel'].includes(a.type)?'video':null;
      if(!modality)continue;
      const matching=assets.filter(v=>v.attachmentIndex===index&&v.modality===modality&&v.sourceVersion===member.postSourceVersion);
      if(matching.length!==1||!sha(matching[0].attachmentIdentity))throw fail('material_asset_coverage_invalid');
      const asset=matching[0];
      if(modality==='photo'&&(!sha(asset.photo?.artifact?.sha256)||!Number.isSafeInteger(asset.photo.artifact.bytes)||asset.photo.artifact.bytes<1||asset.photo.artifact.bytes>8*1024*1024||asset.photo.sha256!==asset.photo.artifact.sha256))throw fail('mandatory_photo_receipt_missing');
      if(modality==='video'&&(!['transcript','no_speech','no_audio'].includes(asset.speech?.outcome)||typeof asset.speech.text!=='string'||!sha(asset.speech.materialSha256)||asset.speech.transcription?.partial!==false||!['full_audio','no_audio_stream'].includes(asset.speech.coverage)))throw fail('mandatory_speech_outcome_unproven');
    }
  }
  const commentPhotos=rows(bundle.commentPhotos);
  if(bundle.commentPhotos!==undefined&&!Array.isArray(bundle.commentPhotos)||req.commentPhotoSources!==undefined&&!Array.isArray(req.commentPhotoSources))throw fail('comment_photo_source_projection_changed');
  if(commentPhotos.length){
    if(JSON.stringify(canonical(commentPhotos))!==JSON.stringify(canonical(req.commentPhotoSources)))throw fail('comment_photo_source_projection_changed');
    const seen=new Set();
    for(const source of commentPhotos){
      const item=rows(req.items).find(item=>item.id===source.itemId),attachment=(item?.attachments??item?.commentAttachments)?.[source.attachmentIndex];
      const key=JSON.stringify([source.itemId,source.attachmentIndex]);
      if(seen.has(key)||!Number.isSafeInteger(source.attachmentIndex)||source.attachmentIndex<0||source.attachmentIndex>=20
        ||!['photo','image','sticker'].includes(attachment?.type)||!sha(source.attachmentIdentity)||!sha(source.sourceVersion)||!sha(source.acquisitionReceiptSha256)
        ||!['customer','brand','unknown'].includes(source.sourceRole?.role)||typeof source.sourceRole?.messageId!=='string'||!source.sourceRole.messageId
        ||!sha(source.photo?.artifact?.sha256)||source.photo.sha256!==source.photo.artifact.sha256||!Number.isSafeInteger(source.photo.artifact.bytes)||source.photo.artifact.bytes<1||source.photo.artifact.bytes>8*1024*1024
        ||source.photo.origin!=='comment_attachment'||source.photo.itemId!==source.itemId||source.photo.attachmentIndex!==source.attachmentIndex
        ||source.photo.postId!==undefined||source.photo.itemIds!==undefined||source.photo.sourceVersion!==source.sourceVersion
        ||source.photo.acquisitionReceiptSha256!==source.acquisitionReceiptSha256
        ||!['image/png','image/jpeg','image/webp'].includes(source.photo.mime)||!Number.isSafeInteger(source.photo.width)||source.photo.width<1||source.photo.width>12000
        ||!Number.isSafeInteger(source.photo.height)||source.photo.height<1||source.photo.height>12000||source.photo.width*source.photo.height>24000000
        ||JSON.stringify(canonical(source.photo.sourceRole))!==JSON.stringify(canonical(source.sourceRole)))throw fail('mandatory_comment_photo_receipt_invalid');
      seen.add(key);
    }
    for(const item of rows(req.items))for(const [index,a] of (item.attachments??item.commentAttachments??[]).entries()){
      if(['photo','image','sticker'].includes(a.type)&&!seen.has(JSON.stringify([item.id,index])))throw fail('mandatory_comment_photo_coverage_invalid');
    }
  }else if(rows(req.commentPhotoSources).length)throw fail('comment_photo_source_projection_changed');
  // An undeclared message-owned photo cannot fall back to a URL download in a
  // mandatory capture. Source-only acquisition and model delivery are separate.
  for(const item of rows(req.items))for(const [index,a] of rows(item.attachments??item.commentAttachments).entries()){
    if(['photo','image','sticker'].includes(a.type)&&!commentPhotos.some(source=>source.itemId===item.id&&source.attachmentIndex===index))throw fail('mandatory_comment_photo_coverage_invalid');
  }
  return structuredClone({mandatoryMaterialContract:req.mandatoryMaterialContract,postContextBundle:bundle,materialReadiness:req.materialReadiness,optionalFrameRefs:req.optionalFrameRefs??[],
    ...(commentPhotos.length?{commentPhotoSources:commentPhotos}:{})});
}
export function expectedMaterialInvocation(req){
  const bundle=req.postContextBundle;
  return {companyId:bundle.companyId,postContextBundleSha256:bundle.contentSha256,
    ...(rows(bundle.commentPhotos).length?{requiredCommentPhotos:bundle.commentPhotos.map(source=>({itemId:source.itemId,attachmentIndex:source.attachmentIndex,attachmentIdentity:source.attachmentIdentity,
      sourceRole:source.sourceRole,sourceVersion:source.sourceVersion,artifact:source.photo.artifact,acquisitionReceiptSha256:source.acquisitionReceiptSha256}))}:{}),
    memberPins:bundle.members.map(m=>({postId:m.canonicalPostId,connectorBinding:m.connectorBinding,sourceVersion:m.postSourceVersion,postFieldsSha256:hash(m.fields)})),
    requiredPhotos:bundle.members.flatMap(m=>rows(m.assets).filter(a=>a.modality==='photo').map(a=>({postId:m.canonicalPostId,attachmentIndex:a.attachmentIndex,attachmentIdentity:a.attachmentIdentity,sourceVersion:a.sourceVersion,artifact:a.photo.artifact,acquisitionReceiptSha256:a.acquisitionReceiptSha256}))),
    suppliedSpeech:bundle.members.flatMap(m=>rows(m.assets).filter(a=>a.modality==='video').map(a=>({postId:m.canonicalPostId,attachmentIndex:a.attachmentIndex,attachmentIdentity:a.attachmentIdentity,sourceVersion:a.sourceVersion,materialId:a.speech.materialId,materialSha256:a.speech.materialSha256,outcome:a.speech.outcome,coverage:a.speech.coverage})))};
}
async function rejectLinks(target){
  const absolute=path.resolve(target),parsed=path.parse(absolute);let current=parsed.root;
  for(const part of absolute.slice(parsed.root.length).split(path.sep).filter(Boolean)){current=path.join(current,part);const stat=await fs.lstat(current);if(stat.isSymbolicLink())throw fail('material_artifact_link_rejected');}
}
export async function readVerifiedPhoto(candidate,{root=process.env.COMMUNITYHERO_MEDIA_EVIDENCE_DIR,signal}={}){
  const image=candidate.materialPhoto,artifact=image?.artifact;
  if(!root||!path.isAbsolute(root)||!sha(artifact?.sha256)||!Number.isSafeInteger(artifact?.bytes)||artifact.bytes<1||artifact.bytes>8*1024*1024)throw fail('material_artifact_invalid');
  const file=path.join(root,'objects',artifact.sha256.slice(0,2),artifact.sha256);await rejectLinks(file);signal?.throwIfAborted();
  const handle=await fs.open(file,fsConstants.O_RDONLY|(fsConstants.O_NOFOLLOW??0));
  try{const stat=await handle.stat();if(!stat.isFile()||stat.size!==artifact.bytes||stat.nlink!==1)throw fail('material_artifact_size_changed');
    const bytes=await handle.readFile();signal?.throwIfAborted();if(bytes.length!==artifact.bytes||hash(bytes)!==artifact.sha256)throw fail('material_artifact_hash_changed');
    const format=validateImage(bytes,image.mime);if(format.width!==image.width||format.height!==image.height)throw fail('material_artifact_dimensions_changed');
    return {bytes,mime:image.mime};
  }finally{await handle.close();}
}
export async function stageMandatoryMaterials(prepared,home,options={}){
  const span=currentTraceRecorder()?.start('model.image_stage',{spanClass:'activity'});
  try{const images=await stageAssistantImages(prepared,home,{...options,loadPostImage:(candidate,opts)=>readVerifiedPhoto(candidate,{...opts,...options})});
    const expected=expectedMaterialInvocation(prepared.payload),required=expected.requiredPhotos,delivered=images.manifest.filter(v=>v.origin==='post_attachment');
    if(images.blockedItemIds?.length||delivered.length!==required.length||required.some(r=>delivered.filter(i=>i.postId===r.postId&&i.attachmentIndex===r.attachmentIndex&&i.sha256===r.artifact.sha256).length!==1))throw fail('mandatory_photo_delivery_incomplete');
    const comments=images.manifest.filter(image=>image.origin==='comment_attachment'),requiredComments=expected.requiredCommentPhotos??[];
    if(comments.length!==requiredComments.length||requiredComments.some(source=>comments.filter(image=>image.itemId===source.itemId&&image.attachmentIndex===source.attachmentIndex
      &&image.sha256===source.artifact.sha256&&image.bytes===source.artifact.bytes&&image.sourceVersion===source.sourceVersion&&image.acquisitionReceiptSha256===source.acquisitionReceiptSha256
      &&JSON.stringify(canonical(image.sourceRole))===JSON.stringify(canonical(source.sourceRole))).length!==1))throw fail('mandatory_comment_photo_delivery_incomplete');
    const frameRefs=rows(prepared.payload.optionalFrameRefs),frameManifest=[];
    if(!Array.isArray(prepared.payload.optionalFrameRefs??[])||images.paths.length+frameRefs.length>16)throw fail('photo_frame_transport_count_exceeded');
    let totalBytes=(await Promise.all(images.paths.map(p=>fs.stat(p)))).reduce((n,s)=>n+s.size,0),totalPixels=images.manifest.reduce((n,i)=>n+i.width*i.height,0);
    if(totalBytes>32*1024*1024||totalPixels>64000000)throw fail('photo_frame_transport_volume_exceeded');
    for(const ref of frameRefs){
      const member=prepared.payload.postContextBundle.members.find(m=>m.canonicalPostId===ref?.postId);
      if(!member||ref.companyId!==prepared.payload.postContextBundle.companyId||ref.sourceVersion!==member.postSourceVersion||JSON.stringify(canonical(ref.connectorBinding))!==JSON.stringify(canonical(member.connectorBinding))
        ||!member.assets.some(a=>a.modality==='video'&&a.attachmentIndex===ref.attachmentIndex&&a.attachmentIdentity===ref.attachmentIdentity)
        ||!sha(ref.resultSha256)||!sha(ref.needSha256)||typeof ref.frameJobId!=='string'||!ref.frameJobId||typeof ref.needId!=='string'||!ref.needId
        ||!Number.isSafeInteger(ref.actualPts)||!Number.isSafeInteger(ref.requestedTimestampMs)||ref.requestedTimestampMs<0||!Number.isSafeInteger(ref.timeBase?.num)||ref.timeBase.num<1||!Number.isSafeInteger(ref.timeBase?.den)||ref.timeBase.den<1)throw fail('targeted_frame_binding_invalid');
      const loaded=await readVerifiedPhoto({materialPhoto:ref},{...options});
      totalBytes+=loaded.bytes.length;totalPixels+=ref.width*ref.height;if(totalBytes>32*1024*1024||totalPixels>64000000)throw fail('photo_frame_transport_volume_exceeded');
      const file=path.join(home,`targeted-video-frame-${frameManifest.length+1}.png`);await fs.writeFile(file,loaded.bytes,{flag:'wx',mode:0o600});images.paths.push(file);
      frameManifest.push({imageNumber:images.paths.length,refSha256:hash(ref),needId:ref.needId,resultSha256:ref.resultSha256,postId:ref.postId,sha256:ref.sha256,bytes:ref.artifact.bytes,mime:ref.mime,width:ref.width,height:ref.height,requestedTimestampMs:ref.requestedTimestampMs,actualPts:ref.actualPts,timeBase:ref.timeBase});
    }
    images.frameManifest=frameManifest;
    if(frameManifest.length){prepared.payload.targetedVideoFrameEvidence={status:'attached',exhaustive:false,frames:frameRefs.map((ref,i)=>({...ref,imageNumber:frameManifest[i].imageNumber})),meaning:'These exact bounded source frames are attached as pixels. Requested timestamps and observed source PTS are separate. Sampling does not establish absence outside these frames.'};prepared.input=serializeAssistantModelInput(prepared.payload);}
    span?.finish({outcome:'completed',measurements:{photoExpected:required.length,photoStaged:delivered.length,imageBytes:required.reduce((n,r)=>n+r.artifact.bytes,0)}});return images;
  }catch(e){span?.finish({outcome:'held',reasonCode:'required_material_pending'});throw e;}
}
export function materialInvocation(prepared,images,{instructions,schema,cliSha256,stdin}){
  const expected=expectedMaterialInvocation(prepared.payload);
  const photos=images.manifest.filter(i=>i.origin==='post_attachment').map(i=>({...i,bytes:expected.requiredPhotos.find(r=>r.postId===i.postId&&r.attachmentIndex===i.attachmentIndex).artifact.bytes}));
  return {schemaVersion:1,contract:MANDATORY_MATERIAL_CONTRACT,completenessStatus:'complete',...expected,
    actualTextInputSha256:hash(prepared.input),actualStdinSha256:hash(stdin),instructionSha256:hash(instructions),schemaSha256:hash(schema),cliSha256,
    stagedPhotos:photos,deliveredPhotos:photos,
    ...(expected.requiredCommentPhotos?{stagedCommentPhotos:images.manifest.filter(image=>image.origin==='comment_attachment'),deliveredCommentPhotos:images.manifest.filter(image=>image.origin==='comment_attachment')}:{}),
    optionalFrameRefs:prepared.payload.optionalFrameRefs??[],stagedFrames:images.frameManifest??[],deliveredFrames:images.frameManifest??[]};
}
export const VIDEO_FRAME_NEED_INSTRUCTIONS='All photos and video speech are supplied as mandatory context. If a held recipient needs a visual detail of a video that is not visible in attached pixels, request videoFrameNeeds for that exact post and video attachment. Choose a known_range of at most 30000 ms only when supplied context identifies the interval; otherwise choose uniform_overview. Times are relative to the video start, never guessed source PTS. Return no proposal for that recipient and mark missing_context. This is a bounded source request; no locator model or repeated answering call is available in this invocation.';
export function videoFrameNeedSchema(ids){
  const time={type:'object',additionalProperties:false,required:['kind','timelineBasis','startMs','endMs'],properties:{
    kind:{type:'string',enum:['known_range','uniform_overview']},timelineBasis:{type:'string',enum:['relative_video_start']},
    startMs:{anyOf:[{type:'integer',minimum:0},{type:'null'}]},endMs:{anyOf:[{type:'integer',minimum:0},{type:'null'}]}}};
  const need={type:'object',additionalProperties:false,required:['itemId','postId','attachmentIndex','requestedTimeOrIntent','reason'],properties:{
    itemId:{type:'string',enum:[...ids]},postId:{type:'string',minLength:1,maxLength:500},attachmentIndex:{type:'integer',minimum:0,maximum:19},reason:{type:'string',minLength:1,maxLength:1000},requestedTimeOrIntent:time}};
  return {type:'array',maxItems:8,items:need};
}
export function admitVideoFrameNeeds(declarations,prepared,result){
  if(!Array.isArray(declarations)||declarations.length>8)throw fail('video_frame_needs_invalid');const seen=new Set();
  return declarations.map(need=>{
    const member=prepared.payload.postContextBundle.members.find(m=>m.canonicalPostId===need?.postId);
    const intent=need?.requestedTimeOrIntent;
    if(!need||Object.keys(need).sort().join('|')!==['itemId','postId','attachmentIndex','requestedTimeOrIntent','reason'].sort().join('|')
      ||!prepared.payload.items.some(i=>i.id===need.itemId&&i.postId===need.postId)||!member?.assets.some(a=>a.modality==='video'&&a.attachmentIndex===need.attachmentIndex)
      ||!result.assessments?.some(a=>a.itemId===need.itemId&&a.outcome==='needs_attention'&&a.tags?.includes('missing_context'))||result.proposals?.some(p=>p.itemId===need.itemId)
      ||typeof need.reason!=='string'||!need.reason.trim()||need.reason.length>1000||!intent||Object.keys(intent).sort().join('|')!==['kind','timelineBasis','startMs','endMs'].sort().join('|')||intent.timelineBasis!=='relative_video_start'
      ||intent.kind==='uniform_overview'&&(intent.startMs!==null||intent.endMs!==null)
      ||!['known_range','uniform_overview'].includes(intent.kind)||intent.kind==='known_range'&&(!Number.isSafeInteger(intent.startMs)||!Number.isSafeInteger(intent.endMs)||intent.startMs<0||intent.endMs<=intent.startMs||intent.endMs-intent.startMs>30000))throw fail('video_frame_need_source_or_hold_invalid');
    const key=JSON.stringify([need.itemId,need.postId,need.attachmentIndex]);if(seen.has(key))throw fail('video_frame_need_duplicate');seen.add(key);return structuredClone(need);
  });
}

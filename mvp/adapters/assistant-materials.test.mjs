import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {validateMandatoryMaterials,expectedMaterialInvocation,stageMandatoryMaterials,materialInvocation,admitVideoFrameNeeds} from './assistant-materials.mjs';
import {prepareAssistantRequest,runPreparationVisualFollowup} from './assistant.mjs';
import {createTraceRecorder,withTraceRecorder} from '../cli/trace-recorder.mjs';
import {conservativeImageEvidenceBytes} from './assistant-images.mjs';
const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC','base64');
const canonical=v=>Array.isArray(v)?v.map(canonical):v&&typeof v==='object'?Object.fromEntries(Object.keys(v).sort().map(k=>[k,canonical(v[k])])):v;
const hash=v=>createHash('sha256').update(Buffer.isBuffer(v)?v:JSON.stringify(canonical(v))).digest('hex');
function fixture(key='baw-russia'){
  const company=key==='baw-russia'?'BAW Russia':'LikeAvto';
  const connectorBinding={id:`angryspace-${key}-v1`,workspaceId:'local-pilot',accountId:company,connector:'angryspace',revision:1,providerAccountId:key};
  const attachments=[{type:'photo',url:'https://cdn.example/photo.png'}],artifact={sha256:hash(png),bytes:png.length};
  const member={canonicalPostId:'p',connectorBinding,postSourceVersion:'a'.repeat(64),fields:{title:'Post',text:'Exact caption 990 000',body:null,attachments},
    assets:[{modality:'photo',attachmentIndex:0,attachmentIdentity:'b'.repeat(64),sourceVersion:'a'.repeat(64),acquisitionReceiptSha256:'c'.repeat(64),photo:{sha256:artifact.sha256,artifact,mime:'image/png',width:1,height:1}}]};
  const readiness={status:'ready',requirements:[{kind:'post_text',status:'ready'},{kind:'post_photo',status:'ready'}]};
  const postContextBundle={schemaVersion:1,companyId:company,connectionBindings:[connectorBinding],members:[member],readiness};postContextBundle.contentSha256=hash(postContextBundle);
  return {account:key,connectorBinding,mandatoryMaterialContract:'mandatory_post_materials_v1',postContextBundle,materialReadiness:readiness,
    items:[{id:'i',postId:'p'}],posts:[{id:'p',title:'Post',text:'Exact caption 990 000',attachments,attachmentStatus:'present'}],branches:[],visualSelection:{version:1,postImages:[{itemId:'i',postId:'p',attachmentIndices:[0],reason:'Mandatory'}]}};
}
test('canonical company remains display-scoped across both normalized transport profiles',()=>{
  for(const key of ['baw-russia','likeavto']){const request=fixture(key);assert.ok(validateMandatoryMaterials(request));const prepared=prepareAssistantRequest(request);assert.equal(prepared.payload.postContextBundle.companyId,request.connectorBinding.accountId);assert.equal(expectedMaterialInvocation(prepared.payload).companyId,request.connectorBinding.accountId);request.account=request.postContextBundle.companyId;assert.ok(validateMandatoryMaterials(request));}
  const request=fixture();request.account='likeavto';assert.throws(()=>validateMandatoryMaterials(request),/binding_invalid/);
  request.account='baw-russia';request.connectorBinding={...request.connectorBinding,accountId:'LikeAvto'};assert.throws(()=>validateMandatoryMaterials(request),/binding_invalid/);
});
function videoFixture(){
  const request=fixture();const member=request.postContextBundle.members[0];member.fields.attachments.push({type:'video',url:'https://cdn.example/video.mp4'});request.posts[0].attachments=structuredClone(member.fields.attachments);
  member.assets.push({modality:'video',attachmentIndex:1,attachmentIdentity:'e'.repeat(64),sourceVersion:member.postSourceVersion,speech:{outcome:'no_speech',text:'Inspected no speech',materialId:'speech',materialSha256:'f'.repeat(64),coverage:'full_audio',transcription:{partial:false,audioStatus:'inspected_no_speech'}}});
  delete request.postContextBundle.contentSha256;request.postContextBundle.contentSha256=hash(request.postContextBundle);return request;
}
test('verified no_speech and no_audio differ from failed transcription',()=>{
  const request=videoFixture();assert.ok(validateMandatoryMaterials(request));
  const speech=request.postContextBundle.members[0].assets[1].speech;speech.outcome='no_audio';speech.coverage='no_audio_stream';speech.transcription.audioStatus='no_audio_stream';delete request.postContextBundle.contentSha256;request.postContextBundle.contentSha256=hash(request.postContextBundle);assert.ok(validateMandatoryMaterials(request));
  speech.outcome='failed';delete request.postContextBundle.contentSha256;request.postContextBundle.contentSha256=hash(request.postContextBundle);assert.throws(()=>validateMandatoryMaterials(request),/speech_outcome_unproven/);
});
test('frame declarations require exact held recipient and bounded typed intent',()=>{
  const payload=videoFixture(),prepared={payload};const result={assessments:[{itemId:'i',outcome:'needs_attention',tags:['missing_context']}],proposals:[]};
  const need={itemId:'i',postId:'p',attachmentIndex:1,reason:'Need visible dashboard detail',requestedTimeOrIntent:{kind:'uniform_overview',timelineBasis:'relative_video_start',startMs:null,endMs:null}};
  assert.equal(admitVideoFrameNeeds([need],prepared,result).length,1);
  for(const mutation of [n=>n.requestedTimeOrIntent.locator='guess',n=>n.requestedTimeOrIntent.startMs=1,n=>n.postId='different',n=>n.attachmentIndex=0]){const bad=structuredClone(need);mutation(bad);assert.throws(()=>admitVideoFrameNeeds([bad],prepared,result));}
  assert.throws(()=>admitVideoFrameNeeds([need],prepared,{...result,proposals:[{itemId:'i'}]}));assert.throws(()=>admitVideoFrameNeeds([need,need],prepared,result),/duplicate/);
});
test('mandatory workflow cannot use the legacy internal paid visual followup',async()=>{
  let calls=0;const request=fixture();const result=await runPreparationVisualFollowup(request,{runPass:async()=>{calls++;return {videoFrameNeeds:[{reason:'need'}],assessments:[],proposals:[]};}});
  assert.equal(calls,1);assert.equal(result.videoFrameNeeds.length,1);
});
test('targeted frame pixels are staged intact with separate actual PTS and trace completion',async()=>{
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-frame-transport-fixture-'));
  try{const request=videoFixture(),asset=request.postContextBundle.members[0].assets[0].photo,object=path.join(root,'objects',asset.sha256.slice(0,2),asset.sha256);await fs.mkdir(path.dirname(object),{recursive:true});await fs.writeFile(object,png);const home=path.join(root,'home');await fs.mkdir(home);
    const member=request.postContextBundle.members[0];request.optionalFrameRefs=[{...asset,needId:'need',needSha256:'1'.repeat(64),resultSha256:'2'.repeat(64),frameJobId:'frame-job',companyId:'BAW Russia',postId:'p',connectorBinding:member.connectorBinding,attachmentIndex:1,attachmentIdentity:'e'.repeat(64),sourceVersion:member.postSourceVersion,actualPts:-4500,requestedTimestampMs:1000,timeBase:{num:1,den:90000}}];
    const recorder=createTraceRecorder({context:{version:1,traceId:'fixture',companyKey:'baw-russia',runtimeId:'fixture-runtime',runtimeEpoch:1,sourcePin:'a'.repeat(64)}});
    const prepared={payload:request,input:'before',triage:true},images=await withTraceRecorder(recorder,()=>stageMandatoryMaterials(prepared,home,{root,download:async()=>{throw Error('Network forbidden');}}));
    assert.equal(images.paths.length,2);assert.deepEqual(await fs.readFile(images.paths[1]),png);assert.equal(images.frameManifest[0].actualPts,-4500);assert.equal(images.frameManifest[0].requestedTimestampMs,1000);assert.ok(JSON.parse(prepared.input).targetedVideoFrameEvidence.frames.length);
    const body=materialInvocation(prepared,images,{instructions:'i',schema:'{}',cliSha256:'d'.repeat(64),stdin:'s'});assert.deepEqual(body.deliveredFrames,body.stagedFrames);assert.equal(body.deliveredFrames.length,1);
    const telemetry=recorder.snapshot();assert.equal(telemetry.droppedEventCount,0);assert.ok(telemetry.events.some(e=>e.stage==='model.image_stage'&&e.eventType==='span_end'&&e.outcome==='completed'&&e.measurements.photoStaged===1));
    const tooMany={payload:{...request,optionalFrameRefs:Array(16).fill(request.optionalFrameRefs[0])},input:'before',triage:true};const home2=path.join(root,'home2');await fs.mkdir(home2);await assert.rejects(stageMandatoryMaterials(tooMany,home2,{root}),/transport_count_exceeded/);
  }finally{await fs.rm(root,{recursive:true,force:true});}
});
test('one missing mandatory photo or changed source text fails before model work',()=>{
  const request=fixture();request.posts[0].text='invented price';assert.throws(()=>validateMandatoryMaterials(request),/text_changed/);
  const pending=fixture();pending.materialReadiness=pending.postContextBundle.readiness={status:'pending',requirements:[{kind:'post_photo',status:'pending'}]};delete pending.postContextBundle.contentSha256;pending.postContextBundle.contentSha256=hash(pending.postContextBundle);
  assert.throws(()=>validateMandatoryMaterials(pending),/not_ready/);
});
test('all required photos use retained bytes; same-size tamper stops whole call',async()=>{
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-material-fixture-'));
  try{const request=fixture(),artifact=request.postContextBundle.members[0].assets[0].photo.artifact;
    const object=path.join(root,'objects',artifact.sha256.slice(0,2),artifact.sha256);await fs.mkdir(path.dirname(object),{recursive:true});await fs.writeFile(object,png);
    const home=path.join(root,'home');await fs.mkdir(home);
    const prepared={payload:request,input:'before',triage:true};let networkCalls=0;
    const images=await stageMandatoryMaterials(prepared,home,{root,download:async()=>{networkCalls++;throw Error('Network forbidden');}});
    assert.equal(networkCalls,0);assert.equal(images.manifest.length,1);assert.equal(images.manifest[0].sha256,artifact.sha256);
    const body=materialInvocation(prepared,images,{instructions:'instructions',schema:'{}',cliSha256:'d'.repeat(64),stdin:'actual stdin'});
    assert.equal(body.companyId,'BAW Russia');assert.deepEqual(body.requiredPhotos,expectedMaterialInvocation(request).requiredPhotos);assert.deepEqual(body.stagedPhotos,body.deliveredPhotos);
    const tampered=Buffer.from(png);tampered[40]^=1;await fs.writeFile(object,tampered);const home2=path.join(root,'home2');await fs.mkdir(home2);
    await assert.rejects(stageMandatoryMaterials({payload:fixture(),input:'before',triage:true},home2,{root}),/incomplete|unavailable/i);
  }finally{await fs.rm(root,{recursive:true,force:true});}
});

function commentFixture(){
  const request=fixture(),sourceVersion='6'.repeat(64),receiptSha='7'.repeat(64),sourceRole={role:'customer',messageId:'source-message',roleEvidence:'connector-observed'};
  request.items[0].attachments=[{type:'sticker',url:'https://cdn.example/comment.png'}];request.items[0].attachmentsState='present';
  const artifact=structuredClone(request.postContextBundle.members[0].assets[0].photo.artifact);
  const source={itemId:'i',attachmentIndex:0,attachmentIdentity:'8'.repeat(64),sourceRole,sourceVersion,acquisitionReceiptSha256:receiptSha,
    photo:{origin:'comment_attachment',itemId:'i',attachmentIndex:0,sha256:artifact.sha256,bytes:artifact.bytes,artifact,mime:'image/png',width:1,height:1,
      sourceRole,sourceVersion,acquisitionReceiptSha256:receiptSha}};
  request.postContextBundle.commentPhotos=[source];request.commentPhotoSources=[structuredClone(source)];
  request.postContextBundle.readiness.requirements.push({kind:'comment_photo',status:'ready'});delete request.postContextBundle.contentSha256;request.postContextBundle.contentSha256=hash(request.postContextBundle);return request;
}
test('mandatory original comment projection survives actual prepare and delivers CAS pixels with typed evidence',async()=>{
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-comment-material-fixture-'));
  try{const request=commentFixture(),artifact=request.commentPhotoSources[0].photo.artifact,object=path.join(root,'objects',artifact.sha256.slice(0,2),artifact.sha256);
    await fs.mkdir(path.dirname(object),{recursive:true});await fs.writeFile(object,png);const home=path.join(root,'home');await fs.mkdir(home);
    const prepared=prepareAssistantRequest(request);assert.deepEqual(prepared.payload.commentPhotoSources,request.commentPhotoSources);
    const bound=conservativeImageEvidenceBytes(prepared);
    const images=await stageMandatoryMaterials(prepared,home,{root,download:()=>assert.fail('no URI fallback')});
    assert.equal(images.paths.length,2);assert.equal(images.manifest[0].origin,'comment_attachment');assert.equal(images.manifest[1].origin,'post_attachment');
    assert.deepEqual(await fs.readFile(images.paths[0]),png);assert.deepEqual(images.manifest[0].sourceRole,request.commentPhotoSources[0].sourceRole);
    assert.equal(images.manifest[0].bytes,png.length);assert.equal(images.manifest[0].postId,undefined);
    assert.ok(Buffer.byteLength(',"imageEvidence":')+Buffer.byteLength(JSON.stringify(prepared.payload.imageEvidence))<=bound,'precall envelope includes typed source provenance');
    const invocation=materialInvocation(prepared,images,{instructions:'i',schema:'{}',cliSha256:'d'.repeat(64),stdin:'s'});
    assert.equal(invocation.requiredCommentPhotos.length,1);assert.deepEqual(invocation.stagedCommentPhotos,invocation.deliveredCommentPhotos);
    assert.deepEqual(invocation.deliveredCommentPhotos,[images.manifest[0]]);assert.equal(invocation.deliveredPhotos.length,1);
    const changed=Buffer.from(png);changed[40]^=1;await fs.writeFile(object,changed);const nextHome=path.join(root,'next');await fs.mkdir(nextHome);
    await assert.rejects(stageMandatoryMaterials(prepareAssistantRequest(request),nextHome,{root,download:()=>assert.fail('corrupt CAS cannot fall back')}),/incomplete|unavailable/i);
  }finally{await fs.rm(root,{recursive:true,force:true});}
});
test('mandatory comment omission wrong role foreign post and duplicate slot fail before download',()=>{
  const missing=commentFixture();delete missing.commentPhotoSources;assert.throws(()=>prepareAssistantRequest(missing),/source_projection_changed/);
  const undeclared=commentFixture();delete undeclared.postContextBundle.commentPhotos;delete undeclared.commentPhotoSources;delete undeclared.postContextBundle.contentSha256;undeclared.postContextBundle.contentSha256=hash(undeclared.postContextBundle);assert.throws(()=>prepareAssistantRequest(undeclared),/coverage_invalid/);
  for(const mutate of [source=>source.photo.sourceRole={...source.photo.sourceRole,role:'brand'},source=>source.photo.postId='p',source=>source.photo.sourceVersion='9'.repeat(64),source=>source.photo.width=0,
    source=>source.attachmentIdentity='bad',source=>source.attachmentIndex=7]){
    const input=commentFixture();mutate(input.postContextBundle.commentPhotos[0]);input.commentPhotoSources=structuredClone(input.postContextBundle.commentPhotos);delete input.postContextBundle.contentSha256;input.postContextBundle.contentSha256=hash(input.postContextBundle);
    assert.throws(()=>validateMandatoryMaterials(input),/receipt_invalid/);
  }
  const duplicate=commentFixture();duplicate.postContextBundle.commentPhotos.push(structuredClone(duplicate.postContextBundle.commentPhotos[0]));duplicate.commentPhotoSources=structuredClone(duplicate.postContextBundle.commentPhotos);delete duplicate.postContextBundle.contentSha256;duplicate.postContextBundle.contentSha256=hash(duplicate.postContextBundle);assert.throws(()=>validateMandatoryMaterials(duplicate),/receipt_invalid/);
});

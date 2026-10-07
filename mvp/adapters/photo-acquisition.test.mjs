import test from 'node:test';
import {registerHooks} from 'node:module';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {runPhotoAcquisition} from './photo-acquisition.mjs';
import {dispatch} from './bridge.mjs';
import {downloadImage} from './assistant-images.mjs';

const receiptId='a921d3b2-272f-4c13-9f7c-13151f77a001';
const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC','base64');
const pixelSha=createHash('sha256').update(png).digest('hex');
const scratchBase=path.join(os.tmpdir(),'communityhero-photo-acquisition-tests');
const request=(count=3)=>({operation:'photo_acquire_only',account:'baw-russia',request:{version:1,purpose:'photo_acquire_only',
  receiptId,sourceDigest:'a'.repeat(64),sourcePost:{id:'post-1',postKey:'11341:post-1',sourceVersion:'b'.repeat(64),
    attachments:Array.from({length:count},(_,i)=>({type:i%2?'image':'photo',url:`https://cdn.example/image-${i}.png`}))},
  items:[{id:'native-comment-1',postId:'post-1',postKey:'11341:post-1',attachments:[{type:'photo',url:'https://cdn.example/COMMENT-MUST-NOT-DOWNLOAD.png'}]}],branches:[]}});
const fixture=async run=>{
  await fs.mkdir(scratchBase,{recursive:true});
  const scratch=await fs.mkdtemp(path.join(scratchBase,'case-'));
  try{return await run(scratch,{env:{COMMUNITYHERO_MEDIA_SCRATCH_DIR:scratch}});}
  finally{assert.equal(path.dirname(scratch),scratchBase);await fs.rm(scratch,{recursive:true,force:true});}
};
const downloaded=()=>({bytes:Buffer.from(png),mime:'image/png'});

test('nine slots persist exact structurally validated bytes; same URL does not merge source slots',()=>fixture(async(scratch,options)=>{
  const input=request(9);input.request.sourcePost.attachments[8].url=input.request.sourcePost.attachments[0].url;
  const before=JSON.stringify(input),calls=[];let active=0,peak=0;
  const result=await runPhotoAcquisition(input,{...options,download:async url=>{
    calls.push(url);active++;peak=Math.max(peak,active);await new Promise(resolve=>setTimeout(resolve,2));active--;return downloaded();
  }});
  assert.equal(calls.length,9);assert.equal(calls.filter(url=>url.endsWith('image-0.png')).length,2);
  assert.equal(peak,4);assert.equal(result.images.length,9);assert.deepEqual(result.failures,[]);
  assert.deepEqual(Object.keys(result).sort(),['account','failures','images','receiptId','sourceDigest','version']);
  assert.equal(JSON.stringify(input),before,'source records remain unchanged');
  for(const [index,row] of result.images.entries()){
    assert.deepEqual(row,{attachmentIndex:index,postId:'post-1',sha256:pixelSha,bytes:png.length,mime:'image/png',width:1,height:1});
    assert.deepEqual(await fs.readFile(path.join(scratch,'photo-acquisition','baw-russia',receiptId,`photo-${index}.image`)),png);
  }
  assert.deepEqual((await fs.readdir(path.join(scratch,'photo-acquisition','baw-russia',receiptId))).sort(),Array.from({length:9},(_,i)=>`photo-${i}.image`).sort());
  assert.ok(!JSON.stringify(result).includes('photo-slot'));assert.ok(!JSON.stringify(result).includes('imageEvidence'));
}));

test('bridge acquisition bypasses provider resolution/process/model setup',()=>fixture(async(_scratch,options)=>{
  const assistantURL=new URL('./assistant.mjs',import.meta.url);
  let assistantResolutions=0,downloads=0;
  const hooks=registerHooks({resolve(specifier,context,nextResolve){
    const target=new URL(specifier,context.parentURL??import.meta.url);
    target.search='';target.hash='';
    if(target.href===assistantURL.href){
      assistantResolutions++;
      throw Object.assign(new Error('assistant module resolution forbidden during photo acquisition'),{code:'TEST_ASSISTANT_IMPORT_FORBIDDEN'});
    }
    return nextResolve(specifier,context);
  }});
  try{
    // A query gives the control its own module-cache identity. The real loader
    // callback rejects before assistant code loads, even when the file exists.
    await assert.rejects(import(new URL('./assistant.mjs?photo-acquisition-guard-control=1',import.meta.url).href),{code:'TEST_ASSISTANT_IMPORT_FORBIDDEN'});
    assert.equal(assistantResolutions,1,'negative control must exercise the registered module hook');
    const result=await dispatch(request(1),{photoAcquireOptions:{...options,download:async()=>{downloads++;return downloaded();}},
      resolvePaths:()=>{assert.fail('provider/credentials resolution must not run');},
      runProcessFn:()=>{assert.fail('model/provider/ASR/vision process must not run');}});
    assert.equal(downloads,1);assert.equal(result.images.length,1);
    assert.equal(assistantResolutions,1,'photo dispatch must not resolve the assistant module');
  }finally{hooks.deregister();}
}));

test('bad bytes, network failure and missing original yield distinct failures while independent slot survives',()=>fixture(async(scratch,options)=>{
  const input=request(4);delete input.request.sourcePost.attachments[2].url;
  input.request.sourcePost.attachments[2].preview_url='https://cdn.example/preview.png';
  const calls=[];
  const result=await runPhotoAcquisition(input,{...options,download:async url=>{
    calls.push(url);if(url.endsWith('image-0.png'))return {bytes:Buffer.from('corrupt'),mime:'image/png'};
    if(url.endsWith('image-1.png'))throw Object.assign(new Error('PRIVATE LOCATOR'),{code:'ECONNRESET'});return downloaded();
  }});
  assert.deepEqual(result.images.map(row=>row.attachmentIndex),[3]);
  assert.deepEqual(result.failures.map(row=>[row.attachmentIndex,row.category]),[[0,'image_integrity'],[1,'image_network'],[2,'image_invalid_source']]);
  assert.equal(calls.length,3);assert.ok(!calls.some(url=>url.endsWith('preview.png')));
  assert.deepEqual(await fs.readdir(path.join(scratch,'photo-acquisition','baw-russia',receiptId)),['photo-3.image']);
  assert.ok(!JSON.stringify(result).includes('PRIVATE'));
}));

test('single-slot failure returns closed source evidence and leaves a retained receipt directory',()=>fixture(async(scratch,options)=>{
  const result=await runPhotoAcquisition(request(1),{...options,download:async()=>{throw Object.assign(new Error('PRIVATE'),{imageCategory:'image_http_forbidden'});}});
  assert.deepEqual(result.images,[]);
  assert.deepEqual(result.failures,[{attachmentIndex:0,postId:'post-1',category:'image_http_forbidden',stage:'acquisition'}]);
  assert.ok((await fs.stat(path.join(scratch,'photo-acquisition','baw-russia',receiptId))).isDirectory());
}));

test('closed schema, bindings, unsafe UUID and slot budget reject before filesystem/network',async()=>{
  const mutations=[input=>input.account='BAW Russia',input=>input.request.purpose='assistant',input=>input.request.imageEvidence={},
    input=>input.request.outputPath='C:/arbitrary',input=>input.request.sourceDigest='bad',input=>input.request.sourcePost.sourceVersion='bad',
    input=>input.request.receiptId='../other',input=>input.request.items[0].postId='different-post',
    input=>input.request.items[0].postKey='foreign-key',input=>input.request.items.push({...input.request.items[0]}),
    input=>input.request.branches.push({id:'branch',postId:'post-1'}),input=>input.op='assistant'];
  for(const mutate of mutations){const input=request();mutate(input);
    await assert.rejects(runPhotoAcquisition(input,{env:{},download:()=>assert.fail('network forbidden')}),
      error=>['PHOTO_ACQUISITION_INVALID_REQUEST','ACCOUNT_SCOPE_MISMATCH'].includes(error.code));}
  await assert.rejects(runPhotoAcquisition(request(17),{env:{}}),{code:'PHOTO_ACQUISITION_INVALID_REQUEST'});
  await assert.rejects(runPhotoAcquisition(request(),{env:{COMMUNITYHERO_MEDIA_SCRATCH_DIR:'relative'}}),{code:'PHOTO_ACQUISITION_STAGING_UNAVAILABLE'});
});

test('existing staging is never overwritten or replayed by the JS downloader',()=>fixture(async(scratch,options)=>{
  const directory=path.join(scratch,'photo-acquisition','baw-russia',receiptId);await fs.mkdir(directory,{recursive:true});
  await fs.writeFile(path.join(directory,'photo-0.image'),'RETAINED');
  await assert.rejects(runPhotoAcquisition(request(1),{...options,download:()=>assert.fail('must not retry')}),{code:'PHOTO_ACQUISITION_STAGING_EXISTS'});
  assert.equal(await fs.readFile(path.join(directory,'photo-0.image'),'utf8'),'RETAINED');
}));

test('same receipt UUID has separate company staging and source result bindings',()=>fixture(async(scratch,options)=>{
  const baw=request(1),likeavto=request(1);likeavto.account='likeavto';
  const first=await runPhotoAcquisition(baw,{...options,download:async()=>downloaded()});
  const second=await runPhotoAcquisition(likeavto,{...options,download:async()=>downloaded()});
  assert.equal(first.account,'baw-russia');assert.equal(second.account,'likeavto');
  for(const account of ['baw-russia','likeavto'])
    assert.deepEqual(await fs.readFile(path.join(scratch,'photo-acquisition',account,receiptId,'photo-0.image')),png);
}));

test('symlink/junction staging ancestor fails closed',()=>fixture(async(scratch,_options)=>{
  const target=path.join(scratch,'real'),linked=path.join(scratch,'linked');await fs.mkdir(target);
  await fs.symlink(target,linked,process.platform==='win32'?'junction':'dir');
  await assert.rejects(runPhotoAcquisition(request(1),{env:{COMMUNITYHERO_MEDIA_SCRATCH_DIR:linked},download:()=>assert.fail('network forbidden')}),
    {code:'PHOTO_ACQUISITION_INVALID_STAGING'});
}));

test('destination collision cannot replace an existing file',()=>fixture(async(scratch,options)=>{
  const destination=path.join(scratch,'photo-acquisition','baw-russia',receiptId,'photo-0.image');
  await assert.rejects(runPhotoAcquisition(request(1),{...options,download:async()=>{
    await fs.writeFile(destination,'EXISTING');return downloaded();
  }}),{code:'EEXIST'});
  assert.equal(await fs.readFile(destination,'utf8'),'EXISTING');
}));

test('HTTPS parser and DNS rejection happen without a network request',async()=>{
  for(const url of ['http://cdn.example/p.png','https://127.0.0.1/p.png','https://user:password@cdn.example/p.png','https://cdn.example/p.png?access_token=x'])
    await assert.rejects(downloadImage(url,{lookup:()=>assert.fail('DNS forbidden'),request:()=>assert.fail('network forbidden')}),
      error=>error.imageCategory==='image_invalid_source');
  await assert.rejects(downloadImage('https://cdn.example/p.png',{lookup:async()=>[{address:'127.0.0.1',family:4}],
    request:()=>assert.fail('private destination must never connect')}),error=>error.imageCategory==='image_invalid_source');
});

test('cancellation returns each slot as unavailable without a download',()=>fixture(async(_scratch,options)=>{
  const controller=new AbortController();controller.abort();
  const result=await runPhotoAcquisition(request(3),{...options,signal:controller.signal,download:()=>assert.fail('cancelled before download')});
  assert.deepEqual(result.images,[]);assert.equal(result.failures.length,3);
  assert.ok(result.failures.every(row=>row.category==='image_cancelled'));
}));

const commentRequest=()=>({operation:'photo_acquire_only',account:'baw-russia',request:{version:2,purpose:'photo_acquire_only',receiptId,sourceDigest:'a'.repeat(64),
  sourceComment:{id:'native-comment',sourceVersion:'b'.repeat(64),sourceRole:{role:'customer',messageId:'message',roleEvidence:'connector-observed'},
    attachments:[{type:'photo',url:'https://cdn.example/own.png'},{type:'video',url:'https://cdn.example/NEVER-DOWNLOAD.mp4'},{type:'sticker',url:'https://cdn.example/own.png'}]}}});

test('original comment photos retain role and original slots without post or model evidence',()=>fixture(async(scratch,options)=>{
  const input=commentRequest(),before=JSON.stringify(input),calls=[];
  const result=await runPhotoAcquisition(input,{...options,download:async url=>{calls.push(url);return downloaded();}});
  assert.deepEqual(calls,['https://cdn.example/own.png','https://cdn.example/own.png']);assert.equal(JSON.stringify(input),before);
  assert.deepEqual(result.images.map(row=>row.attachmentIndex),[0,2]);assert.deepEqual(result.failures,[]);
  for(const row of result.images){assert.equal(row.itemId,'native-comment');assert.deepEqual(row.sourceRole,input.request.sourceComment.sourceRole);assert.equal(row.postId,undefined);assert.equal(row.imageNumber,undefined);assert.equal(row.artifact,undefined);
    assert.deepEqual(await fs.readFile(path.join(scratch,'photo-acquisition','baw-russia',receiptId,`photo-${row.attachmentIndex}.image`)),png);}
  assert.ok(!JSON.stringify(result).includes('imageEvidence'));assert.ok(!JSON.stringify(result).includes('photo-slot'));
}));
test('comment acquisition failure remains scoped to the original message and slot',()=>fixture(async(_scratch,options)=>{
  const result=await runPhotoAcquisition(commentRequest(),{...options,download:async()=>{throw Object.assign(new Error('PRIVATE'),{imageCategory:'image_http_forbidden'});}});
  assert.deepEqual(result.images,[]);assert.deepEqual(result.failures.map(row=>[row.itemId,row.attachmentIndex,row.sourceRole.role,row.postId]),[['native-comment',0,'customer',undefined],['native-comment',2,'customer',undefined]]);
  assert.ok(!JSON.stringify(result).includes('PRIVATE'));
}));
test('comment transport forbids post coercion and caller delivery claims before IO',async()=>{
  for(const mutate of [input=>input.request.sourcePost={},input=>input.request.sourceComment.sourceRole.role='post',input=>input.request.sourceComment.sourceRole.roleEvidence={},
    input=>input.request.sourceComment.postId='fake-post',input=>input.request.sourceComment.attachments[0].url=undefined,input=>input.request.imageEvidence=[]]){
    const input=commentRequest();mutate(input);await assert.rejects(runPhotoAcquisition(input,{env:{},download:()=>assert.fail('network forbidden')}),{code:'PHOTO_ACQUISITION_INVALID_REQUEST'});
  }
});

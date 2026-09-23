import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {createHash,randomUUID} from 'node:crypto';
import {admitMediaVisionBatch,assertLocalVisionModel,localBackendConfig,manifestDigest,runLocalVisionBatch,runMediaVision,stableJson,
  stageMediaVisionFrames,validateMediaVisionRequest} from './media-vision.mjs';
import {prepareAssistantRequest} from './assistant.mjs';

// Synthetic two-pixel JPEG, generated locally; no customer/video data or model call.
const JPEG=await fs.readFile(new URL('./fixtures/red-2x2.jpg',import.meta.url));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');

async function fixture(){
  const scratch=await fs.mkdtemp(path.join(os.tmpdir(),'ch-vision-scratch-'));
  const workId=`media-${randomUUID()}`,framesDir=path.join(scratch,workId,'vision-frames');
  await fs.mkdir(framesDir,{recursive:true});
  const frames=[];
  for(let index=0;index<2;index++){
    const file=path.join(framesDir,`frame-${String(index+1).padStart(3,'0')}.jpg`);
    await fs.writeFile(file,JPEG);frames.push({id:`f${index+1}`,path:file,sha256:sha(JPEG),timestampMs:index*1000});
  }
  const request={operation:'media_vision',account:'likeavto',schemaVersion:1,workId,
    createdAtUtc:new Date().toISOString(),source:{account:'LikeAvto',postKey:'native:exact-post',mediaSha256:'a'.repeat(64),durationMs:1100},
    coverage:{kind:'sampled_frames',samplingVersion:1,durationMs:1100,regularIntervalMs:2000,tailWindowMs:10000,
      tailIntervalMs:1000,maxGapMs:1000,tailStartMs:0,endingFrameId:'f2'},frames};
  request.manifestSha256=manifestDigest(request);
  return {scratch,request,cleanup:()=>fs.rm(scratch,{recursive:true,force:true})};
}
const none=id=>({id,status:'none',scene:'Красный тестовый кадр без видимого текста.',text:[],numbers:[],uncertainties:[]});
const readable=id=>({id,status:'readable',scene:'Табличка с ценой.',text:['12 900 ₽'],
  numbers:[{raw:'12 900 ₽',value:'12900',unit:null,currency:'₽',uncertain:false}],uncertainties:[]});

test('canonical digest is order independent and exact source frame bytes are admitted',async()=>{
  const f=await fixture();try{
    assert.equal(stableJson({b:1,a:{z:2,y:3}}),stableJson({a:{y:3,z:2},b:1}));
    const validated=await validateMediaVisionRequest(f.request,{scratchRoot:f.scratch});
    assert.equal(validated.frames.length,2);assert.deepEqual(validated.frames.map(x=>x.timestampMs),[0,1000]);
    const home=path.join(f.scratch,'private');await fs.mkdir(home);
    const staged=await stageMediaVisionFrames(validated,home);
    assert.equal(sha(await fs.readFile(staged[0].path)),f.request.frames[0].sha256);
    assert.notEqual(staged[0].path,f.request.frames[0].path);
  }finally{await f.cleanup();}
});

test('manifest, source, frame bytes, path and freshness changes fail before inference',async()=>{
  const f=await fixture();try{
    for(const mutate of [
      r=>r.manifestSha256='b'.repeat(64),
      r=>{r.source.account='baw-russia';r.manifestSha256=manifestDigest(r);},
      r=>{r.frames[0].sha256='b'.repeat(64);r.manifestSha256=manifestDigest(r);},
      r=>{r.frames[0].path=path.join(f.scratch,'outside','frame-001.jpg');r.manifestSha256=manifestDigest(r);},
      r=>{r.frames[0].path=path.join(f.scratch,r.workId,'vision-frames','..','frame-001.jpg');r.manifestSha256=manifestDigest(r);},
      r=>{r.coverage.endingFrameId='f1';r.manifestSha256=manifestDigest(r);},
      r=>{r.createdAtUtc=new Date(Date.now()-30*60_000).toISOString();r.manifestSha256=manifestDigest(r);}
    ]){const r=structuredClone(f.request);mutate(r);await assert.rejects(validateMediaVisionRequest(r,{scratchRoot:f.scratch}),/MEDIA_VISION_/);}
    await fs.writeFile(f.request.frames[1].path,Buffer.from('not an image'));
    await assert.rejects(validateMediaVisionRequest(f.request,{scratchRoot:f.scratch}),/MEDIA_VISION_FRAME_INVALID/);
  }finally{await f.cleanup();}
});

test('exact batch union admits prices with units; empty, duplicate and uncertain outputs cannot claim complete',()=>{
  const expected=[{id:'f1',sha256:'a'.repeat(64),timestampMs:0},{id:'f2',sha256:'b'.repeat(64),timestampMs:1000}];
  const accepted=admitMediaVisionBatch({frames:[readable('f1'),none('f2')],summary:'Только видимые детали.'},expected);
  assert.equal(accepted.frames[0].numbers[0].currency,'₽');assert.equal(accepted.frames[1].status,'none');
  for(const raw of [
    {frames:[],summary:'Тишина'},
    {frames:[none('f1'),none('f1')],summary:'Тишина'},
    {frames:[{...readable('f1'),numbers:[],text:[]},none('f2')],summary:'Тишина'},
    {frames:[{...none('f1'),status:'unreadable'},none('f2')],summary:'Тишина'},
    {frames:[{...readable('f1'),numbers:[{...readable('f1').numbers[0],uncertain:true}]},none('f2')],summary:'Тишина'}
  ])assert.throws(()=>admitMediaVisionBatch(raw,expected),/MEDIA_VISION_OUTPUT_INVALID/);
});

test('backend-neutral runner covers every exact frame in a private copy and marks unreadable incomplete',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let calls=0,batches=0,checks=0;
    const run=async(secondStatus)=>runMediaVision(f.request,{scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,
      withLane:async(_base,lane,fn)=>{assert.equal(lane,'media_vision');const home=path.join(f.scratch,`private-${++calls}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{checks++;},localBatch:async(batch)=>{
        batches++;assert.equal(batch.length,1);assert.ok(batch[0].path.includes('private-'));
        const id=batch[0].id;
        const frames=[id==='f1'?readable(id):secondStatus==='unreadable'?{...none(id),status:'unreadable',uncertainties:['Текст размыт.']}:
          secondStatus==='qualifier'?{...readable(id),text:['от 3 240 000 ₽'],numbers:[{raw:'от 3 240 000 ₽',value:'3240000',unit:null,currency:'₽',uncertain:false}],
            uncertainties:['Показана начальная цена: «от».']}:none(id)];
        return {frames,summary:'Описание только этого кадра.'};
      }});
    const complete=await run('none');assert.equal(complete.status,'complete');assert.equal(complete.frames.length,2);
    assert.equal(complete.source.postKey,f.request.source.postKey);assert.equal(complete.manifestSha256,f.request.manifestSha256);
    assert.equal(complete.provenance.model,'synthetic-vlm:1@sha256:'+'e'.repeat(64));
    const incomplete=await run('unreadable');assert.equal(incomplete.status,'incomplete');
    const qualified=await run('qualifier');assert.equal(qualified.status,'complete');
    assert.equal(qualified.frames[1].numbers[0].raw,'от 3 240 000 ₽');
    assert.equal(batches,6);assert.equal(checks,6);
  }finally{await f.cleanup();}
});

test('local backend has only an exact loopback endpoint and named model',()=>{
  const good={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'qwen3-vl:4b',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
  assert.equal(localBackendConfig(good).model,'qwen3-vl:4b');
  for(const endpoint of ['https://127.0.0.1:11434','http://localhost:11434','http://192.168.1.5:11434','http://127.0.0.1:11434/other'])
    assert.throws(()=>localBackendConfig({...good,COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:endpoint}),/MEDIA_VISION_BACKEND_UNAVAILABLE/);
  for(const model of ['qwen3-vl:cloud','other:7b-cloud'])
    assert.throws(()=>localBackendConfig({...good,COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:model}),/MEDIA_VISION_BACKEND_UNAVAILABLE/);
  assert.throws(()=>localBackendConfig({...good,COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:undefined}),/MEDIA_VISION_BACKEND_UNAVAILABLE/);
});

function fakeHttp(responses,calls){
  return (url,_options,callback)=>{
    const req=new EventEmitter();req.setTimeout=()=>{};req.destroy=error=>req.emit('error',error);
    req.end=body=>queueMicrotask(()=>{
      calls.push({path:url.pathname,body});
      const res=new EventEmitter();res.statusCode=200;callback(res);
      res.emit('data',Buffer.from(JSON.stringify(responses[url.pathname])));res.emit('end');
    });
    return req;
  };
}
test('offline model identity check requires exact installed local vision digest and excludes remote entries',async()=>{
  const config=localBackendConfig({COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
    COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'qwen3-vl:4b-instruct-q4_K_M',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)});
  const responses={'/api/tags':{models:[{name:config.model,model:config.model,digest:config.digest,size:3295636231}]},
    '/api/show':{capabilities:['completion','vision','tools']}};
  const calls=[];await assertLocalVisionModel(config,{requestFn:fakeHttp(responses,calls)});
  assert.deepEqual(calls.map(x=>x.path),['/api/tags','/api/show']);
  assert.deepEqual(JSON.parse(calls[1].body),{model:config.model});
  for(const bad of [
    {...responses,'/api/tags':{models:[{...responses['/api/tags'].models[0],digest:'f'.repeat(64)}]}},
    {...responses,'/api/tags':{models:[{...responses['/api/tags'].models[0],remote_model:'cloud-model'}]}},
    {...responses,'/api/show':{capabilities:['completion']}}
  ])await assert.rejects(assertLocalVisionModel(config,{requestFn:fakeHttp(bad,[])}),/MEDIA_VISION_BACKEND_UNAVAILABLE/);
});

test('assistant receives path-free, source-only sampled frames and rejects foreign or unpinned evidence',async()=>{
  const f=await fixture();try{
    const {operation,account,manifestSha256,frames,...requestRest}=f.request;
    const manifest={...requestRest,frames:frames.map(({id,sha256,timestampMs})=>({id,sha256,timestampMs}))};
    const summary='Наблюдение в двух выбранных кадрах.';
    const evidence={schemaVersion:1,manifest,durableManifestSha256:sha(stableJson(manifest)),result:{schemaVersion:1,status:'complete',
      source:structuredClone(f.request.source),manifestSha256,coverage:structuredClone(f.request.coverage),
      frames:[{...readable('f1'),sha256:frames[0].sha256,timestampMs:0},
        {...none('f2'),sha256:frames[1].sha256,timestampMs:1000}],summary,
      provenance:{backend:'local_ollama',model:'qwen3-vl:4b@sha256:'+'e'.repeat(64),instructionSha256:'f'.repeat(64)}}};
    const material={id:'visual-1',account:'LikeAvto',kind:'visual_context',trust:'source_only',postKey:'native:exact-post',
      text:summary,knowledgeEntryId:'entry-1',knowledgeVersionId:'version-1',visualEvidence:evidence};
    const manifestEntry={entryId:'entry-1',versionId:'version-1',kind:'visual_context',trust:'source_only',hash:'h'};
    const request={account:'likeavto',posts:[{postKey:'native:exact-post'}],materials:[material],knowledgeManifest:[manifestEntry]};
    const prepared=prepareAssistantRequest(request);
    assert.equal(prepared.payload.materials[0].visualEvidence.frames[0].numbers[0].currency,'₽');
    assert.equal(prepared.input.includes(f.request.frames[0].path),false);
    assert.equal(prepared.payload.materials[0].visualEvidence.coverage.kind,'sampled_frames');
    for(const change of [
      r=>{r.materials[0].trust='verified';},
      r=>{r.knowledgeManifest=[];},
      r=>{r.materials[0].visualEvidence.manifest.frames[0].path='C:/private/frame.jpg';},
      r=>{r.materials[0].visualEvidence.result.frames[0].sha256='a'.repeat(64);},
      r=>{r.materials[0].visualEvidence.manifest.source.account='BAW Russia';},
      r=>{r.materials[0].visualEvidence.result.status='incomplete';}
    ]){const bad=structuredClone(request);change(bad);assert.throws(()=>prepareAssistantRequest(bad),{code:'ASSISTANT_INVALID_REQUEST'});}
  }finally{await f.cleanup();}
});

test('local chat admits only completed model-bound tool-free response and binds actual staged bytes',async()=>{
  const f=await fixture();try{
    const frame=f.request.frames[0],model='synthetic-vlm:1';
    const valid={model,done:true,message:{role:'assistant',content:JSON.stringify({frames:[none('f1')],summary:'Красный кадр.'})}};
    const invoke=(outer,requestFn)=>runLocalVisionBatch([frame],{endpoint:'http://127.0.0.1:11434',model},50,
      {requestFn:requestFn??fakeHttp({'/api/chat':outer},[])});
    const calls=[];
    assert.equal((await invoke(valid,fakeHttp({'/api/chat':valid},calls))).frames[0].id,'f1');
    const sent=JSON.parse(calls[0].body);
    assert.equal(sent.keep_alive,'5m');assert.deepEqual(sent.options,{temperature:0,num_ctx:4096,num_predict:1536});
    assert.equal(sent.messages[1].images.length,1);assert.equal(sent.messages[1].images[0],JPEG.toString('base64'));
    for(const outer of [{...valid,done:false},{...valid,model:'other'},
      {...valid,message:{...valid.message,tool_calls:[{function:{name:'shell'}}]}}])
      await assert.rejects(invoke(outer),/MEDIA_VISION_OUTPUT_INVALID/);
    await fs.writeFile(frame.path,Buffer.concat([JPEG,Buffer.from('tamper')]));
    await assert.rejects(invoke(valid),/MEDIA_VISION_STAGE_FAILED/);
    await fs.writeFile(frame.path,JPEG);
    const stuck=()=>{const req=new EventEmitter();req.setTimeout=()=>{};req.end=()=>{};
      req.destroy=error=>req.emit('error',error);return req;};
    await assert.rejects(invoke(valid,stuck),/MEDIA_VISION_TIMEOUT/);
  }finally{await f.cleanup();}
});

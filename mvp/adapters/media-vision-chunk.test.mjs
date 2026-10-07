import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {createHash,randomUUID} from 'node:crypto';
import {chunkManifestDigest,LEGACY_LOCAL_PARTIAL_POLICY_SHA,PREVIOUS_LOCAL_PARTIAL_POLICY_SHA,LOCAL_PARTIAL_POLICY_SHA,persistVisionFailureDiagnostic,runMediaVisionChunk,stageMediaVisionChunk,validateMediaVisionChunk} from './media-vision-chunk.mjs';
import {prepareAssistantRequest} from './assistant.mjs';
import {admitMediaVisionBatch,runLocalVisionBatch,stableJson} from './media-vision.mjs';
import {rememberClosedCodexFailure,mediaGpuFailureProof} from './media-gpu-outcome.mjs';

const PNG=await fs.readFile(new URL('./fixtures/red-2x2.png',import.meta.url));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');

async function fixture(){
  const scratch=await fs.mkdtemp(path.join(os.tmpdir(),'ch-vision-v2-'));
  const workId=`media-${randomUUID()}`,frameDir=path.join(scratch,workId,'vision-frames');
  await fs.mkdir(frameDir,{recursive:true});
  const frames=[];
  for(const [selectionIndex,frameIndex,reasons] of [[64,100,['scene_before']],[95,500,['last','transient_pulse']]]){
    const id=`frame-${String(frameIndex).padStart(12,'0')}`;
    const file=path.join(frameDir,`${id}-${randomUUID().replaceAll('-','')}.png`);
    await fs.writeFile(file,PNG);
    frames.push({id,frameIndex,selectionIndex,selectionReasons:reasons,pts:String(frameIndex*33-1),
      timestampMs:frameIndex*33,pixelSha256:sha(Buffer.alloc(12,255)),sha256:sha(PNG),path:file,mimeType:'image/png'});
  }
  const request={operation:'media_vision_chunk',account:'likeavto',schemaVersion:2,workId,
    createdAtUtc:new Date().toISOString(),source:{account:'LikeAvto',postKey:'native:post',mediaSha256:'a'.repeat(64),durationMs:600000},
    inventory:{sha256:'b'.repeat(64),frameCount:18000,selectionSha256:'c'.repeat(64),
      selectionPolicySha256:'d'.repeat(64),selectedFrameCount:1800,decoderContractSha256:'e'.repeat(64),
      timeBaseNumerator:1,timeBaseDenominator:1000,width:2,height:2,pixelFormat:'rgb24'},
    chunk:{firstSelectionIndex:64,endSelectionIndexExclusive:96,previousReceiptSha256:'f'.repeat(64),leaseId:'lease-v2'},frames};
  request.manifestSha256=chunkManifestDigest(request);
  return {scratch,request,cleanup:()=>fs.rm(scratch,{recursive:true,force:true})};
}
const readable=id=>({id,status:'readable',scene:'Красный фон.',text:['от 3 240 000 ₽'],
  numbers:[{raw:'от 3 240 000 ₽',value:'3240000',unit:null,currency:'₽',uncertain:false}],
  uncertainties:['Указана начальная цена «от».']});
const none=id=>({id,status:'none',scene:'Красный фон.',text:[],numbers:[],uncertainties:[]});

test('chunk resource proof rejects any earlier local fallback even when final Codex child closed',async()=>{
  const f=await fixture();try{
    for(const primary of ['codex_luna','local']){
      let localCalls=0,cloudCalls=0,caught;
      try{await runMediaVisionChunk(f.request,{scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',enablePartialCache:false,
        env:{COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)},
        readRouting:async()=>({schemaVersion:1,account:'likeavto',defaultBackend:primary,automaticFallback:primary==='local',cloudMaxSelectedFrames:2000,sourceOverrides:{}}),
        secureHome:async()=>{},verifyLocalModel:async()=>{},withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,randomUUID());await fs.mkdir(home);return fn(home);},
        localBatch:async()=>{localCalls++;throw Object.assign(new Error('unknown remote local request'),{code:'MEDIA_VISION_TIMEOUT'});},
        codexBatch:async()=>{cloudCalls++;throw rememberClosedCodexFailure(Object.assign(new Error('same failure'),{code:'MEDIA_VISION_CODEX_FAILED'}),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:1}});}
      });}catch(error){caught=error;}
      assert.equal(caught.code,'MEDIA_VISION_CODEX_FAILED');assert.equal(cloudCalls,1);
      assert.equal(localCalls,primary==='local'?1:0);assert.equal(!!mediaGpuFailureProof(caught),primary==='codex_luna');
      if(primary==='codex_luna')assert.equal(mediaGpuFailureProof(caught).requestSha256,f.request.manifestSha256);
    }
  }finally{await f.cleanup();}
});

test('route is pinned per chunk, batches Luna images and only explicitly fails over',async()=>{
  const f=await fixture();try{
    let runs=0,cloudCalls=0,localCalls=0;
    const config={schemaVersion:1,account:'likeavto',defaultBackend:'codex_luna',automaticFallback:false,cloudMaxSelectedFrames:2000,sourceOverrides:{}};
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,enablePartialCache:false,
      readRouting:async()=>structuredClone(config),secureHome:async()=>{},verifyLocalModel:async()=>{},
      withLane:async(_base,_lane,fn)=>{const h=path.join(f.scratch,`route-${++runs}`);await fs.mkdir(h);return fn(h);},
      localBatch:async b=>{localCalls++;return {frames:b.map(x=>none(x.id)),summary:'Просмотрено.'};},
      codexBatch:async b=>{cloudCalls++;assert.equal(b.length,2);return {frames:b.map(x=>readable(x.id)),summary:'Просмотрено.'};}};
    const cloud=await runMediaVisionChunk(f.request,options);
    assert.equal(cloud.provenance.backend,'codex_isolated');assert.equal(cloud.provenance.model,'gpt-6.1-sol');
    assert.equal(cloudCalls,1);assert.equal(localCalls,0);assert.deepEqual(cloud.chunk,f.request.chunk);
    config.defaultBackend='codex_sol';
    options.codexBatch=async(b,_timeout,options)=>{assert.equal(options.model,'gpt-6.1-sol');return {frames:b.map(x=>readable(x.id)),summary:'Просмотрено.'};};
    const sol=await runMediaVisionChunk(f.request,options);
    assert.equal(sol.provenance.model,'gpt-6.1-sol');assert.equal(sol.provenance.backend,'codex_isolated');
    options.codexBatch=async()=>{throw Object.assign(new Error('missing model'),{code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});};
    await assert.rejects(()=>runMediaVisionChunk(f.request,options));assert.equal(localCalls,0);
    config.automaticFallback=true;
    const fallback=await runMediaVisionChunk(f.request,options);assert.equal(fallback.provenance.backend,'local_ollama');assert.equal(localCalls,2);
    options.codexBatch=async()=>{throw Object.assign(new Error('changed image'),{code:'MEDIA_VISION_CODEX_FRAME_CHANGED'});};
    await assert.rejects(()=>runMediaVisionChunk(f.request,options));assert.equal(localCalls,2);
  }finally{await f.cleanup();}
});

test('fixed two-per-second reasons retain sealed identity and reject invented reasons',async()=>{
  const f=await fixture();try{
    f.request.frames[0].selectionReasons=['second_midpoint'];
    f.request.frames[1].selectionReasons=['second_end'];
    f.request.manifestSha256=chunkManifestDigest(f.request);
    await validateMediaVisionChunk(f.request,{scratchRoot:f.scratch});
    f.request.frames[0].selectionReasons=['invented_reason'];
    f.request.manifestSha256=chunkManifestDigest(f.request);
    await assert.rejects(()=>validateMediaVisionChunk(f.request,{scratchRoot:f.scratch}));
  }finally{await f.cleanup();}
});

test('v2 admits sparse selected positions across a long full-frame inventory, exact PNG bytes and sealed identity',async()=>{
  const f=await fixture();try{
    const admitted=await validateMediaVisionChunk(f.request,{scratchRoot:f.scratch});
    assert.deepEqual(admitted.frames.map(x=>x.selectionIndex),[64,95]);
    assert.equal(admitted.inventory.frameCount,18000);
    assert.equal(admitted.inventory.selectedFrameCount,1800);
    const home=path.join(f.scratch,'private');await fs.mkdir(home);
    const staged=await stageMediaVisionChunk(admitted,home);
    assert.equal(sha(await fs.readFile(staged[0].path)),f.request.frames[0].sha256);
    assert.notEqual(staged[0].path,f.request.frames[0].path);
  }finally{await f.cleanup();}
});

test('v2 rejects changed account, lease, selection, encoded image, path and dimensions before model work',async()=>{
  const f=await fixture();try{
    for(const mutate of [
      r=>{r.source.account='BAW Russia';r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.chunk.previousReceiptSha256='0'.repeat(64);},
      r=>{r.frames[0].selectionIndex=96;r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.frames[0].selectionReasons=['unexpected'];r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.frames[0].pixelSha256='not-a-sha';r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.frames[0].path=path.join(f.scratch,'other',path.basename(r.frames[0].path));r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.inventory.width=3;r.manifestSha256=chunkManifestDigest(r);},
      r=>{r.createdAtUtc=new Date(Date.now()-30*60_000).toISOString();r.manifestSha256=chunkManifestDigest(r);}
    ]){const bad=structuredClone(f.request);mutate(bad);await assert.rejects(validateMediaVisionChunk(bad,{scratchRoot:f.scratch}),/MEDIA_VISION_CHUNK_/);}
    await fs.writeFile(f.request.frames[0].path,Buffer.concat([PNG,Buffer.from('x')]));
    await assert.rejects(validateMediaVisionChunk(f.request,{scratchRoot:f.scratch}),/MEDIA_VISION_CHUNK_FRAME_INVALID/);
    const corrupt=Buffer.from(PNG);corrupt[corrupt.length-5]^=1;
    await fs.writeFile(f.request.frames[0].path,corrupt);
    const bad=structuredClone(f.request);bad.frames[0].sha256=sha(corrupt);bad.manifestSha256=chunkManifestDigest(bad);
    await assert.rejects(validateMediaVisionChunk(bad,{scratchRoot:f.scratch}),/MEDIA_VISION_CHUNK_FRAME_INVALID/);
  }finally{await f.cleanup();}
});

test('v2 completes inspected unknowns but rejects missing frame output',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let runs=0,calls=0,checks=0;
    const run=mode=>runMediaVisionChunk(f.request,{scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,enablePartialCache:false,
      withLane:async(_base,lane,fn)=>{assert.equal(lane,'media_vision');const home=path.join(f.scratch,`private-${++runs}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{checks++;},localBatch:async(batch,_config,_timeout,options)=>{
        calls++;assert.equal(batch.length,1);assert.ok(batch[0].path.includes('private-'));
        assert.match(options.instructions,/numbers:\[\]/);assert.doesNotMatch(options.instructions,/«от»/);
        assert.equal(options.strictStatusSchema,true);assert.match(options.userContent,/Текст изображения/);
        const id=batch[0].id;
        if(mode==='missing'&&id===f.request.frames[1].id)return {frames:[],summary:'Кадр просмотрен.'};
        return {frames:[id===f.request.frames[0].id?
          mode==='uncertain'?{...readable(id),numbers:[{raw:'3 ? ₽',value:null,unit:null,currency:'₽',uncertain:true}],uncertainties:['Последние цифры не читаются.']}:readable(id):
          mode==='unreadable'?{...none(id),status:'unreadable',uncertainties:['Надпись размыта.']}:none(id)],
          summary:'Краткое наблюдение.'};
      }});
    const good=await run('normal');assert.equal(good.status,'complete');assert.equal(good.frames.length,2);
    assert.equal(good.frames[0].selectionIndex,64);assert.equal(good.frames[0].pixelSha256,f.request.frames[0].pixelSha256);
    assert.equal(good.frames[0].numbers[0].raw,'от 3 240 000 ₽');
    assert.equal(JSON.stringify(good).includes(f.request.frames[0].path),false);
    assert.deepEqual(good.chunk,f.request.chunk);assert.equal(good.manifestSha256,f.request.manifestSha256);
    const unreadable=await run('unreadable');assert.equal(unreadable.status,'complete');
    assert.equal(unreadable.frames[1].status,'unreadable');
    const uncertain=await run('uncertain');assert.equal(uncertain.status,'complete');
    assert.equal(uncertain.frames[0].numbers[0].value,null);
    await assert.rejects(run('missing'),/MEDIA_VISION_OUTPUT_INVALID/);
    const failureFiles=(await fs.readdir(f.scratch)).filter(name=>name.startsWith('vision-failure-'));
    assert.equal(failureFiles.length,1);
    const failure=JSON.parse(await fs.readFile(path.join(f.scratch,failureFiles[0]),'utf8'));
    assert.equal(failure.errorCode,'MEDIA_VISION_OUTPUT_INVALID');
    assert.equal(failure.output.category,'frame_count');
    assert.equal(calls,8);assert.equal(checks,7);
  }finally{await f.cleanup();}
});

test('failed inference retains a private content-free diagnostic',async()=>{
  const f=await fixture();try{
    const trace={backend:'codex_sol',batchOffset:0,batch:{inferenceProcessError:'ADAPTER_PROCESS_FAILED',
      transport:{done:true,modelMatches:true,assistantRole:true,toolCalls:false,contentType:'string',doneReason:'length'},
      eventFailure:{kind:'turn_failed',category:'schema',messageSha256:'a'.repeat(64),
        message:'private frame text'},output:{category:'none_with_content',expectedFrames:1,frameOffset:0}}};
    const file=await persistVisionFailureDiagnostic(f.scratch,f.request,
      Object.assign(new Error('private stderr'),{code:'MEDIA_VISION_CODEX_FAILED'}),trace);
    const saved=JSON.parse(await fs.readFile(file,'utf8'));
    assert.equal(saved.errorCode,'MEDIA_VISION_CODEX_FAILED');
    assert.equal(saved.eventFailure.category,'schema');
    assert.equal(saved.output.category,'none_with_content');
    assert.equal(saved.transport.doneReason,'length');
    assert.equal(JSON.stringify(saved).includes('private'),false);
    assert.ok(path.basename(file).startsWith(`vision-failure-${f.request.workId}-`));
  }finally{await f.cleanup();}
});
test('local transport failure is persisted through error metadata with a strict safe field allowlist',async()=>{
  const f=await fixture();try{
    const error=Object.assign(new Error('private HTTP response'),{code:'MEDIA_VISION_BACKEND_UNAVAILABLE',localTransportDiagnostic:{
      stage:'chat',category:'http_status',httpStatus:500,responseBytes:91,responseSha256:'a'.repeat(64),backendErrorCategory:'memory',networkCode:null,
      body:'private prompt or frame text',url:'http://private/?token=secret'}});
    const file=await persistVisionFailureDiagnostic(f.scratch,f.request,error,{backend:'local'});
    const saved=JSON.parse(await fs.readFile(file,'utf8'));
    assert.equal(saved.localTransport.httpStatus,500);assert.equal(saved.localTransport.backendErrorCategory,'memory');
    assert.equal(saved.localTransport.stage,'chat');assert.equal(JSON.stringify(saved).includes('private'),false);
    assert.equal(JSON.stringify(saved).includes('secret'),false);
    error.localTransportDiagnostic={stage:'private',category:'private',httpStatus:900,responseBytes:-1,
      responseSha256:'private',backendErrorCategory:'private',networkCode:'private'};
    const sanitized=JSON.parse(await fs.readFile(await persistVisionFailureDiagnostic(f.scratch,f.request,error,{backend:'local'}),'utf8'));
    assert.deepEqual(sanitized.localTransport,{stage:'other',category:'other',httpStatus:null,responseBytes:null,
      responseSha256:null,backendErrorCategory:null,networkCode:null});
  }finally{await f.cleanup();}
});

test('local truncated JSON retries once with a bounded larger context; other invalid output stays held',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,calls=[];
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,enablePartialCache:false,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`retry-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch,_local,_timeout,request)=>{
        calls.push({id:batch[0].id,numCtx:request.numCtx,numPredict:request.numPredict,
          repeatPenalty:request.repeatPenalty});
        if(calls.length===1){request.diagnostic.transport={doneReason:'length'};
          request.diagnostic.output={category:'invalid_json',expectedFrames:1};
          throw Object.assign(new Error('truncated'),{code:'MEDIA_VISION_OUTPUT_INVALID'});}
        return {frames:[none(batch[0].id)],summary:'Просмотрено.'};
      }};
    const result=await runMediaVisionChunk(f.request,options);
    assert.equal(result.frames.length,2);
    assert.deepEqual(calls.map(call=>[call.numCtx,call.numPredict]),[[undefined,undefined],[8192,3072],[undefined,undefined]]);
    assert.deepEqual(calls.map(call=>call.repeatPenalty),[undefined,1.1,undefined]);
    calls=[];
    options.localBatch=async(batch,_local,_timeout,request)=>{calls.push(batch[0].id);
      request.diagnostic.transport={doneReason:'stop'};
      request.diagnostic.output={category:'invalid_json',expectedFrames:1};
      throw Object.assign(new Error('invalid'),{code:'MEDIA_VISION_OUTPUT_INVALID'});};
    await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_OUTPUT_INVALID'});
    assert.equal(calls.length,1);
  }finally{await f.cleanup();}
});

test('local admitted frames survive a failed chunk and only exact source, selection, pixels and model can reuse them',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,failSecond=true,calls=[];
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`partial-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch)=>{
        calls.push(batch[0].selectionIndex);
        if(failSecond&&batch[0].selectionIndex===95)
          throw Object.assign(new Error('held second frame'),{code:'MEDIA_VISION_OUTPUT_INVALID'});
        return {frames:[none(batch[0].id)],summary:'Кадр просмотрен.'};
      }};
    await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_OUTPUT_INVALID'});
    assert.deepEqual(calls,[64,95]);
    const cacheDir=path.join(f.scratch,'vision-local-partials-v1');
    const entries=await fs.readdir(cacheDir);assert.equal(entries.length,1);
    const saved=JSON.parse(await fs.readFile(path.join(cacheDir,entries[0]),'utf8'));
    assert.equal(saved.identity.source.mediaSha256,f.request.source.mediaSha256);
    assert.equal(saved.identity.inventory.selectionSha256,f.request.inventory.selectionSha256);
    assert.equal(saved.identity.frame.pixelSha256,f.request.frames[0].pixelSha256);
    assert.equal(saved.identity.backend.digest,env.COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST);
    assert.match(saved.identity.backend.policySha256,/^[a-f0-9]{64}$/);
    failSecond=false;calls=[];
    const retry=structuredClone(f.request);retry.chunk.leaseId='new-lease-after-hold';
    retry.manifestSha256=chunkManifestDigest(retry);
    const completed=await runMediaVisionChunk(retry,options);
    assert.equal(completed.frames.length,2);assert.deepEqual(calls,[95]);
    for(const [mutate,expectedCalls] of [
      [r=>{r.source.mediaSha256='1'.repeat(64);},[64,95]],
      [r=>{r.inventory.selectionSha256='2'.repeat(64);},[64,95]],
      [r=>{r.frames[0].pixelSha256='3'.repeat(64);},[64]]
    ]){
      const changed=structuredClone(retry);mutate(changed);changed.manifestSha256=chunkManifestDigest(changed);
      calls=[];await runMediaVisionChunk(changed,options);assert.deepEqual(calls,expectedCalls);
    }
    calls=[];const changedConfig={...options,env:{...env,COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'4'.repeat(64)}};
    await runMediaVisionChunk(retry,changedConfig);assert.deepEqual(calls,[64,95]);
    calls=[];const changedEndpoint={...options,env:{...env,COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11435'}};
    await runMediaVisionChunk(retry,changedEndpoint);assert.deepEqual(calls,[64,95]);
    await fs.writeFile(path.join(cacheDir,entries[0]),'{"tampered":true}');
    calls=[];await assert.rejects(runMediaVisionChunk(retry,options),{code:'MEDIA_VISION_PARTIAL_CACHE_INVALID'});
    assert.deepEqual(calls,[],'corrupt partial proof must fail before inference');
  }finally{await f.cleanup();}
});

test('number shape re-inference is bounded, uses the same frame and never coerces uncertain facts',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,calls=[],mode='recover';const start=Date.now();let clock=start;
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,enablePartialCache:false,now:()=>clock,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`shape-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch,_local,timeout,request)=>{
        calls.push({frame:batch[0],request,timeout});
        if(mode==='deadline')clock=start+3_540_000;
        const frame=readable(batch[0].id);
        if(mode==='semantic')frame.numbers[0].uncertain=true;
        else if(!request.strictNumberShape||mode==='persistent')frame.numbers[0].currency='';
        return {frames:[frame],summary:'Кадр просмотрен.'};
      }};
    const completed=await runMediaVisionChunk(f.request,options);
    assert.equal(completed.frames.length,2);assert.equal(calls.length,4);
    assert.equal(calls[0].frame,calls[1].frame);assert.equal(calls[2].frame,calls[3].frame);
    assert.equal(calls[1].request.strictNumberShape,true);
    assert.match(calls[1].request.userContent,/иначе null/);
    assert.equal(completed.frames[0].numbers[0].currency,'₽');
    mode='persistent';calls=[];
    await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_OUTPUT_INVALID'});
    assert.equal(calls.length,2,'one number schema retry only');
    const files=(await fs.readdir(f.scratch)).filter(file=>file.startsWith('vision-failure-'));
    const failure=JSON.parse(await fs.readFile(path.join(f.scratch,files[0]),'utf8'));
    assert.equal(failure.localNumberShapeRetry,true);assert.equal(failure.output.numberField,'currency');
    assert.equal(failure.output.numberIssue,'empty');assert.equal(failure.output.numberOffset,0);
    mode='semantic';calls=[];
    await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_OUTPUT_INVALID'});
    assert.equal(calls.length,1,'contradictory uncertainty must remain rejected');
    mode='deadline';calls=[];
    await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_TIMEOUT'});
    assert.equal(calls.length,1,'retry cannot reset original deadline');
  }finally{await f.cleanup();}
});

test('exact token repetition gets one same-frame retry, original deadline and full admission',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,calls=[],mode='recover',clock=Date.now();
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,enablePartialCache:false,now:()=>clock,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`repeat-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch,_local,timeout,request)=>{
        calls.push({batch,request,timeout});
        if(mode==='deadline')clock+=3_540_000;
        if(!request.repeatLastN||mode==='persistent'||mode==='unavailable'){
          throw Object.assign(new Error('safe'),{code:mode==='unavailable'?'MEDIA_VISION_BACKEND_UNAVAILABLE':'MEDIA_VISION_GENERATION_REPETITION',
            localTransportDiagnostic:{stage:'chat',category:'http_status',httpStatus:500,backendErrorCategory:'token_repetition'}});
        }
        const frame=readable(batch[0].id);if(mode==='invalid')frame.numbers[0].currency='';
        return {frames:[frame],summary:'Кадр просмотрен.'};
      }};
    const result=await runMediaVisionChunk(f.request,options);
    assert.equal(result.frames.length,2);assert.equal(calls.length,4);
    assert.equal(calls[0].batch,calls[1].batch);assert.equal(calls[0].request.instructions,calls[1].request.instructions);
    assert.equal(calls[0].request.userContent,calls[1].request.userContent);
    assert.equal(calls[1].request.repeatPenalty,1.1);assert.equal(calls[1].request.repeatLastN,256);
    assert.equal(calls[1].request.strictNumberShape,undefined);
    for(const [failure,code,count] of [['persistent','MEDIA_VISION_GENERATION_REPETITION',2],
      ['invalid','MEDIA_VISION_OUTPUT_INVALID',2],['unavailable','MEDIA_VISION_BACKEND_UNAVAILABLE',1],['deadline','MEDIA_VISION_TIMEOUT',1]]){
      mode=failure;calls=[];await assert.rejects(runMediaVisionChunk(f.request,options),{code});
      assert.equal(calls.length,count,'no third attempt, generic retry or deadline reset');
    }
    const records=await Promise.all((await fs.readdir(f.scratch)).filter(n=>n.startsWith('vision-failure-')).map(async n=>JSON.parse(await fs.readFile(path.join(f.scratch,n),'utf8'))));
    assert.ok(records.some(r=>r.localRepetitionRetry&&r.localTransport?.backendErrorCategory==='token_repetition'));
  }finally{await f.cleanup();}
});

test('automatic bounded Luna repair preserves cached local frames and exact mixed attribution; charged replay cannot dispatch',async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,cloudCalls=0,localCalls=0;
    const config={schemaVersion:2,account:'likeavto',defaultBackend:'local',automaticFallback:true,
      cloudMaxSelectedFrames:1000,sourceOverrides:{[f.request.source.mediaSha256]:'local'},
      boundedLocalFallback:{maxCloudFramesPerChunk:32,maxCloudInvocationsPerChunk:8,maxCloudFramesPerSource:64}};
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,readRouting:async()=>config,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`mixed-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch)=>{
        localCalls++;
        if(batch[0].id===f.request.frames[0].id)return {frames:[none(batch[0].id)],summary:'Просмотрено.'};
        throw Object.assign(new Error('safe'),{code:'MEDIA_VISION_GENERATION_REPETITION',
          localTransportDiagnostic:{stage:'chat',category:'http_status',httpStatus:500,backendErrorCategory:'token_repetition'}});
      },codexBatch:async(batch,_timeout,request)=>{
        cloudCalls++;assert.equal(batch.length,1);assert.equal(batch[0].id,f.request.frames[1].id);
        assert.equal(request.model,'gpt-6.1-sol');assert.match(request.instructions,/только видимая сцена/);
        assert.ok((await fs.readdir(f.scratch)).some(n=>n.startsWith('vision-frame-rescue-attempt-')),'budget is durable before cloud call');
        return {frames:[readable(batch[0].id)],summary:'Просмотрено.'};
      }};
    // Account permit files are not an input contract: only trusted routing can enable fallback.
    const unrelatedFile=path.join(f.scratch,'vision-frame-rescue-likeavto.json');
    await fs.writeFile(unrelatedFile,'unrelated owner file');
    const result=await runMediaVisionChunk(f.request,options);
    assert.equal(await fs.readFile(unrelatedFile,'utf8'),'unrelated owner file');
    assert.equal(localCalls,3);assert.equal(cloudCalls,1);assert.equal(result.frames.length,2);
    assert.equal(result.provenance.kind,'mixed_frames');assert.equal(result.provenance.schemaVersion,2);
    assert.deepEqual(result.provenance.frames.map(p=>[p.id,p.backend]),[[f.request.frames[0].id,'local_ollama'],[f.request.frames[1].id,'codex_isolated']]);
    assert.equal(result.provenance.rescue.cloudFrameCount,1);
    const dir=path.join(f.scratch,'vision-local-partials-v1');
    const cacheFiles=await fs.readdir(dir);assert.equal(cacheFiles.length,1);
    const before=await fs.readFile(path.join(dir,cacheFiles[0]));
    const retry=structuredClone(f.request);retry.chunk.leaseId='new-lease';retry.manifestSha256=chunkManifestDigest(retry);
    await assert.rejects(runMediaVisionChunk(retry,options),{code:'MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED'});
    assert.equal(cloudCalls,1);assert.deepEqual(await fs.readFile(path.join(dir,cacheFiles[0])),before);
    await fs.writeFile(path.join(dir,cacheFiles[0]),'{}');
    await assert.rejects(runMediaVisionChunk(retry,options),{code:'MEDIA_VISION_PARTIAL_CACHE_INVALID'});
    assert.equal(cloudCalls,1,'cache corruption must not activate a cloud repair');
  }finally{await f.cleanup();}
});

test('bounded fallback charges partial/unknown cloud failures and never cycles or switches unproven timeout',async()=>{
  for(const mode of ['partial','unknown','local_timeout','disabled','semantic','http500','wrong_model','wrong_role','tools','bad_content']){
    const f=await fixture();try{
      let cloudCalls=0,lane=0;
      const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
        COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
      const config={schemaVersion:2,account:'likeavto',defaultBackend:'local',automaticFallback:mode!=='disabled',cloudMaxSelectedFrames:1000,
        sourceOverrides:{},boundedLocalFallback:{maxCloudFramesPerChunk:32,maxCloudInvocationsPerChunk:8,maxCloudFramesPerSource:64}};
      const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,readRouting:async()=>config,
        withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`failed-${++lane}`);await fs.mkdir(home);return fn(home);},
        secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async(batch,_local,_timeout,request)=>{
          if(['semantic','wrong_model','wrong_role','tools','bad_content'].includes(mode)){
            const outer={model:mode==='wrong_model'?'foreign':_local.model,done:true,message:{role:mode==='wrong_role'?'user':'assistant',
              content:mode==='bad_content'?{}:JSON.stringify({frames:[],summary:'incomplete'}),...(mode==='tools'?{tool_calls:[{}]}:{})}};
            const fake=(_url,_options,callback)=>{
              const req=new EventEmitter();req.setTimeout=()=>{};req.destroy=error=>req.emit('error',error);
              req.end=()=>queueMicrotask(()=>{const res=new EventEmitter();res.statusCode=200;callback(res);
                res.emit('data',Buffer.from(JSON.stringify(outer)));res.emit('end');});return req;
            };
            return runLocalVisionBatch(batch,_local,_timeout,{...request,requestFn:fake});
          }
          throw Object.assign(new Error('safe'),{code:mode==='local_timeout'?'MEDIA_VISION_TIMEOUT':mode==='http500'?'MEDIA_VISION_BACKEND_UNAVAILABLE':'MEDIA_VISION_GENERATION_REPETITION',
            localTransportDiagnostic:{stage:'chat',category:mode==='local_timeout'?'deadline':'http_status',httpStatus:500,backendErrorCategory:'token_repetition'}});
        },codexBatch:async(batch)=>{cloudCalls++;
          if(mode==='unknown')throw Object.assign(new Error('unknown'),{code:'MEDIA_VISION_TIMEOUT'});
          return {frames:mode==='partial'?[]:batch.map(f=>none(f.id)),summary:'Просмотрено.'};
        }};
      if(['semantic','http500'].includes(mode)){const result=await runMediaVisionChunk(f.request,options);assert.equal(result.frames.length,2);assert.equal(cloudCalls,1);}
      else await assert.rejects(runMediaVisionChunk(f.request,options));
      const charged=(await fs.readdir(f.scratch)).filter(n=>n.startsWith('vision-frame-rescue-attempt-'));
      if(['local_timeout','disabled','wrong_model','wrong_role','tools','bad_content'].includes(mode)){assert.equal(cloudCalls,0);assert.equal(charged.length,0);}
      else{assert.equal(cloudCalls,1);assert.equal(charged.length,1);}
    }finally{await f.cleanup();}
  }
});

for(const historicalPolicy of [LEGACY_LOCAL_PARTIAL_POLICY_SHA,PREVIOUS_LOCAL_PARTIAL_POLICY_SHA])
test(`previous exact policy cache survives retry upgrade (${historicalPolicy}); changed pixels and invalid records reject`,async()=>{
  const f=await fixture();try{
    const env={COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT:'http://127.0.0.1:11434',
      COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL:'synthetic-vlm:1',COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST:'e'.repeat(64)};
    let lane=0,calls=0;
    const options={scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,
      withLane:async(_base,_lane,fn)=>{const home=path.join(f.scratch,`legacy-${++lane}`);await fs.mkdir(home);return fn(home);},
      secureHome:async()=>{},verifyLocalModel:async()=>{},localBatch:async batch=>{calls++;return {frames:[none(batch[0].id)],summary:'Просмотрено.'};}};
    await runMediaVisionChunk(f.request,options);assert.equal(calls,2);
    const dir=path.join(f.scratch,'vision-local-partials-v1'),records=[];
    for(const file of await fs.readdir(dir)){
      const record=JSON.parse(await fs.readFile(path.join(dir,file),'utf8'));
      await fs.unlink(path.join(dir,file));record.identity.backend.policySha256=historicalPolicy;
      record.digest=sha(stableJson({identity:record.identity,frame:record.frame}));
      const legacyFile=path.join(dir,sha(stableJson(record.identity))+'.json');records.push({legacyFile,record});
      await fs.writeFile(legacyFile,stableJson(record));
    }
    calls=0;await runMediaVisionChunk(f.request,options);assert.equal(calls,0);
    const changed=structuredClone(f.request);changed.frames[0].pixelSha256='7'.repeat(64);changed.manifestSha256=chunkManifestDigest(changed);
    calls=0;await runMediaVisionChunk(changed,options);assert.equal(calls,1);
    const {legacyFile,record}=records[0];record.frame.status='readable';
    record.digest=sha(stableJson({identity:record.identity,frame:record.frame}));await fs.writeFile(legacyFile,stableJson(record));
    calls=0;await assert.rejects(runMediaVisionChunk(f.request,options),{code:'MEDIA_VISION_PARTIAL_CACHE_INVALID'});
    assert.equal(calls,0,'even digest-valid legacy records need current semantic admission');
  }finally{await f.cleanup();}
});

test('assistant projects only final proof-gated selected-frame groups with source-bound qualifiers',async()=>{
  const f=await fixture();try{
    const evidence={schemaVersion:2,source:structuredClone(f.request.source),sourcePostVersion:'8'.repeat(64),
      finalEvidence:{sha256:'a'.repeat(64),bytes:4096},
      coverage:{kind:'all_frames_fast_selected_neural',selectionPolicySha256:'d'.repeat(64),
        frameCount:18000,selectedFrameCount:3,coveredSelectedFrameCount:3,uniqueReviewedFrames:2},
      aggregateOverflow:false,aggregate:f.request.frames.map((frame,index)=>({
        observation:{scene:'Красный фон.',text:index===0?['от 3 240 000 ₽']:[],
          numbers:index===0?[{raw:'от 3 240 000 ₽',value:'3240000',unit:null,currency:'₽',uncertain:false}]:[],
          uncertainties:index===0?['Цена указана как начальная.']:[]},
        sources:[{frameIndex:frame.frameIndex,pts:frame.pts,timestampMs:frame.timestampMs,pixelSha256:frame.pixelSha256}]
      }))};
    evidence.aggregate[1].sources[0].pixelSha256='9'.repeat(64);
    const material={id:'visual-v2',account:'LikeAvto',kind:'visual_context',trust:'source_only',postKey:'native:post',
      mediaSha256:'a'.repeat(64),
      text:'Every decoded frame fast-screened; policy-selected frames visually reviewed. Historical source observations, not verified current offers. Selection is not a guarantee of detecting every transient detail.',
      knowledgeEntryId:'entry-v2',knowledgeVersionId:'version-v2',visualEvidence:evidence};
    const request={account:'likeavto',materials:[material],knowledgeManifest:[{entryId:'entry-v2',versionId:'version-v2',
      kind:'visual_context',trust:'source_only',hash:'h'}]};
    const prepared=prepareAssistantRequest(request);
    assert.equal(prepared.payload.materials[0].visualEvidence.aggregate[0].observation.numbers[0].raw,'от 3 240 000 ₽');
    assert.equal(prepared.input.includes(f.request.frames[0].path),false);
    const unknown=structuredClone(request);
    unknown.materials[0].visualEvidence.aggregate[1].observation.numbers=[{raw:'3 ? ₽',value:null,unit:null,currency:'₽',uncertain:true}];
    unknown.materials[0].visualEvidence.aggregate[1].observation.uncertainties=['Цифры не читаются.'];
    assert.equal(prepareAssistantRequest(unknown).payload.materials[0].visualEvidence.aggregate[1].observation.numbers[0].value,null);
    for(const change of [
      r=>{r.materials[0].visualEvidence.coverage.coveredSelectedFrameCount=1;},
      r=>{r.materials[0].visualEvidence.aggregate[0].sources[0].path='C:/private/frame.png';},
      r=>{r.materials[0].visualEvidence.source.account='BAW Russia';},
      r=>{r.materials[0].visualEvidence.sourcePostVersion='not-a-hash';},
      r=>{r.materials[0].mediaSha256='f'.repeat(64);},
      r=>{r.materials[0].visualEvidence.aggregate[1].sources[0].pixelSha256=r.materials[0].visualEvidence.aggregate[0].sources[0].pixelSha256;},
      r=>{r.materials[0].visualEvidence.aggregate[0].observation.numbers[0].uncertain=true;},
      r=>{r.materials[0].trust='verified';},
      r=>{r.materials[0].visualEvidence.aggregateOverflow=true;}
    ]){const bad=structuredClone(request);change(bad);assert.throws(()=>prepareAssistantRequest(bad),{code:'ASSISTANT_INVALID_REQUEST'});}
  }finally{await f.cleanup();}
});

test('v2 prompt override sends one native PNG with bounded local schema and no tools',async()=>{
  const f=await fixture();try{
    const frame=f.request.frames[0],calls=[];
    const outer={model:'synthetic-vlm:1',done:true,message:{role:'assistant',
      content:JSON.stringify({frames:[none(frame.id)],summary:'Кадр просмотрен.'})}};
    const fake=(url,options,callback)=>{
      const req=new EventEmitter();req.setTimeout=()=>{};req.destroy=error=>req.emit('error',error);
      req.end=body=>queueMicrotask(()=>{calls.push({url:url.pathname,options,body:JSON.parse(body)});
        const res=new EventEmitter();res.statusCode=200;callback(res);
        res.emit('data',Buffer.from(JSON.stringify(outer)));res.emit('end');});return req;
    };
    const result=await runLocalVisionBatch([frame],{endpoint:'http://127.0.0.1:11434',model:'synthetic-vlm:1'},1000,
      {requestFn:fake,instructions:'Русский v2 источник',userContent:'Только frame ID',strictStatusSchema:true,strictNumberShape:true});
    assert.equal(result.frames[0].id,frame.id);
    assert.equal(calls[0].url,'/api/chat');
    assert.deepEqual(calls[0].body.options,{temperature:0,num_ctx:4096,num_predict:1536});
    assert.equal(calls[0].body.messages[0].content,'Русский v2 источник');
    assert.equal(calls[0].body.messages[1].content,'Только frame ID');
    assert.deepEqual(calls[0].body.messages[1].images,[PNG.toString('base64')]);
    assert.equal(calls[0].body.messages[1].tools,undefined);
    const variants=calls[0].body.format.properties.frames.items.anyOf;
    assert.deepEqual(variants.map(v=>v.properties.status.enum),[['none'],['readable'],['unreadable']]);
    const noneVariant=variants[0].properties;
    assert.equal(noneVariant.text.maxItems,0);assert.equal(noneVariant.numbers.maxItems,0);
    assert.equal(noneVariant.uncertainties.maxItems,0);
    const numberProperties=variants[1].properties.numbers.items.properties;
    for(const key of ['raw','value','unit','currency'])assert.equal(numberProperties[key].minLength,1);
    for(const key of ['value','unit','currency'])assert.deepEqual(numberProperties[key].type,['string','null']);
    const diagnostic={frames:[{...none(frame.id),numbers:[{raw:'от',value:null,unit:'от',currency:null,uncertain:true}]}],summary:'Кадр просмотрен'};
    assert.throws(()=>admitMediaVisionBatch(diagnostic,[frame]),/MEDIA_VISION_OUTPUT_INVALID/);
    const uncertain={frames:[{...readable(frame.id),numbers:[{raw:'3 ? ₽',value:null,unit:null,currency:'₽',uncertain:true}],
      uncertainties:['Цифры не читаются.']}],summary:'Кадр просмотрен'};
    assert.equal(admitMediaVisionBatch(uncertain,[frame]).frames[0].numbers[0].value,null);
  }finally{await f.cleanup();}
});

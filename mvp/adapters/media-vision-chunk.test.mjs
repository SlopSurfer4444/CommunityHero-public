import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {EventEmitter} from 'node:events';
import {createHash,randomUUID} from 'node:crypto';
import {chunkManifestDigest,runMediaVisionChunk,stageMediaVisionChunk,validateMediaVisionChunk} from './media-vision-chunk.mjs';
import {prepareAssistantRequest} from './assistant.mjs';
import {admitMediaVisionBatch,runLocalVisionBatch} from './media-vision.mjs';

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
    const run=mode=>runMediaVisionChunk(f.request,{scratchRoot:f.scratch,dataDir:f.scratch,backend:'local',env,
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
    assert.equal(calls,8);assert.equal(checks,7);
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
      {requestFn:fake,instructions:'Русский v2 источник',userContent:'Только frame ID',strictStatusSchema:true});
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
    const diagnostic={frames:[{...none(frame.id),numbers:[{raw:'от',value:null,unit:'от',currency:null,uncertain:true}]}],summary:'Кадр просмотрен'};
    assert.throws(()=>admitMediaVisionBatch(diagnostic,[frame]),/MEDIA_VISION_OUTPUT_INVALID/);
    const uncertain={frames:[{...readable(frame.id),numbers:[{raw:'3 ? ₽',value:null,unit:null,currency:'₽',uncertain:true}],
      uncertainties:['Цифры не читаются.']}],summary:'Кадр просмотрен'};
    assert.equal(admitMediaVisionBatch(uncertain,[frame]).frames[0].numbers[0].value,null);
  }finally{await f.cleanup();}
});

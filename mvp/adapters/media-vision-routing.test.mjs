import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {readVisionRouting,selectVisionRoute,validateVisionRouting,allowsVisionFallback} from './media-vision-routing.mjs';
const source='a'.repeat(64);
const config=()=>({schemaVersion:1,account:'baw-russia',defaultBackend:'codex_luna',automaticFallback:false,cloudMaxSelectedFrames:1000,sourceOverrides:{}});
const request=count=>({account:'baw-russia',inventory:{selectedFrameCount:count},source:{mediaSha256:source}});
test('short videos can use Luna but 2059-frame video stays local, including fallback',()=>{
  assert.deepEqual(selectVisionRoute(config(),request(82)),{primary:'codex_luna',fallback:null});
  assert.deepEqual(selectVisionRoute({...config(),automaticFallback:true},request(2059)),{primary:'local',fallback:null});
});

test('v2 enables bounded chunk repair without changing whole-source route and rejects enlarged budgets',()=>{
  const c={...config(),schemaVersion:2,automaticFallback:true,sourceOverrides:{[source]:'local'},
    boundedLocalFallback:{maxCloudFramesPerChunk:32,maxCloudInvocationsPerChunk:8,maxCloudFramesPerSource:64}};
  assert.deepEqual(validateVisionRouting(c,'baw-russia'),c);
  assert.deepEqual(selectVisionRoute(c,request(2059)),{primary:'local',fallback:null});
  assert.deepEqual(selectVisionRoute({...c,automaticFallback:false},request(2059)),{primary:'local',fallback:null});
  assert.deepEqual(selectVisionRoute({...c,sourceOverrides:{}},request(82)),{primary:'codex_luna',fallback:null},'v2 cannot cycle cloud-local-cloud');
  for(const change of [b=>b.maxCloudFramesPerChunk=33,b=>b.maxCloudInvocationsPerChunk=9,
    b=>b.maxCloudFramesPerSource=65,b=>b.maxCloudFramesPerSource=16,b=>b.unknown=true]){
    const bad=structuredClone(c);change(bad.boundedLocalFallback);
    assert.throws(()=>validateVisionRouting(bad,'baw-russia'),{code:'MEDIA_VISION_ROUTING_INVALID'});
  }
});
test('Sol is an explicit route, including exact source override and configured fallback',()=>{
  const c={...config(),defaultBackend:'codex_sol',automaticFallback:true};
  assert.deepEqual(selectVisionRoute(c,request(82)),{primary:'codex_sol',fallback:'local'});
  assert.deepEqual(selectVisionRoute({...c,defaultBackend:'local'},request(82)),
    {primary:'local',fallback:'codex_luna'});
  c.sourceOverrides[source]='codex_luna';
  assert.deepEqual(selectVisionRoute(c,request(82)),{primary:'codex_luna',fallback:'local'});
  c.sourceOverrides[source]='codex_sol';
  assert.deepEqual(selectVisionRoute(c,request(2059)),{primary:'local',fallback:null});
  const local={...config(),defaultBackend:'local',automaticFallback:true};
  local.defaultBackend='codex_sol';local.sourceOverrides[source]='local';
  assert.deepEqual(selectVisionRoute(local,request(82)),{primary:'local',fallback:null});
});
test('source-local override forbids automatic cloud fallback; fallback is explicit',()=>{
  const c={...config(),defaultBackend:'local',automaticFallback:true};
  assert.deepEqual(selectVisionRoute(c,request(82)),{primary:'local',fallback:'codex_luna'});
  c.sourceOverrides[source]='local';
  assert.deepEqual(selectVisionRoute(c,request(82)),{primary:'local',fallback:null});
});
test('wrong account, unknown fields and malformed budgets fail closed',()=>{
  for(const c of [{...config(),account:'likeavto'},{...config(),cloudMaxSelectedFrames:0},{...config(),defaultBackend:'auto'},{...config(),extra:true}])
    assert.throws(()=>validateVisionRouting(c,'baw-russia'));
});
test('missing config means local; corrupt existing config does not silently reset',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'vision-routing-'));
  try {const c=await readVisionRouting(dir,'baw-russia');assert.equal(c.defaultBackend,'local');assert.equal(c.automaticFallback,false);
    await fs.writeFile(path.join(dir,'vision-routing-baw-russia.json'),'{');
    await assert.rejects(()=>readVisionRouting(dir,'baw-russia'));
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});
test('artifact, identity, staging and policy failures cannot cause failover',()=>{
  for(const code of ['MEDIA_VISION_FRAME_INVALID','MEDIA_VISION_STAGE_FAILED','ACCOUNT_SCOPE_MISMATCH','MEDIA_VISION_ROUTING_INVALID'])assert.equal(allowsVisionFallback({code}),false);
  assert.equal(allowsVisionFallback({code:'MEDIA_VISION_TIMEOUT'}),true);
});

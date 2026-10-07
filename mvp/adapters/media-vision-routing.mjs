import fs from 'node:fs/promises';
import path from 'node:path';
import {accountDefinition} from './config.mjs';

const BACKENDS=new Set(['local','codex_luna','codex_sol']);
const fail=()=>{throw Object.assign(new Error('MEDIA_VISION_ROUTING_INVALID'),{code:'MEDIA_VISION_ROUTING_INVALID'});};
export function validateVisionRouting(value,account){
  accountDefinition(account);
  if(!value||typeof value!=='object'||Array.isArray(value)||
    Object.keys(value).sort().join('|')!==(value.schemaVersion===2?'account|automaticFallback|boundedLocalFallback|cloudMaxSelectedFrames|defaultBackend|schemaVersion|sourceOverrides':'account|automaticFallback|cloudMaxSelectedFrames|defaultBackend|schemaVersion|sourceOverrides')||
    ![1,2].includes(value.schemaVersion)||value.account!==account||!BACKENDS.has(value.defaultBackend)||
    typeof value.automaticFallback!=='boolean'||!Number.isSafeInteger(value.cloudMaxSelectedFrames)||
    value.cloudMaxSelectedFrames<1||value.cloudMaxSelectedFrames>10000||
    !value.sourceOverrides||typeof value.sourceOverrides!=='object'||Array.isArray(value.sourceOverrides)||
    Object.keys(value.sourceOverrides).length>1000)fail();
  for(const [source,backend] of Object.entries(value.sourceOverrides))
    if(!/^[a-f0-9]{64}$/.test(source)||!BACKENDS.has(backend))fail();
  if(value.schemaVersion===2){
    const bounded=value.boundedLocalFallback;
    if(!bounded||Object.keys(bounded).sort().join('|')!=='maxCloudFramesPerChunk|maxCloudFramesPerSource|maxCloudInvocationsPerChunk'||
      !Number.isSafeInteger(bounded.maxCloudFramesPerChunk)||bounded.maxCloudFramesPerChunk<1||bounded.maxCloudFramesPerChunk>32||
      bounded.maxCloudInvocationsPerChunk!==Math.ceil(bounded.maxCloudFramesPerChunk/4)||
      !Number.isSafeInteger(bounded.maxCloudFramesPerSource)||bounded.maxCloudFramesPerSource<bounded.maxCloudFramesPerChunk||
      bounded.maxCloudFramesPerSource>64)fail();
  }
  return structuredClone(value);
}
export function visionRoutingPath(dataDir,account){
  accountDefinition(account);
  if(typeof dataDir!=='string'||!path.isAbsolute(dataDir))fail();
  return path.join(dataDir,`vision-routing-${account}.json`);
}
export async function readVisionRouting(dataDir,account){
  const file=visionRoutingPath(dataDir,account);
  try {
    const info=await fs.lstat(file);
    if(!info.isFile()||info.isSymbolicLink()||info.size>128*1024)fail();
    return validateVisionRouting(JSON.parse(await fs.readFile(file,'utf8')),account);
  } catch(error){
    if(error.code==='ENOENT')return {schemaVersion:1,account,defaultBackend:'local',automaticFallback:false,
      cloudMaxSelectedFrames:1000,sourceOverrides:{}};
    throw error;
  }
}
export function selectVisionRoute(config,request){
  validateVisionRouting(config,request.account);
  const count=request.inventory.selectedFrameCount;
  if(!Number.isSafeInteger(count)||count<1||!/^[a-f0-9]{64}$/.test(request.source.mediaSha256))fail();
  const override=config.sourceOverrides[request.source.mediaSha256];
  const cloudAllowed=count<=config.cloudMaxSelectedFrames&&override!=='local';
  const preferred=override??config.defaultBackend;
  const primary=preferred!=='local'&&!cloudAllowed?'local':preferred;
  // v2 local fallback is bounded by the incomplete chunk and durable source
  // budget. It never changes the source's preferred route or size threshold.
  return {primary,fallback:config.schemaVersion===2?null:
    config.automaticFallback?(primary!=='local'?'local':cloudAllowed?'codex_luna':null):null};
}
// Input/artifact/identity failures must never trigger another backend. A failed
// inference may be repeated only after its process has ended, never concurrently.
export function allowsVisionFallback(error){
  return new Set(['MEDIA_VISION_BACKEND_UNAVAILABLE','MEDIA_VISION_TIMEOUT',
    'MEDIA_VISION_OUTPUT_INVALID','MEDIA_VISION_CODEX_FAILED','MEDIA_VISION_CODEX_UNAVAILABLE',
    'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE','MEDIA_VISION_CODEX_OUTPUT_INVALID']).has(error?.code);
}

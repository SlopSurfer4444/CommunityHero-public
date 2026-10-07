import test from 'node:test';
import assert from 'node:assert/strict';
import {rememberClosedCodexFailure,bindUnusedLocalGpu,mediaGpuFailureProof} from './media-gpu-outcome.mjs';
import {safeError} from './bridge.mjs';
const hash='a'.repeat(64);
test('closed ordinary Codex exit plus no local entry gives resource evidence, same failure envelope',()=>{
  const error=rememberClosedCodexFailure(Object.assign(new Error('private'),{code:'MEDIA_VISION_CODEX_FAILED'}),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:1},stderr:'secret'});
  const before=safeError(error);bindUnusedLocalGpu(error,hash,false);const after=safeError(error);
  assert.equal(after.ok,false);assert.deepEqual(after.error.mediaGpuResource,{version:1,disposition:'unused_local_gpu',child:'closed_normal_exit',requestSha256:hash,exitCode:1});
  delete after.error.mediaGpuResource;assert.deepEqual(after,before);assert.equal(JSON.stringify(safeError(error)).includes('private'),false);
  bindUnusedLocalGpu(error,hash,true);assert.equal(mediaGpuFailureProof(error),undefined);
});
test('plain forged properties, timeouts, kills, signals and native crashes never release',()=>{
  for(const source of [{code:'ADAPTER_TIMEOUT',processExit:{exitCode:1}},{code:'CANCELLED',processExit:{exitCode:1}},
    {code:'MEDIA_VISION_CODEX_FAILED',processExit:{exitCode:1}},{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:1,signal:'SIGTERM'}},
    ...[0,-1,256,3221226505,'1',null].map(exitCode=>({code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode}}))]){
    const error=rememberClosedCodexFailure(Object.assign(new Error(),{code:'MEDIA_VISION_CODEX_FAILED'}),source);
    bindUnusedLocalGpu(error,hash,false);assert.equal(mediaGpuFailureProof(error),undefined);
  }
  const forged={code:'MEDIA_VISION_CODEX_FAILED',mediaGpuResource:{version:1,disposition:'unused_local_gpu',child:'closed_normal_exit',requestSha256:hash,exitCode:1}};
  bindUnusedLocalGpu(forged,hash,false);assert.equal(safeError(forged).error.mediaGpuResource,undefined);
});

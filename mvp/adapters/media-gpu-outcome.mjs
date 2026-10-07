// Resource evidence is issued only by trusted adapter control flow, never by
// error properties, model output or a diagnostic file. It is not retry authority.
const closedCodex=new WeakMap(),unusedLocalGpu=new WeakMap();
export function rememberClosedCodexFailure(mapped,processFailure){
  const exit=processFailure?.processExit;
  if(processFailure?.code==='ADAPTER_PROCESS_FAILED'&&Number.isInteger(exit?.exitCode)&&
    exit.exitCode>=1&&exit.exitCode<=255&&exit.signal===undefined)
    closedCodex.set(mapped,exit.exitCode);
  return mapped;
}
export function bindUnusedLocalGpu(error,requestSha256,localBackendEntered){
  const exitCode=closedCodex.get(error);
  if(localBackendEntered===false&&/^[a-f0-9]{64}$/.test(requestSha256??'')&&exitCode!==undefined)
    unusedLocalGpu.set(error,Object.freeze({version:1,disposition:'unused_local_gpu',
      child:'closed_normal_exit',requestSha256,exitCode}));
  else unusedLocalGpu.delete(error);
}
export function mediaGpuFailureProof(error){return unusedLocalGpu.get(error);}

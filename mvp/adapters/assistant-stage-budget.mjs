import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash,randomUUID} from 'node:crypto';

// Total remains absolute. Validated model events renew only the separate idle
// limit. Neither limit grants retries, checkpoint replay or partial admission.
export const ASSISTANT_STAGE_BUDGET=Object.freeze({timeoutMs:2700000,idleTimeoutMs:900000,maxOutputBytes:2*1024*1024});
export function isAssistantProgressEvent(event){
  if(!event||typeof event!=='object')return false;
  if(['thread.started','turn.started','turn.completed'].includes(event.type))return true;
  return ['item.started','item.updated','item.completed'].includes(event.type)
    &&['web_search','agent_message','reasoning'].includes(event.item?.type);
}
const FAILURE_CODES=new Set(['ADAPTER_TIMEOUT','ADAPTER_OUTPUT_LIMIT','ADAPTER_PROCESS_FAILED','ASSISTANT_FAILED']);
const ITEM_TYPES=new Set(['web_search','agent_message','reasoning']);
export function stageBudgetObservation(prepared,{research=false,now=()=>performance.now()}={}) {
  const started=now(),count=value=>Array.isArray(value)?value.length:0;
  const base={version:1,stage:research?'research':prepared.review?'stronger_review':prepared.triage?'first_pass':'discussion',
    ...ASSISTANT_STAGE_BUDGET,inputSha256:createHash('sha256').update(prepared.input).digest('hex'),inputBytes:Buffer.byteLength(prepared.input),
    itemCount:count(prepared.payload?.items),firstPassAssessmentCount:count(prepared.payload?.firstPass?.assessments),
    firstPassBytes:prepared.payload?.firstPass?Buffer.byteLength(JSON.stringify(prepared.payload.firstPass)):0};
  let eventCount=0,lastEventElapsedMs=null;const completed={web_search:0,agent_message:0,reasoning:0};
  const elapsed=()=>Math.max(0,Math.floor(now()-started));
  return {
    observe(event){eventCount++;lastEventElapsedMs=elapsed();
      if(event?.type==='item.completed'&&ITEM_TYPES.has(event.item?.type))completed[event.item.type]++;},
    snapshot(failure,phase='generation'){
      if(!FAILURE_CODES.has(failure?.code)||!['generation','admission_or_verification'].includes(phase))return null;
      return {...base,phase,errorCode:failure.code,...(['idle','total'].includes(failure.timeoutKind)?{timeoutKind:failure.timeoutKind}:{}),elapsedMs:elapsed(),lastEventElapsedMs,eventCount,
        completedEvents:{...completed},eventScope:'initial_generation_only',at:new Date().toISOString(),diagnosis:'budget_or_process_failure_only',
        rootCause:'unknown',retryAuthorized:false,partialOutputAdmitted:false};
    }
  };
}
export async function persistStageBudgetDiagnostic(laneBase,observation,failure,phase) {
  // Best effort under the already-owned assistant lane; never mask the failure.
  try {
    const diagnostic=observation.snapshot(failure,phase);if(!diagnostic)return false;
    const content=JSON.stringify(diagnostic);if(Buffer.byteLength(content)>4096)return false;
    if((await fs.lstat(laneBase)).isSymbolicLink())return false;
    const directory=path.join(laneBase,'stage-diagnostics');await fs.mkdir(directory,{recursive:true});
    if((await fs.lstat(directory)).isSymbolicLink())return false;
    await fs.writeFile(path.join(directory,`stage-failure-${Date.now()}-${randomUUID()}.json`),content,{flag:'wx',mode:0o600});
    const owned=/^stage-failure-\d{13}-[0-9a-f-]{36}\.json$/;
    const files=(await fs.readdir(directory,{withFileTypes:true})).filter(e=>e.isFile()&&owned.test(e.name)).map(e=>e.name).sort();
    for(const file of files.slice(0,Math.max(0,files.length-32)))await fs.unlink(path.join(directory,file));
    return true;
  }catch{return false;}
}

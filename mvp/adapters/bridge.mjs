import {pathToFileURL} from 'node:url';
import {runProcess} from './process.mjs';
import {accountDefinition,here,reject,resolveAdapterPaths} from './config.mjs';
import path from 'node:path';
import {mediaGpuFailureProof} from './media-gpu-outcome.mjs';
import {bindInvocationWireRequest} from './assistant-invocation-budget.mjs';
import {traceRecorderFromEnv,withTraceRecorder} from '../cli/trace-recorder.mjs';

const nativeCrashSignals=new Set(['SIGABRT','SIGBUS','SIGFPE','SIGILL','SIGSEGV']);
function terminatedNativeCrash(error) {
  if(error?.code!=='ADAPTER_PROCESS_FAILED')return false;
  const exit=error.processExit;
  // runProcess reports this only after child close. Cancellation, timeouts,
  // malformed output and provider rejections are deliberately not retried.
  return Number.isInteger(exit?.exitCode)&&exit.exitCode>=-2147483648&&exit.exitCode<=4294967295&&[0xC0000409,0xC0000005].includes(exit.exitCode>>>0)
    || nativeCrashSignals.has(exit?.signal);
}

export async function dispatch(request,{runProcessFn=runProcess,resolvePaths=resolveAdapterPaths,nowFn=()=>performance.now(),photoAcquireOptions,ownerSessionInspect}={}) {
  if (!request || Array.isArray(request) || typeof request !== 'object') reject('INVALID_REQUEST');
  const op = request.op ?? request.operation;
  if (!['caps','scan','read','context','head','status','auth_status','owner_session_inspect','execute','readback','assistant','assistant_preflight','assistant_research','materials','media_source','media_vision','media_vision_chunk','photo_acquire_only'].includes(op)) reject('UNSUPPORTED_OPERATION');
  accountDefinition(request.account);
  if (op === 'owner_session_inspect') {
    if (Object.keys(request).some(key=>!['account','operation','op'].includes(key))) reject('INVALID_REQUEST');
    const inspect=ownerSessionInspect??(await import('../connectors/angryspace-provider/src/transport/owner-session-handoff.ts')).readEnvConfiguredOwnerSessionStatus;
    const result=await inspect();
    if (result?.account!==request.account||result?.socialRequests!==0||result?.sendGateOpen!==false||result?.retryOriginalOperation!==false)reject('INVALID_REQUEST');
    return result;
  }
  if (op === 'photo_acquire_only') return (await import('./photo-acquisition.mjs')).runPhotoAcquisition(request,photoAcquireOptions);
  if (op === 'assistant_preflight') return (await import('./assistant-preflight.mjs')).assistantPreflight(request);
  if (op === 'assistant') return (await import('./assistant.mjs')).runAssistant(request);
  if (op === 'assistant_research') return (await import('./assistant.mjs')).runAssistantResearch(request);
  if (op === 'media_source') return (await import('./media-source.mjs')).runMediaSource(request);
  if (op === 'media_vision') return (await import('./media-vision.mjs')).runMediaVision(request);
  if (op === 'media_vision_chunk') return (await import('./media-vision-chunk.mjs')).runMediaVisionChunk(request);
  if (op === 'materials' || op === 'media') {const mod = await import('./materials.mjs'); return op === 'materials' ? mod.runMaterials(request) : mod.runMedia(request);}
  const paths=resolvePaths(request.account);
  const env={...process.env,COMMUNITYHERO_CONVEYOR_ROOT:paths.conveyorRepo,COMMUNITYHERO_PROVIDER_ROOT:paths.providerRepo,COMMUNITYHERO_PROVIDER_NODE:paths.providerNode};
  const input=JSON.stringify({...request,op}),deadline=nowFn()+180000;
  let stdout,firstFailure;
  for(let attempt=1;attempt<=2;attempt++) {
    try {
      ({stdout} = await runProcessFn(paths.providerNode, ['--experimental-strip-types',path.join(here,'provider.mjs')], {input,cwd:paths.providerRepo,env,timeoutMs:Math.max(1,Math.floor(deadline-nowFn())),maxOutputBytes:12*1024*1024,processRole:'provider-process'}));
      break;
    } catch(error) {
      // This is the operation whose child failed, not a claim about an earlier
      // mutation. A second read is safe; execute is never replayed here.
      if(error&&typeof error==='object')error.adapterOperation=op;
      if(op==='readback'&&attempt===1&&terminatedNativeCrash(error)&&deadline-nowFn()>=1000) {
        firstFailure=safeError(error).error;
        continue;
      }
      if(firstFailure&&error&&typeof error==='object')error.readbackProcessRecovery={attempts:2,firstFailure};
      throw error;
    }
  }
  try {
    const envelope=JSON.parse(stdout);
    if (!envelope.ok) {
      const diagnostic=safeError(envelope.error??{code:'PROVIDER_UNAVAILABLE'}).error;
      throw Object.assign(new Error(diagnostic.code),diagnostic);
    }
    return firstFailure?{...envelope.result,readbackProcessRecovery:{attempts:2,firstFailure}}:envelope.result;
  } catch(error) {
    if(error&&typeof error==='object') {
      error.adapterOperation=op;
      if(firstFailure)error.readbackProcessRecovery={attempts:2,firstFailure};
    }
    throw error;
  }
}

/** Native transport metadata is outside the business request and result. */
export async function dispatchEnvelope(request,{recorder=traceRecorderFromEnv(),dispatchFn=dispatch,maxResponseBytes=32*1024*1024,...options}={}) {
  if(recorder?.context.companyKey!==request?.account)recorder=null;
  return withTraceRecorder(recorder,async()=>{
    const span=recorder?.start('provider.request',{spanClass:'container'});
    let business;
    try{business={ok:true,result:await (span?span.scope(()=>dispatchFn(request,options)):dispatchFn(request,options))};span?.finish();}
    catch(error){span?.finish({outcome:'failed'});business=safeError(error);}
    const telemetry=recorder?.finish();
    if(!telemetry)return business;
    // Native uses a 32 MiB short-child output budget. If only observation
    // exceeds it, retain the exact successful/error business envelope.
    const observed={...business,telemetry};
    const limit=Number.isSafeInteger(maxResponseBytes)&&maxResponseBytes>0&&maxResponseBytes<=32*1024*1024?maxResponseBytes:32*1024*1024;
    return Buffer.byteLength(JSON.stringify(observed))<=limit?observed:business;
  });
}

export async function stdinRequest() {
  let size=0;const chunks=[];
  for await(const chunk of process.stdin){size+=chunk.length;if(size>8*1024*1024)reject('INPUT_LIMIT');chunks.push(chunk);}
  const wire=Buffer.concat(chunks);
  const request=JSON.parse(wire.toString('utf8'));
  if(['assistant','assistant_research'].includes(request?.op??request?.operation))bindInvocationWireRequest(request,wire);
  if((request?.op??request?.operation)!=='assistant_preflight'&&size>2*1024*1024)reject('INPUT_LIMIT');
  return request;
}
const researchCategories=new Set(['UNOBSERVED_URL','MISSING_EVIDENCE','RECIPIENT','FIELDS','UNATTRIBUTED_REPLY','ACTIVITY_ID']);
const responseCategories=new Set(['OUTPUT_JSON','TOOL_ARGUMENTS','TOOL_CALL','TOOL_REQUEST','LOOKUP','PROPOSAL','TRIAGE','ACTION_REVIEW','CORE_FIELDS']);
const requestCategories=new Set(['MEDIA_BINDING','LEGACY_POST_BINDING']);
const transportStages=new Set(['read-fetch','read-json','read-auth','read-http',
  ...['proactive','after-401'].flatMap(phase=>['token-state','credential-helper-startup','credential-helper-timeout-before-connect','credential-helper-timeout-after-connect','credential-helper-transport','credential-helper-response-invalid','credential-helper-failed','credential-missing','credential-value-invalid','token-envelope-invalid','token-freshness-boundary','token-lifecycle-blocked','refresh-lock','refresh-lock-acquire','refresh-lock-timeout','refresh-lock-release','oauth-client','refresh-fetch','refresh-http','refresh-json','refresh-schema','token-rotate'].map(source=>`read-auth-${phase}-${source}`))]);
const transportCauses=new Set(['dns','tcp','tls','connect_timeout','timeout','abort','unknown']);
// Re-project the connector's optional closed OAuth evidence at the process
// boundary. No arbitrary body, message, token fingerprint or header is retained.
const oauthCodes={expired:'expired',token_expired:'expired',expired_token:'expired',refresh_token_expired:'expired',session_expired:'expired',
  revoked:'revoked',token_revoked:'revoked',refresh_token_revoked:'revoked',reuse:'reuse',token_reuse:'reuse',refresh_token_reused:'reuse',refresh_token_reuse:'reuse',reuse_detected:'reuse',
  invalid_grant:'invalid_grant',invalid_client:'client',unauthorized_client:'client',captcha:'captcha',captcha_required:'captcha',invalid_captcha:'captcha',code_required:'code_required',mfa_required:'code_required',
  invalid_request:'unrecognized',unsupported_grant_type:'unrecognized',invalid_scope:'unrecognized',access_denied:'unrecognized',server_error:'unrecognized',temporarily_unavailable:'unrecognized'};
function safeOAuthDiagnostic(value){
  try{
    if(!value||typeof value!=='object'||Array.isArray(value)||value.version!==1||!['unavailable','oversized','non_json','json_object','json_other'].includes(value.responseShape))return undefined;
    const code=v=>typeof v==='string'&&Object.hasOwn(oauthCodes,v)?v:undefined;
    const error=code(value.error),errorType=code(value.errorType);
    const trigger=['proactive','after-401'].includes(value.trigger)?value.trigger:undefined;
    const generation=Number.isSafeInteger(value.generation)&&value.generation>=0?value.generation:undefined;
    const observedAt=typeof value.observedAt==='string'&&/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value.observedAt)
      &&new Date(value.observedAt).toISOString()===value.observedAt?value.observedAt:undefined;
    return {version:1,responseShape:value.responseShape,classification:errorType?oauthCodes[errorType]:error?oauthCodes[error]:'unrecognized',
      ...(error===undefined?{}:{error}),...(errorType===undefined?{}:{errorType}),...(trigger===undefined?{}:{trigger}),
      ...(generation===undefined?{}:{generation}),...(observedAt===undefined?{}:{observedAt})};
  }catch{return undefined;}
}

// Keep this runtime-free mirror aligned with the connector sanitizer and native
// parser. Additional keys are dropped, and inconsistent facts are rejected.
export function safeAuthExchangeDiagnostic(value) {
  try {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
    const d=Object.fromEntries(['version','diagnosticId','observation','stage','cause','watchdogFired','elapsedMs','deadlineMs','responseReceived','httpStatusValid','httpStatus','responseDisposal','trigger','generation'].map(key=>[key,Reflect.get(value,key)]));
    if (d.version!==1 || typeof d.diagnosticId!=='string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(d.diagnosticId)
      || !['originating_exchange','durable_barrier'].includes(d.observation)
      || !['fetch','http','json','schema'].includes(d.stage)
      || !['dns','tcp','tls','connect_timeout','timeout','abort','unknown','http_rejection','json_invalid','schema_invalid','invalid_status'].includes(d.cause)
      || typeof d.watchdogFired!=='boolean' || typeof d.elapsedMs!=='number' || !Number.isFinite(d.elapsedMs) || d.elapsedMs<0 || d.elapsedMs>Number.MAX_SAFE_INTEGER
      || !Number.isSafeInteger(d.deadlineMs) || d.deadlineMs<1 || d.deadlineMs>60000
      || typeof d.responseReceived!=='boolean' || typeof d.httpStatusValid!=='boolean'
      || !['not_requested','requested','unavailable','failed'].includes(d.responseDisposal)) return undefined;
    if (d.httpStatusValid ? !d.responseReceived || !Number.isInteger(d.httpStatus) || d.httpStatus<100 || d.httpStatus>599 : d.httpStatus!==undefined) return undefined;
    if (!d.responseReceived && d.responseDisposal!=='not_requested') return undefined;
    if (d.trigger!==undefined && !['proactive','after-401'].includes(d.trigger)) return undefined;
    if (d.generation!==undefined && (!Number.isSafeInteger(d.generation) || d.generation<0)) return undefined;
    return {version:1,diagnosticId:d.diagnosticId,observation:d.observation,stage:d.stage,cause:d.cause,
      watchdogFired:d.watchdogFired,elapsedMs:d.elapsedMs,deadlineMs:d.deadlineMs,responseReceived:d.responseReceived,
      httpStatusValid:d.httpStatusValid,...(d.httpStatusValid?{httpStatus:d.httpStatus}:{}),responseDisposal:d.responseDisposal,
      ...(d.trigger===undefined?{}:{trigger:d.trigger}),...(d.generation===undefined?{}:{generation:d.generation})};
  } catch {return undefined;}
}

function safeConnectionState(value) {
  try {
    const status=value?.status,reason=value?.reason;
    return ['recoverable','needs_user'].includes(status)&&['auth_required','credential_missing','credential_invalid','challenge','scope_mismatch','recovery_uncertain'].includes(reason)
      ?{status,reason}:undefined;
  } catch {return undefined;}
}
export function safeError(error) {
  let code=typeof error?.code==='string' && /^[A-Z0-9_]{1,80}$/.test(error.code)?error.code:'ADAPTER_UNAVAILABLE';
  // Only fixed validator categories cross the process boundary, never diagnostic text or source URLs.
  if(code==='ASSISTANT_INVALID_RESEARCH' || code.startsWith('ASSISTANT_INVALID_RESEARCH_')) {
    const category=code==='ASSISTANT_INVALID_RESEARCH'?error.researchCategory:code.slice('ASSISTANT_INVALID_RESEARCH_'.length);
    code='ASSISTANT_INVALID_RESEARCH'+(researchCategories.has(category)?`_${category}`:'');
  }
  if(code==='ASSISTANT_INVALID_RESPONSE' || code.startsWith('ASSISTANT_INVALID_RESPONSE_')) {
    const category=code==='ASSISTANT_INVALID_RESPONSE'?error.validationCategory:code.slice('ASSISTANT_INVALID_RESPONSE_'.length);
    code='ASSISTANT_INVALID_RESPONSE'+(responseCategories.has(category)?`_${category}`:'');
  }
  if(code==='ASSISTANT_INVALID_REQUEST' || code.startsWith('ASSISTANT_INVALID_REQUEST_')) {
    const category=code==='ASSISTANT_INVALID_REQUEST'?error.requestCategory:code.slice('ASSISTANT_INVALID_REQUEST_'.length);
    code='ASSISTANT_INVALID_REQUEST'+(requestCategories.has(category)?`_${category}`:'');
  }
  // Retain only OS exit facts. Child stderr/stdout may contain credentials or
  // comment text and must never cross the safe error boundary.
  const exit=error?.processExit;
  const processExit=exit&&typeof exit==='object'?{
    ...(Number.isInteger(exit.exitCode)&&exit.exitCode>=-2147483648&&exit.exitCode<=4294967295?{exitCode:exit.exitCode}:{}),
    ...(['SIGABRT','SIGBUS','SIGFPE','SIGHUP','SIGILL','SIGINT','SIGKILL','SIGPIPE','SIGQUIT','SIGSEGV','SIGTERM'].includes(exit.signal)?{signal:exit.signal}:{})
  }:{};
  const adapterOperation=['caps','scan','read','context','head','status','auth_status','execute','readback'].includes(error?.adapterOperation)?error.adapterOperation:undefined;
  const recovery=error?.readbackProcessRecovery;
  const readbackProcessRecovery=adapterOperation==='readback'&&recovery?.attempts===2
    ?{attempts:2,firstFailure:safeError({code:recovery.firstFailure?.code,processExit:recovery.firstFailure?.processExit,adapterOperation:'readback'}).error}:undefined;
  const httpStatus=code==='HTTP_ERROR'&&Number.isInteger(error?.httpStatus)&&error.httpStatus>=100&&error.httpStatus<=599?error.httpStatus:undefined;
  // Provider errors carry their fixed inner stage in operation; the outer
  // bridge operation is separate. Re-project the closed slot at both boundaries.
  const transportStage=['HTTP_ERROR','TRANSPORT_ERROR'].includes(code)
    ?[error?.transportStage,error?.operation].find(stage=>transportStages.has(stage)):undefined;
  const transportCause=code==='TRANSPORT_ERROR'&&transportStage==='read-fetch'&&transportCauses.has(error?.transportCause)
    ?error.transportCause:undefined;
  const processRole=['provider-process','provider-worker','credential-helper','legacy-process-guard'].includes(error?.processRole)?error.processRole:undefined;
  const processId=processRole&&Number.isSafeInteger(error?.processId)&&error.processId>0?error.processId:undefined;
  const oauthDiagnostic=code==='HTTP_ERROR'&&/^read-auth-(?:proactive|after-401)-refresh-http$/.test(transportStage??'')?safeOAuthDiagnostic(error?.oauthDiagnostic):undefined;
  const authExchangeDiagnostic=['HTTP_ERROR','TRANSPORT_ERROR'].includes(code)&&/^read-auth-(?:proactive|after-401)-(?:refresh-(?:fetch|http|json|schema)|token-lifecycle-blocked)$/.test(transportStage??'')?safeAuthExchangeDiagnostic(error?.authExchangeDiagnostic):undefined;
  const mediaGpuResource=mediaGpuFailureProof(error);
  const connectionState=transportStage?.startsWith('read-auth-')?safeConnectionState(error?.connectionState):undefined;
  return {ok:false,error:{code,message:code,...(mediaGpuResource?{mediaGpuResource}:{}),...(httpStatus!==undefined?{httpStatus}:{}),...(transportStage?{transportStage}:{}),...(transportCause?{transportCause}:{}),...(oauthDiagnostic?{oauthDiagnostic}:{}),...(authExchangeDiagnostic?{authExchangeDiagnostic}:{}),...(connectionState?{connectionState}:{}),...(Object.keys(processExit).length?{processExit}:{}),...(processRole?{processRole}:{}),...(processId?{processId}:{}),...(adapterOperation?{adapterOperation}:{}),...(readbackProcessRecovery?{readbackProcessRecovery}:{})}};
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {process.stdout.write(JSON.stringify(await dispatchEnvelope(await stdinRequest())));}
  catch(error) {process.stdout.write(JSON.stringify(safeError(error)));}
}

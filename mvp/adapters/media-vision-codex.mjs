import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {runProcess} from './process.mjs';
import {rememberClosedCodexFailure} from './media-gpu-outcome.mjs';
import {copyAssistantLogin,secureAssistantHome} from './assistant.mjs';
import {admitMediaVisionBatch,batchSchema,mediaVisionOutputDiagnostic} from './media-vision.mjs';
import {CODEX_MODEL,CODEX_CLI_SHA256,assistantCatalogForRun} from './codex-model-policy.mjs';

export const CODEX_VISION_MODEL=CODEX_MODEL;
export const CODEX_VISION_EFFORT='low';
// Legacy configuration aliases preserve their frame budgets, not their models.
export const CODEX_VISION_MODELS=Object.freeze({codex_luna:CODEX_MODEL,codex_sol:CODEX_MODEL});
const MODEL_BATCH_LIMITS=Object.freeze({[CODEX_MODEL]:32});
const VERIFIED_CLI_SHA256=new Set([CODEX_CLI_SHA256]);
const SHA=/^[a-f0-9]{64}$/;
const MAX_CATALOG_BYTES=2*1024*1024, MAX_CATALOG_AGE_MS=24*60*60*1000;
const DISABLED=['shell_tool','unified_exec','apps','plugins','remote_plugin','hooks','multi_agent',
  'multi_agent_v2','code_mode','code_mode_host','code_mode_only','computer_use','browser_use',
  'browser_use_external','in_app_browser','view_image','image_generation','memories','skill_search',
  'goals','sleep_tool','workspace_dependencies','tool_suggest'];
const CATALOG_OVERRIDES={tool_mode:null,use_responses_lite:false,multi_agent_version:null,
  experimental_supported_tools:[],apply_patch_tool_type:null,supports_experimental_context:false};

function fail(code){throw Object.assign(new Error(code),{code});}
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
function batchLimit(model){
  if(!Object.hasOwn(MODEL_BATCH_LIMITS,model))fail('MEDIA_VISION_CODEX_MODEL_UNAVAILABLE');
  return MODEL_BATCH_LIMITS[model];
}
function rethrowProcessFailure(error){
  if(error?.code==='ADAPTER_TIMEOUT')fail('MEDIA_VISION_TIMEOUT');
  if(error?.code==='ADAPTER_PROCESS_UNAVAILABLE')fail('MEDIA_VISION_CODEX_UNAVAILABLE');
  if(error?.code==='ADAPTER_PROCESS_FAILED')throw rememberClosedCodexFailure(
    Object.assign(new Error('MEDIA_VISION_CODEX_FAILED'),{code:'MEDIA_VISION_CODEX_FAILED'}),error);
  throw error;
}
function isolatedEnv(home,env){
  const child={};
  for(const key of ['SystemRoot','WINDIR','TEMP','TMP'])if(env[key])child[key]=env[key];
  return {...child,CODEX_HOME:home,HOME:home,USERPROFILE:home};
}

export function codexVisionCatalog(catalog,model=CODEX_VISION_MODEL){
  batchLimit(model);
  const matches=catalog?.models?.filter(item=>item?.slug===model);
  if(!Array.isArray(matches)||matches.length!==1||!Array.isArray(matches[0].input_modalities)
    ||!matches[0].input_modalities.includes('image')||!matches[0].input_modalities.includes('text')
    ||!Array.isArray(matches[0].supported_reasoning_levels)
    ||!matches[0].supported_reasoning_levels.some(level=>level?.effort===CODEX_VISION_EFFORT))
    fail('MEDIA_VISION_CODEX_MODEL_UNAVAILABLE');
  return {models:[{...matches[0],...CATALOG_OVERRIDES}]};
}

export async function readCodexVisionCatalogCache(sourceHome,{nowMs=Date.now(),readFileFn=fs.readFile,
  model=CODEX_VISION_MODEL}={}){
  batchLimit(model);
  const file=path.join(sourceHome,'models_cache.json');
  let before,after,real,bytes;
  try{
    before=await fs.lstat(file);
    if(!before.isFile()||before.isSymbolicLink()||before.size<2||before.size>MAX_CATALOG_BYTES)
      fail('MEDIA_VISION_CODEX_CATALOG_INVALID');
    real=await fs.realpath(file);
    if(path.resolve(real).toLowerCase()!==path.resolve(file).toLowerCase())
      fail('MEDIA_VISION_CODEX_CATALOG_INVALID');
    bytes=await readFileFn(file);after=await fs.lstat(file);
    if(bytes.length!==before.size||after.size!==before.size||after.mtimeMs!==before.mtimeMs||
      after.ino!==before.ino||after.dev!==before.dev)fail('MEDIA_VISION_CODEX_CATALOG_INVALID');
  }catch(error){if(error?.code?.startsWith('MEDIA_VISION_CODEX_'))throw error;fail('MEDIA_VISION_CODEX_CATALOG_UNAVAILABLE');}
  let parsed;
  try{parsed=JSON.parse(bytes.toString('utf8'));}catch{fail('MEDIA_VISION_CODEX_CATALOG_INVALID');}
  const fetchedAt=parsed?.fetched_at;
  if(typeof fetchedAt!=='string'||!/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?Z$/.test(fetchedAt))
    fail('MEDIA_VISION_CODEX_CATALOG_STALE');
  const fetched=Date.parse(fetchedAt);
  if(!Number.isFinite(fetched)||!Number.isFinite(nowMs)||fetched>nowMs+30_000||nowMs-fetched>MAX_CATALOG_AGE_MS)
    fail('MEDIA_VISION_CODEX_CATALOG_STALE');
  return {catalog:codexVisionCatalog(parsed,model),sha256:digest(bytes),fetchedAt};
}

export function codexVisionArgs(home,imagePaths,model=CODEX_VISION_MODEL){
  batchLimit(model);
  return ['exec','--ignore-user-config','--ignore-rules','--ephemeral','--skip-git-repo-check',
    '--sandbox','read-only','-C',home,'-m',model,'--json','--color','never',
    '--output-schema',path.join(home,'response.schema.json'),
    '--output-last-message',path.join(home,'response.json'),
    '-c','approval_policy="never"','-c','web_search="disabled"',
    '-c',`model_reasoning_effort="${CODEX_VISION_EFFORT}"`,
    '-c','features.skip_host_skill_discovery=true','-c','suppress_unstable_features_warning=true',
    '-c','project_doc_max_bytes=0',
    '-c',`model_instructions_file=${JSON.stringify(path.join(home,'instructions.txt'))}`,
    ...DISABLED.flatMap(name=>['--disable',name]),
    '-c','tools.experimental_request_user_input.enabled=false',
    '-c',`model_catalog_json=${JSON.stringify(path.join(home,'models.json'))}`,
    ...imagePaths.flatMap(file=>['-i',file]),'-'];
}

function eventFailureCategory(message){
  if(typeof message!=='string')return 'unspecified';
  if(/rate.?limit|quota|usage.?limit/i.test(message))return 'rate_limit';
  if(/schema|json.?schema|response.?format/i.test(message))return 'schema';
  if(/context.?window|too.?many.?tokens|maximum.?context/i.test(message))return 'context';
  if(/auth|unauthorized|forbidden|credential/i.test(message))return 'authentication';
  if(/network|connect|timeout|temporarily.?unavailable/i.test(message))return 'connection';
  return 'other';
}
function safeFailureExcerpt(message,category){
  if(typeof message!=='string'||category!=='connection'||
    !message.startsWith('Falling back from WebSockets to HTTPS transport.'))return null;
  const first=message.split(/[\r\n{\[]/,1)[0].slice(0,600);
  return first.replace(/https?:\/\/\S+/gi,'[url]')
    .replace(/[A-Za-z]:[\\/][^\s"']+/g,'[path]')
    .replace(/\b[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}\b/g,'[email]')
    .replace(/\b(?:Bearer\s+)?[A-Za-z0-9_-]{32,}\b/g,'[token]')
    .replace(/(['"`])(?:\\.|(?!\1).)*?\1/g,'[quoted]')
    .replace(/\s+/g,' ').trim().slice(0,240)||null;
}
const HTTPS_TRANSPORT_FALLBACK='Falling back from WebSockets to HTTPS transport. stream disconnected before completion: websocket closed by server before response.completed';
export function admitCodexVisionEvents(stdout,diagnostic){
  for(const line of stdout.split(/\r?\n/).filter(Boolean)){
    let event;
    try{event=JSON.parse(line);}catch{fail('MEDIA_VISION_CODEX_EVENT_INVALID');}
    const item=event?.item;
    const transportFallback=item?.type==='error'&&event?.type!=='turn.failed'&&
      item.message===HTTPS_TRANSPORT_FALLBACK;
    if(diagnostic&&typeof diagnostic==='object'){
      diagnostic.eventCount=(diagnostic.eventCount??0)+1;
      if(transportFallback)diagnostic.transportFallbacks=(diagnostic.transportFallbacks??0)+1;
      if(!transportFallback&&(event?.type==='turn.failed'||item?.type==='error')){
        const message=event?.error?.message??item?.message;
        const category=eventFailureCategory(message);
        diagnostic.eventFailure={kind:event?.type==='turn.failed'?'turn_failed':'item_error',
          category,messageSha256:typeof message==='string'?digest(Buffer.from(message,'utf8')):null,
          messageExcerpt:safeFailureExcerpt(message,category)};
      }
    }
    if(item&&!['agent_message','reasoning','error'].includes(item.type))
      fail('MEDIA_VISION_CODEX_ISOLATION_FAILED');
    // The CLI reports this one transport handoff as item.error and continues
    // over HTTPS. A later terminal failure or nonzero process exit still fails.
    if(transportFallback)continue;
    if(event?.type==='turn.failed'||item?.type==='error')fail('MEDIA_VISION_CODEX_FAILED');
  }
}

// The caller owns the existing media_vision lane lock. This function owns only
// its private per-invocation child and never acquires a second lane or worker.
export async function runCodexVisionBatch(batch,timeoutMs,{instructions,userContent,home,env=process.env,
  runProcessFn=runProcess,secureHomeFn=secureAssistantHome,copyLoginFn=copyAssistantLogin,
  catalogDiagnostic,diagnostic,model=CODEX_VISION_MODEL,
  verifyCliFn=async cliPath=>{
    const binary=await fs.readFile(cliPath);
    return VERIFIED_CLI_SHA256.has(digest(binary));
  }}={}){
  const maxBatch=batchLimit(model);
  if(!Array.isArray(batch)||batch.length<1||batch.length>maxBatch||
    !Number.isSafeInteger(timeoutMs)||timeoutMs<1000||timeoutMs>240_000||
    typeof home!=='string'||!path.isAbsolute(home)||
    typeof instructions!=='string'||!instructions.trim()||
    typeof userContent!=='string'||!userContent.trim())fail('MEDIA_VISION_CODEX_REQUEST_INVALID');
  const ids=new Set();
  for(const frame of batch){
    if(typeof frame?.id!=='string'||!/^frame-[0-9]{12,16}$/.test(frame.id)||ids.has(frame.id)||
      typeof frame.path!=='string'||!path.isAbsolute(frame.path)||!SHA.test(frame.sha256)||
      !Number.isSafeInteger(frame.timestampMs)||frame.timestampMs<0)
      fail('MEDIA_VISION_CODEX_REQUEST_INVALID');
    ids.add(frame.id);
  }
  const parent=await fs.lstat(home).catch(()=>null);
  if(!parent?.isDirectory()||parent.isSymbolicLink())fail('MEDIA_VISION_CODEX_HOME_INVALID');
  if(process.platform!=='win32')fail('MEDIA_VISION_CODEX_UNAVAILABLE');
  const cli=env.COMMUNITYHERO_CODEX_CLI??'C:/AIDev/DevTools/bin/codex.exe';
  if(!path.isAbsolute(cli))fail('MEDIA_VISION_CODEX_UNAVAILABLE');
  let verified=false;
  try{verified=await verifyCliFn(cli);}catch{}
  if(verified!==true)fail('MEDIA_VISION_CODEX_UNAVAILABLE');
  const child=await fs.mkdtemp(path.join(home,'codex-vision-'));
  try{
    await secureHomeFn(child);
    const sourceHome=env.CODEX_HOME||path.join(os.homedir(),'.codex');
    await copyLoginFn(sourceHome,child);
    const childEnv=isolatedEnv(child,env);
    let bundled;
    try{
      bundled=await runProcessFn(cli,['debug','models','--bundled'],
        {cwd:child,env:childEnv,timeoutMs:10_000,maxOutputBytes:2*1024*1024});
    }catch(error){if(diagnostic)diagnostic.catalogProcessError=error?.code??'unknown';rethrowProcessFailure(error);}
    let parsed;
    try{parsed=JSON.parse(bundled.stdout);}catch{fail('MEDIA_VISION_CODEX_CATALOG_INVALID');}
    let catalog,source,sourceSha256;
    if(parsed?.models?.some(item=>item?.slug===model)){
      catalog=codexVisionCatalog(parsed,model);source='bundled';sourceSha256=digest(Buffer.from(bundled.stdout,'utf8'));
    }else{
      let cached;
      try {
        cached=await readCodexVisionCatalogCache(sourceHome,{model});source='authenticated_cache';
      } catch {
        try {
          const refreshed=await assistantCatalogForRun({sourceHome,home:child,cacheHome:path.join(home,'model-catalog'),
            cli,env:childEnv,runProcessFn});
          cached={catalog:codexVisionCatalog(refreshed.catalog,model),sha256:refreshed.sha256};source='authenticated_refresh';
        } catch(error) {
          if(diagnostic)diagnostic.catalogProcessError=error?.code??'unknown';
          if(error?.code?.startsWith('ADAPTER_'))rethrowProcessFailure(error);
          fail('MEDIA_VISION_CODEX_CATALOG_UNAVAILABLE');
        }
      }
      catalog=cached.catalog;sourceSha256=cached.sha256;
    }
    if(catalogDiagnostic&&typeof catalogDiagnostic==='object'){
      catalogDiagnostic.source=source;catalogDiagnostic.sha256=sourceSha256;
      catalogDiagnostic.model=model;catalogDiagnostic.effort=CODEX_VISION_EFFORT;
    }
    await fs.writeFile(path.join(child,'models.json'),JSON.stringify(catalog),{mode:0o600});
    await fs.writeFile(path.join(child,'instructions.txt'),instructions,{mode:0o600});
    await fs.writeFile(path.join(child,'response.schema.json'),
      JSON.stringify(batchSchema([...ids],{strictStatusSchema:true})),{mode:0o600});
    const imagePaths=[];
    for(const [index,frame] of batch.entries()){
      const bytes=await fs.readFile(frame.path);
      if(digest(bytes)!==frame.sha256)fail('MEDIA_VISION_CODEX_FRAME_CHANGED');
      const target=path.join(child,`frame-${index}-${frame.id}.png`);
      await fs.writeFile(target,bytes,{flag:'wx',mode:0o600});
      imagePaths.push(target);
    }
    const mapping=batch.map((frame,index)=>`${index+1}: ${frame.id} at ${frame.timestampMs} ms`).join('\n');
    const input=`Inspect exactly these attached images in this order. Return one frame result for each exact ID; never infer another frame.\n${mapping}\n${userContent}`;
    let pending='';
    let result;
    try {result=await runProcessFn(cli,codexVisionArgs(child,imagePaths,model),{input,cwd:child,env:childEnv,
      timeoutMs,maxOutputBytes:2*1024*1024,onStdout:chunk=>{
        pending+=chunk;
        let at;
        while((at=pending.indexOf('\n'))>=0){
          const line=pending.slice(0,at);pending=pending.slice(at+1);
          if(line.trim())admitCodexVisionEvents(line,diagnostic);
        }
      }});
    } catch(error){
      if(diagnostic&&typeof diagnostic==='object'){
        diagnostic.inferenceProcessError=error?.code??'unknown';
        try{
          const failedRaw=JSON.parse(await fs.readFile(path.join(child,'response.json'),'utf8'));
          diagnostic.output=mediaVisionOutputDiagnostic(failedRaw,batch);
        }catch{diagnostic.responseFile='missing_or_invalid_json';}
      }
      rethrowProcessFailure(error);
    }
    admitCodexVisionEvents(result.stdout);
    let raw;
    try{raw=JSON.parse(await fs.readFile(path.join(child,'response.json'),'utf8'));}
    catch{if(diagnostic)diagnostic.responseFile='missing_or_invalid_json';fail('MEDIA_VISION_CODEX_OUTPUT_INVALID');}
    if(diagnostic)diagnostic.output=mediaVisionOutputDiagnostic(raw,batch);
    admitMediaVisionBatch(raw,batch);
    return raw;
  }finally{
    await fs.rm(child,{recursive:true,force:true}).catch(()=>{});
  }
}

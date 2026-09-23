import {pathToFileURL} from 'node:url';
import {runProcess} from './process.mjs';
import {accountDefinition,here,reject,resolveAdapterPaths} from './config.mjs';
import path from 'node:path';

export async function dispatch(request,{runProcessFn=runProcess,resolvePaths=resolveAdapterPaths}={}) {
  if (!request || Array.isArray(request) || typeof request !== 'object') reject('INVALID_REQUEST');
  const op = request.op ?? request.operation;
  if (!['caps','scan','read','context','head','status','execute','readback','assistant','assistant_research','materials','media_source','media_vision','media_vision_chunk'].includes(op)) reject('UNSUPPORTED_OPERATION');
  accountDefinition(request.account);
  if (op === 'assistant') return (await import('./assistant.mjs')).runAssistant(request);
  if (op === 'assistant_research') return (await import('./assistant.mjs')).runAssistantResearch(request);
  if (op === 'media_source') return (await import('./media-source.mjs')).runMediaSource(request);
  if (op === 'media_vision') return (await import('./media-vision.mjs')).runMediaVision(request);
  if (op === 'media_vision_chunk') return (await import('./media-vision-chunk.mjs')).runMediaVisionChunk(request);
  if (op === 'materials' || op === 'media') {const mod = await import('./materials.mjs'); return op === 'materials' ? mod.runMaterials(request) : mod.runMedia(request);}
  const paths=resolvePaths(request.account);
  const env={...process.env,COMMUNITYHERO_CONVEYOR_ROOT:paths.conveyorRepo,COMMUNITYHERO_PROVIDER_ROOT:paths.providerRepo,COMMUNITYHERO_PROVIDER_NODE:paths.providerNode};
  const {stdout} = await runProcessFn(paths.providerNode, ['--experimental-strip-types',path.join(here,'provider.mjs')], {input:JSON.stringify({...request,op}),cwd:paths.providerRepo,env,timeoutMs:180000,maxOutputBytes:12*1024*1024});
  const envelope=JSON.parse(stdout);
  if (!envelope.ok) reject(envelope.error?.code || 'PROVIDER_UNAVAILABLE');
  return envelope.result;
}

export async function stdinRequest() {let size=0;const chunks=[];for await(const chunk of process.stdin){size+=chunk.length;if(size>2*1024*1024)reject('INPUT_LIMIT');chunks.push(chunk);}return JSON.parse(Buffer.concat(chunks).toString('utf8'));}
const researchCategories=new Set(['UNOBSERVED_URL','MISSING_EVIDENCE','RECIPIENT','FIELDS','UNATTRIBUTED_REPLY','ACTIVITY_ID']);
const responseCategories=new Set(['OUTPUT_JSON','TOOL_ARGUMENTS','TOOL_CALL','TOOL_REQUEST','LOOKUP','PROPOSAL','TRIAGE','ACTION_REVIEW','CORE_FIELDS']);
const requestCategories=new Set(['MEDIA_BINDING','LEGACY_POST_BINDING']);
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
  return {ok:false,error:{code,message:code}};
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {process.stdout.write(JSON.stringify({ok:true,result:await dispatch(await stdinRequest())}));}
  catch(error) {process.stdout.write(JSON.stringify(safeError(error)));}
}

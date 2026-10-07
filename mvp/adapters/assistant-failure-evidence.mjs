import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash,randomUUID} from 'node:crypto';

const LIMIT=2*1024*1024;
const CREDENTIAL_TEXT=/access_token|refresh_token|id_token|client_secret|password=|session=|authorization|(?:access[_-]?token|refresh[_-]?token|id[_-]?token|client[_-]?secret|api[_-]?key|password|session|cookie|token|signature|secret)["'\\\s]*[:=]|\bBearer\s+[A-Za-z0-9._~+/-]{12,}|\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}/i;
const CREDENTIAL_KEY=/^(?:access[_-]?token|refresh[_-]?token|id[_-]?token|client[_-]?secret|api[_-]?key|password|session|cookies?|token|signature|secret|authorization)$/i;
function sensitive(value){
  const pending=[{value,depth:0}],decoded=new Set();let inspected=0;
  while(pending.length){
    const current=pending.pop();
    // These bounds apply only to inspecting a quarantined artifact, never to
    // model research or the number of searches/sites. Fail closed on ambiguity.
    if(++inspected>8192||current.depth>16)return true;
    if(typeof current.value==='string'){
      if(CREDENTIAL_TEXT.test(current.value))return true;
      if(!decoded.has(current.value)&&/^[\s]*[\[{"']/.test(current.value)){
        decoded.add(current.value);
        try{pending.push({value:JSON.parse(current.value),depth:current.depth+1});}catch{}
      }
    }else if(current.value&&typeof current.value==='object'){
      for(const [key,entry]of Object.entries(current.value)){
        if(CREDENTIAL_KEY.test(key))return true;
        pending.push({value:entry,depth:current.depth+1});
      }
    }
  }
  return false;
}
const hash=value=>createHash('sha256').update(value).digest('hex');

// Quarantine exact paid output only. No credentials, input context, images,
// logs or other run-home files are copied, and nothing becomes an admitted reply.
export async function persistAssistantFailureEvidence(laneBase,home,{input,stdout='',verificationStdout='',failure,secureDirectory}={}){
  try{
    if(typeof input!=='string'||typeof stdout!=='string'||typeof verificationStdout!=='string'||typeof secureDirectory!=='function')return null;
    if(path.dirname(home)!==path.resolve(laneBase)||!/^run-[A-Za-z0-9_-]+$/.test(path.basename(home)))return null;
    if((await fs.lstat(laneBase)).isSymbolicLink()||(await fs.lstat(home)).isSymbolicLink())return null;
    const artifacts=[],omissions={};const omit=key=>{omissions[key]=(omissions[key]??0)+1;};
    for(const [name,output,prefix]of [['events.jsonl',stdout,''],['verification.events.jsonl',verificationStdout,'verification_']]){
      let events='';
      for(const line of output.split('\n')){
        if(!line.trim())continue;
        if(sensitive(line)){omit('sensitive_'+prefix+'event');continue;}
        // Preserve an unfinished final record as quarantined evidence too.
        if(Buffer.byteLength(events+line+'\n')>LIMIT){omit(prefix+'event_byte_limit');break;}
        events+=line+'\n';
      }
      if(events)artifacts.push({name,content:events});
    }
    // Fixed output allowlist: caller input cannot select a path or another
    // run-home artifact (in particular credentials/configuration).
    for(const [name,prefix]of [['response.json',''],['verification.response.json','verification_']]){
      const response=path.join(home,name);
      const stat=await fs.lstat(response).catch(e=>{if(e.code==='ENOENT')return null;throw e;});
      if(stat?.isFile()&&!stat.isSymbolicLink()&&stat.size<=LIMIT){
        const content=await fs.readFile(response,'utf8');
        if(sensitive(content))omit('sensitive_'+prefix+'response');
        else if(Buffer.byteLength(content)<=LIMIT)artifacts.push({name,content});
      }else if(stat)omit(prefix+'response_not_bounded_regular_file');
    }
    if(!artifacts.length&&!Object.keys(omissions).length)return null;
    const root=path.join(laneBase,'failure-evidence');await fs.mkdir(root,{recursive:true,mode:0o700});
    if((await fs.lstat(root)).isSymbolicLink())return null;
    const id=randomUUID(),directory=path.join(root,`failure-${Date.now()}-${id}`);
    await fs.mkdir(directory,{mode:0o700});
    await secureDirectory(directory); // Must precede all private output writes.
    const files=[];
    for(const artifact of artifacts){await fs.writeFile(path.join(directory,artifact.name),artifact.content,{flag:'wx',mode:0o600});
      files.push({name:artifact.name,bytes:Buffer.byteLength(artifact.content),sha256:hash(artifact.content)});}
    const receipt={version:1,id,inputSha256:hash(input),inputBytes:Buffer.byteLength(input),files,omissions,
      errorCode:['ADAPTER_TIMEOUT','ADAPTER_PROCESS_FAILED','ADAPTER_OUTPUT_LIMIT','CANCELLED','ASSISTANT_FAILED','ASSISTANT_INVALID_RESPONSE','ASSISTANT_INVALID_RESEARCH'].includes(failure?.code)?failure.code:'ASSISTANT_FAILED',
      ...(['idle','total'].includes(failure?.timeoutKind)?{timeoutKind:failure.timeoutKind}:{}),
      at:new Date().toISOString(),admitted:false,retryAuthorized:false,replayAuthorized:false,
      privateOutput:true,credentialsCopied:false};
    await fs.writeFile(path.join(directory,'receipt.json'),JSON.stringify(receipt),{flag:'wx',mode:0o600});
    return receipt;
  }catch{return null;} // Evidence failure never masks cancellation/original error.
}

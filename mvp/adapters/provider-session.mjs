import {once} from 'node:events';
import {StringDecoder} from 'node:string_decoder';
import {pathToFileURL} from 'node:url';
import path from 'node:path';
import {createProviderSession} from './provider.mjs';
import {accountDefinition} from './config.mjs';
import {safeError} from './bridge.mjs';
import {createTraceRecorder,withTraceRecorder} from '../cli/trace-recorder.mjs';

export const SESSION_LIMITS=Object.freeze({maxRequests:400,maxActive:4,maxQueued:64,maxRequestBytes:2*1024*1024,
  maxResponseBytes:12*1024*1024,lifetimeMs:900000,idleMs:60000,retirementAckMs:5000});
const errorEnvelope=code=>({code,message:code,processRole:'provider-worker',processId:process.pid});
const requestError=error=>{
  // A hostile error getter cannot turn a settled request into a lost frame.
  let diagnostic;try{diagnostic=safeError(error).error;}catch{diagnostic={code:'ADAPTER_UNAVAILABLE',message:'ADAPTER_UNAVAILABLE'};}
  // An inner helper's exit facts must never be attributed to this still-running
  // worker. Unknown process provenance stays unknown instead of inventing a PID.
  return diagnostic.processRole||diagnostic.processExit?diagnostic:{...diagnostic,processRole:'provider-worker',processId:process.pid};
};

/** Finite, account-bound protocol. No failed request is ever dispatched twice. */
export async function serveProviderSession({account,session,input=process.stdin,output=process.stdout,limits={}}) {
  accountDefinition(account);
  const bound={...SESSION_LIMITS,...limits};
  for(const [key,value] of Object.entries(bound))if(!Number.isSafeInteger(value)||value<1||value>SESSION_LIMITS[key])throw new Error('INVALID_SESSION_LIMITS');
  const decoder=new StringDecoder('utf8'),seen=new Set(),queue=[];
  let buffer='',active=0,accepted=0,retiring=false,poisoned=false,retiringRejected=0,ended=false,finishing=false,writeChain=Promise.resolve(),idleTimer;
  let retirementTimer,waitingForEnd=false,terminalFailure;
  const terminal=code=>{terminalFailure??=Object.assign(new Error(code),{code});};
  let resolveDone,rejectDone;
  const done=new Promise((resolve,reject)=>{resolveDone=resolve;rejectDone=reject;});
  const emit=value=>{
    // Optional observation can never turn an otherwise valid business result
    // into an output-limit failure. Apply the business budget first.
    const {telemetry,...business}=value;
    let line=JSON.stringify(business)+'\n';
    if(Buffer.byteLength(line)>bound.maxResponseBytes)line=JSON.stringify({id:value.id,ok:false,error:errorEnvelope('ADAPTER_OUTPUT_LIMIT')})+'\n';
    else if(telemetry){const observed=JSON.stringify(value)+'\n';if(Buffer.byteLength(observed)<=bound.maxResponseBytes)line=observed;}
    writeChain=writeChain.then(async()=>{if(!output.write(line))await once(output,'drain');});
    writeChain.catch(()=>{terminal('PROVIDER_SESSION_OUTPUT_FAILED');retiring=true;input.pause();finish();});
    return writeChain;
  };
  const finish=()=>{
    if(finishing||!retiring||active||queue.length)return;
    // Retirement is a handshake: the supervisor stops admission and closes
    // stdin as soon as it receives our control frame, before waiting for replies.
    // Until EOF, an already-written frame must still receive its correlated
    // retiring response. Never detach input merely because local active is zero.
    if(!ended&&!terminalFailure){
      if(!waitingForEnd){
        waitingForEnd=true;
        writeChain.then(()=>{
          if(ended||finishing||terminalFailure)return;
          retirementTimer=setTimeout(()=>{terminal('PROVIDER_SESSION_EOF_TIMEOUT');finish();},bound.retirementAckMs);
        },()=>{});
      }
      return;
    }
    finishing=true;clearTimeout(idleTimer);clearTimeout(lifetimeTimer);clearTimeout(retirementTimer);
    input.off('data',onData);input.off('end',onEnd);input.off('error',onError);output.off('error',onOutputError);input.pause();
    Promise.resolve().then(()=>writeChain).finally(()=>session.close())
      .then(()=>terminalFailure?rejectDone(terminalFailure):resolveDone(),rejectDone);
  };
  const completeTrace=(entry,outcome)=>{
    entry.queueSpan?.finish({outcome,reasonCode:outcome==='completed'?undefined:'not_attempted'});
    return entry.recorder?.finish();
  };
  const notStarted=(entry,reason='retiring')=>emit({id:entry.id,ok:false,error:{...errorEnvelope('PROVIDER_SESSION_RETIRING'),
    // Only this queue owner can attest that session.run was never entered.
    // This is settlement evidence, never permission to replay a mutation.
    sessionAdmission:{version:1,state:'not-started',reason}},...(entry.recorder?{telemetry:completeTrace(entry,'cancelled')}:{})});
  const retire=(reason='graceful')=>{
    if(reason==='resource')poisoned=true;
    if(!retiring){retiring=true;clearTimeout(idleTimer);emit({type:'retiring'});}
    // Graceful EOF/lifetime/request-limit retirement drains accepted frames.
    // Failed/UNKNOWN shared resources must not start their remaining queue.
    if(poisoned)for(const entry of queue.splice(0))notStarted(entry);
    finish();
  };
  const reply=value=>{
    const response=emit(value);
    // Stop local admission immediately, even when the final response is waiting
    // for stdout drain. The write chain still sends response before control.
    if(session.retiring)retire('resource');
    return response;
  };
  const idle=()=>{clearTimeout(idleTimer);if(!active&&!queue.length&&!retiring)idleTimer=setTimeout(retire,bound.idleMs);};
  const fatal=code=>{
    // Once retirement is announced, a malformed trailing frame must not turn
    // into an apparently clean EOF acknowledgment with an uncorrelated loss.
    if(retiring)terminal(code);
    if(!retiring)emit({type:'fatal',error:errorEnvelope(code)});
    for(const entry of queue.splice(0))emit({id:entry.id,ok:false,error:errorEnvelope(code),...(entry.recorder?{telemetry:completeTrace(entry,'failed')}:{})});
    retire();
  };
  const drain=()=>{
    if(session.retiring)retire('resource');
    while(!poisoned&&active<bound.maxActive&&queue.length){
      const entry=queue.shift(),{id,request,recorder}=entry;active++;
      entry.queueSpan?.finish();
      Promise.resolve().then(()=>withTraceRecorder(recorder,async()=>{
        const span=recorder?.start('provider.dispatch',{spanClass:'activity'});
        try{const result=await (span?span.scope(()=>session.run(request)):session.run(request));span?.finish();return {id,ok:true,result};}
        catch(error){span?.finish({outcome:'failed'});return {id,ok:false,error:requestError(error)};}
        finally{entry.telemetry=recorder?.finish();}
      }))
        .then(value=>reply({...value,...(entry.telemetry?{telemetry:entry.telemetry}:{})}))
        .catch(()=>{})
        .finally(()=>{active--;if(session.retiring)retire('resource');drain();idle();finish();});
    }
    idle();finish();
  };
  const frame=line=>{
    if(!line.trim())return fatal('PROVIDER_SESSION_PROTOCOL');
    let value;try{value=JSON.parse(line);}catch{return fatal('PROVIDER_SESSION_PROTOCOL');}
    if(!value||Array.isArray(value)||!['id,request','id,request,traceContext'].includes(Object.keys(value).sort().join(','))||typeof value.id!=='string'||
      !/^[A-Za-z0-9_-]{1,100}$/.test(value.id)||!value.request||typeof value.request!=='object'||Array.isArray(value.request))return fatal('PROVIDER_SESSION_PROTOCOL');
    if(seen.has(value.id))return fatal('PROVIDER_SESSION_DUPLICATE_ID');
    // Only the per-frame context is used. A pool process never inherits the
    // context of the request which happened to start its generation.
    let recorder;try{if(value.traceContext?.companyKey===account)recorder=createTraceRecorder({context:value.traceContext});}catch{}
    const entry={id:value.id,request:value.request,recorder,queueSpan:recorder?.start('provider.queue.wait',{spanClass:'wait'})};
    if(retiring){
      // Bound late-frame correlation until supervisor EOF; repeated IDs remain
      // protocol violations even if their first frame arrived after retirement.
      seen.add(value.id);retiringRejected++;
      if(retiringRejected>bound.maxQueued){fatal('PROVIDER_SESSION_RETIRING_INPUT_LIMIT');input.pause();return;}
      return void notStarted(entry);
    }
    if(queue.length>=bound.maxQueued)return fatal('PROVIDER_SESSION_QUEUE_LIMIT');
    seen.add(value.id);accepted++;queue.push(entry);clearTimeout(idleTimer);drain();
    if(accepted>=bound.maxRequests)retire();
  };
  const onData=chunk=>{
    buffer+=decoder.write(chunk);
    let end;
    while((end=buffer.indexOf('\n'))>=0){
      const line=buffer.slice(0,end);buffer=buffer.slice(end+1);
      if(Buffer.byteLength(line)>bound.maxRequestBytes){fatal('PROVIDER_SESSION_INPUT_LIMIT');buffer='';return;}
      frame(line);
    }
    if(Buffer.byteLength(buffer)>bound.maxRequestBytes){fatal('PROVIDER_SESSION_INPUT_LIMIT');buffer='';}
  };
  const onEnd=()=>{if(ended)return;ended=true;buffer+=decoder.end();if(buffer.length)fatal('PROVIDER_SESSION_TRUNCATED_FRAME');retire();};
  const onError=()=>{terminal('PROVIDER_SESSION_INPUT_FAILED');fatal('PROVIDER_SESSION_INPUT_FAILED');};
  const onOutputError=()=>{terminal('PROVIDER_SESSION_OUTPUT_FAILED');retiring=true;queue.length=0;input.pause();finish();};
  const lifetimeTimer=setTimeout(retire,bound.lifetimeMs);
  input.on('data',onData);input.once('end',onEnd);input.once('error',onError);output.on('error',onOutputError);idle();
  return done;
}

if(process.argv[1]&&import.meta.url===pathToFileURL(path.resolve(process.argv[1])).href){
  try{
    if(process.argv.length!==4||process.argv[2]!=='--account')throw new Error('INVALID_SESSION_ARGUMENTS');
    const account=process.argv[3];accountDefinition(account);
    await serveProviderSession({account,session:createProviderSession(account)});
  }catch(error){
    // Never include stderr, input, source paths, secret references or reply text.
    process.stdout.write(JSON.stringify({type:'fatal',error:{...safeError(error).error,processRole:'provider-worker',processId:process.pid}})+'\n');
    process.exitCode=1;
  }
}

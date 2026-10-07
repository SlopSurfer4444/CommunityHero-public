import test from 'node:test';
import assert from 'node:assert/strict';
import {runProcess} from './process.mjs';
import {dispatch,safeError} from './bridge.mjs';

test('process diagnostics retain only a declared role and OS PID',async()=>{
 await assert.rejects(runProcess(process.execPath,['-e','process.stderr.write("private");process.exit(7)'],{processRole:'provider-process'}),error=>{
  const projected=safeError(error).error;assert.equal(projected.processRole,'provider-process');assert.ok(projected.processId>0);assert.equal(projected.processExit.exitCode,7);assert.doesNotMatch(JSON.stringify(projected),/private/);return true;
 });
 assert.equal(safeError({code:'ADAPTER_PROCESS_FAILED',processRole:'secret command',processId:123}).error.processRole,undefined);
 assert.equal(safeError({code:'ADAPTER_PROCESS_FAILED',processRole:'provider-worker',processId:'secret'}).error.processId,undefined);
});
test('readback process failure keeps prior verified execute receipt and never retries execute',async()=>{
 const calls=[];
 const resolvePaths=()=>({conveyorRepo:'fixture',providerRepo:'fixture',providerNode:'fixture'});
 const dependencies={resolvePaths,runProcessFn:async(_exe,_args,options)=>{
  const op=JSON.parse(options.input).op;calls.push(op);
  if(op==='execute')return {stdout:JSON.stringify({ok:true,result:{account:'baw-russia',operation:'execute',results:[{actionId:'a',itemId:'i',status:'verified',receipt:{mutation:{status:200},verification:{verified:true}}}]}})};
  throw Object.assign(new Error('private stderr'),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221226505,signal:null}});
 }};
 const executeReceipt=await dispatch({op:'execute',account:'baw-russia',actions:[]},dependencies);
 await assert.rejects(dispatch({op:'readback',account:'baw-russia',actions:[]},dependencies),error=>{
  assert.deepEqual(safeError(error),{ok:false,error:{code:'ADAPTER_PROCESS_FAILED',message:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221226505},adapterOperation:'readback',readbackProcessRecovery:{attempts:2,firstFailure:{code:'ADAPTER_PROCESS_FAILED',message:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221226505},adapterOperation:'readback'}}}});
  return true;
 });
 assert.deepEqual(calls,['execute','readback','readback']);
 assert.equal(executeReceipt.results[0].receipt.mutation.status,200);
 assert.equal(executeReceipt.results[0].receipt.verification.verified,true);
 assert.equal(safeError({code:'ADAPTER_PROCESS_FAILED',adapterOperation:'private value'}).error.adapterOperation,undefined);
});
test('readback native crash retries only the same read inside the original deadline',async()=>{
 const request={op:'readback',account:'baw-russia',actions:[{actionId:'a',itemId:'i',readbackEvidence:{baselineReplyIds:[]}}]};
 const calls=[];let elapsed=0;
 const result=await dispatch(request,{resolvePaths:()=>({conveyorRepo:'fixture',providerRepo:'fixture',providerNode:'fixture'}),nowFn:()=>elapsed,runProcessFn:async(_exe,_args,options)=>{
  calls.push(options);
  if(calls.length===1){elapsed=12000;throw Object.assign(new Error('private'),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221226505}});}
  return {stdout:JSON.stringify({ok:true,result:{account:'baw-russia',results:[{status:'verified',actionId:'a',itemId:'i'}]}})};
 }});
 assert.equal(calls.length,2);assert.equal(calls[0].input,calls[1].input);
 assert.equal(calls[0].timeoutMs,180000);assert.equal(calls[1].timeoutMs,168000);
 assert.equal(result.results[0].status,'verified');assert.equal(result.readbackProcessRecovery.attempts,2);
 assert.equal(result.readbackProcessRecovery.firstFailure.processExit.exitCode,3221226505);
 assert.doesNotMatch(JSON.stringify(result),/private/);
});
test('execute crashes and non-crash readback failures are never retried',async()=>{
 for(const [op,code,exitCode] of [['execute','ADAPTER_PROCESS_FAILED',3221226505],['readback','ADAPTER_PROCESS_FAILED',1],['readback','ADAPTER_TIMEOUT',3221226505],['readback','CANCELLED',3221226505],['readback','ADAPTER_OUTPUT_LIMIT',3221226505]]){
  let calls=0;
  await assert.rejects(dispatch({op,account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{calls++;throw Object.assign(new Error('failure'),{code,processExit:{exitCode}});}}),{code});
  assert.equal(calls,1,`${op}/${code}`);
 }
});
test('readback budget exhaustion and valid nonverification do not trigger more reads',async()=>{
 let calls=0,elapsed=0;
 await assert.rejects(dispatch({op:'readback',account:'baw-russia'},{resolvePaths:()=>({}),nowFn:()=>elapsed,runProcessFn:async()=>{
  calls++;elapsed=179500;throw Object.assign(new Error('failure'),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221226505}});
 }}),{code:'ADAPTER_PROCESS_FAILED'});
 assert.equal(calls,1);
 calls=0;
 const result=await dispatch({op:'readback',account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{
  calls++;return {stdout:JSON.stringify({ok:true,result:{results:[{status:'unknown',code:'READBACK_NOT_VERIFIED'}]}})};
 }});
 assert.equal(calls,1);assert.equal(result.results[0].status,'unknown');
});
test('crash recovery preserves unknown or provider refusal and bounded failure evidence',async()=>{
 for(const response of [{ok:true,result:{results:[{status:'unknown'}]}},{ok:false,error:{code:'ACCOUNT_SCOPE_MISMATCH'}}]){
  let calls=0;
  const pending=dispatch({op:'readback',account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{
   if(++calls===1)throw Object.assign(new Error('private'),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:-1073740791}});
   return {stdout:JSON.stringify(response)};
  }});
  if(response.ok)assert.equal((await pending).results[0].status,'unknown');
  else await assert.rejects(pending,error=>{assert.equal(error.code,'ACCOUNT_SCOPE_MISMATCH');assert.equal(safeError(error).error.readbackProcessRecovery.attempts,2);return true;});
  assert.equal(calls,2);
 }
});
test('fatal signal readback recovery is bounded and malformed output is never retried',async()=>{
 for(const failure of [{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:null,signal:'SIGSEGV'}},{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:7516193801}},{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:null,signal:'SIGTERM'}}]){
  let calls=0;
  await assert.rejects(dispatch({op:'readback',account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{calls++;throw Object.assign(new Error('failure'),failure);}}),{code:'ADAPTER_PROCESS_FAILED'});
  assert.equal(calls,failure.processExit.signal==='SIGSEGV'?2:1);
 }
 let calls=0;
 await assert.rejects(dispatch({op:'readback',account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{calls++;return {stdout:'{invalid'};}}),SyntaxError);
 assert.equal(calls,1);
 const sanitized=safeError({code:'ADAPTER_PROCESS_FAILED',adapterOperation:'readback',readbackProcessRecovery:{attempts:2,firstFailure:{code:'bad private text',processExit:{exitCode:Infinity,stderr:'private'},message:'private',readbackProcessRecovery:{attempts:2}}}});
 assert.deepEqual(sanitized.error.readbackProcessRecovery,{attempts:2,firstFailure:{code:'ADAPTER_UNAVAILABLE',message:'ADAPTER_UNAVAILABLE',adapterOperation:'readback'}});
});
test('nonzero child exit retains bounded OS facts without child output',async()=>{
 await assert.rejects(runProcess(process.execPath,['-e',"console.error('private token');console.log('private comment');process.exit(17)"]),error=>{
  assert.deepEqual(safeError(error),{ok:false,error:{code:'ADAPTER_PROCESS_FAILED',message:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:17}}});
  assert.doesNotMatch(JSON.stringify(error),/private token|private comment/);
  return true;
 });
});
test('safe error excludes arbitrary process details and invalid exit values',()=>{
 const envelope=safeError({code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:Infinity,signal:'private token',stderr:'private'}});
 assert.deepEqual(envelope,{ok:false,error:{code:'ADAPTER_PROCESS_FAILED',message:'ADAPTER_PROCESS_FAILED'}});
 assert.deepEqual(safeError({code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:3221225477,signal:'SIGSEGV'}}).error.processExit,{exitCode:3221225477,signal:'SIGSEGV'});
});
test('observer rejection terminates subprocess and preserves reason',async()=>{
 const started=Date.now();
 await assert.rejects(runProcess(process.execPath,['-e',"console.log('event');setInterval(()=>{},1000)"],{
  timeoutMs:10000,onStdout:()=>{throw Object.assign(new Error('Budget reached'),{code:'ASSISTANT_RESEARCH_LIMIT'});}
 }),{code:'ASSISTANT_RESEARCH_LIMIT'});
 assert.ok(Date.now()-started<8000);
});
test('optional stdout observer preserves split UTF-8 and ordinary result',async()=>{
 let observed='';
 const result=await runProcess(process.execPath,['-e',"const b=Buffer.from('Привет');process.stdout.write(b.subarray(0,1));setTimeout(()=>process.stdout.write(b.subarray(1)),25)"],{onStdout:s=>observed+=s});
 assert.equal(result.stdout,'Привет');assert.equal(observed,'Привет');
});
test('absolute subprocess deadline is not renewed by continuous progress and reaps the child',async()=>{
 let pid,events=0;
 await assert.rejects(runProcess(process.execPath,['-e',"console.log(process.pid);setInterval(()=>console.log('progress'),20)"],{
  timeoutMs:1500,onStdout:chunk=>{if(!pid)pid=Number(chunk.split('\n')[0]);events++;}
 }),{code:'ADAPTER_TIMEOUT'});
 assert.ok(Number.isSafeInteger(pid)&&pid>0);assert.ok(events>2);
 assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});
test('ordinary SIGTERM cancellation rejects without retry and reaps the child',async()=>{
 let pid,signalled=false;
 await assert.rejects(runProcess(process.execPath,['-e',"console.log(process.pid);setInterval(()=>{},1000)"],{
  timeoutMs:10000,onStdout:chunk=>{if(!signalled){pid=Number(chunk.trim());signalled=true;process.emit('SIGTERM');}}
 }),{code:'CANCELLED'});
 assert.ok(Number.isSafeInteger(pid)&&pid>0);assert.equal(signalled,true);
 assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

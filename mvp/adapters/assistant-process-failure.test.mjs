import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {runProcess,processExitFacts} from './process.mjs';
import {codexErrorEventFacts,assistantProcessObservation,persistAssistantProcessDiagnostic,admitAssistantEvents,
  runUrlVerificationAttempt,withAssistantLane} from './assistant.mjs';
import {safeError} from './bridge.mjs';

const requestId='req_0123456789abcdef0123456789abcdef';
const secret='PRIVATE token https://secret.example/customer';
const failure=()=>Object.assign(new Error(secret),{code:'ADAPTER_PROCESS_FAILED',
  processExit:{exitCode:17,signal:null,stderr:secret},outputBytes:{stdout:123,stderr:456,private:secret},
  stdout:secret,stderr:secret});
const context=()=>({input:secret,stage:'stronger_review'});
const fallback='Falling back from WebSockets to HTTPS transport. stream disconnected before completion: websocket closed by server before response.completed';

test('Codex error projection admits only closed codes/types and narrow opaque request IDs',()=>{
  assert.deepEqual(codexErrorEventFacts({type:'turn.failed',error:{code:'invalid_json_schema',
    type:'invalid_request_error',request_id:requestId,message:secret,private:secret}}),
    {eventType:'turn.failed',code:'invalid_json_schema',errorType:'invalid_request_error',requestId});
  assert.deepEqual(codexErrorEventFacts({type:'error',code:'context_length_exceeded',requestId,
    message:secret}),{eventType:'error',code:'context_length_exceeded',requestId});
  assert.equal(codexErrorEventFacts({type:'item.completed',item:{type:'agent_message',text:secret}}),null);
  for(const error of [{code:secret,type:secret,request_id:secret},{message:'401 Unauthorized: private quota network schema'},
    {error:{code:'invalid_json_schema',request_id:requestId},message:secret},{code:'PRIVATE_TOKEN',request_id:'req_PRIVATE_TOKEN'},
    {code:'unknown_error',type:'custom_private',request_id:'https://secret.example'}]){
    assert.deepEqual(codexErrorEventFacts({type:'turn.failed',error}),{eventType:'turn.failed'});
  }
});

test('observed message-only transport form is exact and never converts arbitrary prose to a cause',()=>{
  assert.deepEqual(codexErrorEventFacts({type:'item.completed',item:{type:'error',message:fallback}}),
    {eventType:'item.error',reportedCondition:'transport_fallback'});
  assert.deepEqual(codexErrorEventFacts({type:'turn.failed',error:{message:fallback}}),
    {eventType:'turn.failed',reportedCondition:'transport_fallback'});
  assert.deepEqual(codexErrorEventFacts({type:'turn.failed',error:{message:fallback.slice('Falling back from WebSockets to HTTPS transport. '.length)}}),
    {eventType:'turn.failed',reportedCondition:'stream_disconnected'});
  for(const message of [fallback+' '+secret,'prefix '+fallback,'Invalid schema with '+secret,
    'Please renew authentication',secret+' network quota auth schema'])
    assert.deepEqual(codexErrorEventFacts({type:'error',message}),{eventType:'error'});
});

test('embedded JSON upstream error retains only closed schema/status/param facts',()=>{
  const message='unexpected status 400 Bad Request: '+JSON.stringify({type:'error',status:400,error:{code:'invalid_json_schema',type:'invalid_request_error',param:'text.format.schema',message:secret},headers:{Authorization:secret},url:secret});
  for(const event of [{type:'error',message},{type:'turn.failed',error:{message}},{type:'item.completed',item:{type:'error',message}}]) {
    const facts=codexErrorEventFacts(event);
    assert.equal(facts.code,'invalid_json_schema');assert.equal(facts.errorType,'invalid_request_error');
    assert.equal(facts.reportedHttpStatus,400);assert.equal(facts.param,'text.format.schema');
    assert.doesNotMatch(JSON.stringify(facts),/PRIVATE|https:|Authorization|message|headers/);
  }
  for(const message of ['schema authentication quota 400 '+secret,'{broken JSON}',JSON.stringify({error:{code:secret,type:secret,param:secret,request_id:secret},status:900}),JSON.stringify({error:secret,status:'400'})])assert.deepEqual(codexErrorEventFacts({type:'error',message}),{eventType:'error'});
  assert.deepEqual(codexErrorEventFacts({type:'error',message:'x'.repeat(2097153)+'{"error":{"code":"invalid_json_schema"}}'}),{eventType:'error'});
});

test('embedded JSON diagnostic survives persistence with independent closed-field validation',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-embedded-process-'));
  try {
    const observation=assistantProcessObservation(context());
    observation.observe({type:'turn.failed',error:{message:JSON.stringify({status:400,error:{code:'invalid_json_schema',type:'invalid_request_error',param:'text.format.schema',message:secret}})}});
    const snapshot=observation.snapshot;
    observation.snapshot=e=>({...snapshot(e),errorEvents:[...snapshot(e).errorEvents,{eventType:'error',reportedHttpStatus:900,param:secret,message:secret}]});
    assert.equal(await persistAssistantProcessDiagnostic(base,observation,failure()),true);
    const dir=path.join(base,'process-diagnostics'),files=await fs.readdir(dir),raw=await fs.readFile(path.join(dir,files[0]),'utf8'),receipt=JSON.parse(raw);
    assert.deepEqual(receipt.errorEvents,[{eventType:'turn.failed',code:'invalid_json_schema',errorType:'invalid_request_error',reportedHttpStatus:400,param:'text.format.schema'},{eventType:'error'}]);
    assert.equal(receipt.rootCause,'reported_error_code');assert.equal(receipt.retryAuthorized,false);assert.equal(receipt.partialOutputAdmitted,false);
    assert(Buffer.byteLength(raw)<=4096);assert.doesNotMatch(raw,/PRIVATE|https:|customer|message|headers/);
  } finally {assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('failed subprocess preserves exit and byte counters without retaining private child output',async()=>{
  await assert.rejects(runProcess(process.execPath,['-e',
    'process.stdout.write("private-comment");process.stderr.write("private-token");process.exit(17)']),error=>{
    assert.equal(error.code,'ADAPTER_PROCESS_FAILED');assert.deepEqual(error.processExit,{exitCode:17});
    assert.deepEqual(error.outputBytes,{stdout:15,stderr:13});
    assert.doesNotMatch(JSON.stringify(error),/private-comment|private-token/);
    assert.deepEqual(safeError(error).error.processExit,{exitCode:17});return true;
  });
  assert.deepEqual(processExitFacts({exitCode:Infinity,signal:secret,stdout:secret}),{});
  assert.deepEqual(processExitFacts({exitCode:-1073740791,signal:'SIGSEGV'}),{exitCode:-1073740791,signal:'SIGSEGV'});
  for(const signal of ['SIGHUP','SIGPIPE','SIGQUIT'])assert.deepEqual(processExitFacts({signal}),{signal});
});

test('process observation binds stage and input hash while bounding events and preserving unknown cause',()=>{
  let now=1000;const observation=assistantProcessObservation(context(),{now:()=>now});
  now=2000;observation.observe({type:'thread.started',id:secret});
  now=2500;observation.observe({type:'error',message:secret+' quota authentication schema'});
  now=3000;const snapshot=observation.snapshot(failure());
  assert.equal(snapshot.elapsedMs,2000);assert.equal(snapshot.lastEventElapsedMs,1500);
  assert.equal(snapshot.inputSha256,createHash('sha256').update(secret).digest('hex'));
  assert.equal(snapshot.inputBytes,Buffer.byteLength(secret));assert.equal(snapshot.stage,'stronger_review');
  assert.equal(snapshot.eventCount,2);assert.equal(snapshot.errorEventCount,1);assert.equal(snapshot.rootCause,'unknown');
  assert.deepEqual(snapshot.errorEvents,[{eventType:'error'}]);
  assert.deepEqual(snapshot.processExit,{exitCode:17});assert.deepEqual(snapshot.outputBytes,{stdout:123,stderr:456});
  assert.equal(snapshot.timeoutMs,2700000);assert.equal(snapshot.retryAuthorized,false);
  assert.equal(snapshot.partialOutputAdmitted,false);assert.doesNotMatch(JSON.stringify(snapshot),/PRIVATE|https:|customer/);
  for(let i=0;i<20;i++)observation.observe({type:'turn.failed',error:{code:'invalid_json_schema',message:secret}});
  const bounded=observation.snapshot(failure());assert.equal(bounded.errorEvents.length,8);assert.equal(bounded.errorEventCount,21);
  assert.equal(bounded.rootCause,'reported_error_code');
  bounded.errorEvents[0].message=secret;assert.equal(observation.snapshot(failure()).errorEvents[0].message,undefined);
  assert.equal(observation.snapshot({code:'CANCELLED'}),null);
  assert.throws(()=>assistantProcessObservation({...context(),stage:secret}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.equal(assistantProcessObservation({...context(),timeoutMs:180001}).snapshot(failure()).timeoutMs,180001);
  assert.equal(assistantProcessObservation({...context(),timeoutMs:900000}).snapshot(failure()).timeoutMs,900000);
  assert.throws(()=>assistantProcessObservation({...context(),timeoutMs:2700001}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('durable diagnostic survives private lane cleanup without raw events or arbitrary receipt fields',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-assistant-process-'));
  try{
    await assert.rejects(withAssistantLane(base,'preparation',async home=>{
      await fs.writeFile(path.join(home,'response.json'),secret);
      const observation=assistantProcessObservation(context());
      observation.observe({type:'turn.failed',error:{code:'invalid_json_schema',request_id:requestId,message:secret}});
      const original=observation.snapshot;
      observation.snapshot=e=>({...original(e),secret,stdout:secret,stderr:secret,summary:secret,
        errorEvents:[{eventType:'turn.failed',code:'invalid_json_schema',requestId,message:secret,nested:{token:secret}},
          {eventType:'private '+secret,code:'invalid_json_schema'}]});
      assert.equal(await persistAssistantProcessDiagnostic(path.dirname(home),observation,failure()),true);
      throw failure();
    }),{code:'ADAPTER_PROCESS_FAILED'});
    const lane=path.join(base,'preparation'),names=await fs.readdir(lane);
    assert.deepEqual(names,['process-diagnostics']);
    const directory=path.join(lane,'process-diagnostics'),files=await fs.readdir(directory);
    const raw=await fs.readFile(path.join(directory,files[0]),'utf8'),receipt=JSON.parse(raw);
    assert(Buffer.byteLength(raw)<=4096);assert.doesNotMatch(raw,/PRIVATE|https:|customer|response\.json|message|nested/);
    assert.deepEqual(receipt.errorEvents,[{eventType:'turn.failed',code:'invalid_json_schema',requestId}]);
    assert.equal(receipt.errorCode,'ADAPTER_PROCESS_FAILED');assert.equal(receipt.retryAuthorized,false);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('bounded error observations retain the terminal fact after many recoverable item errors',()=>{
  for(const type of ['turn.failed','error']){
    const observation=assistantProcessObservation(context());
    for(let i=0;i<12;i++)observation.observe({type:'item.completed',item:{type:'error',message:secret}});
    observation.observe({type,error:{code:'invalid_json_schema',message:secret},message:secret});
    const receipt=observation.snapshot(failure());
    assert.equal(receipt.errorEventCount,13);assert.equal(receipt.errorEvents.length,8);
    assert.equal(receipt.errorEvents.at(-1).eventType,type);
    assert.equal(receipt.errorEvents.at(-1).code,'invalid_json_schema');
    assert.equal(receipt.rootCause,'reported_error_code');
    assert.doesNotMatch(JSON.stringify(receipt),/PRIVATE|https:|customer/);
  }
});

test('final event admission rejects terminal errors without a trailing newline and permits item fallback',()=>{
  for(const type of ['turn.failed','error'])assert.throws(()=>admitAssistantEvents(
    JSON.stringify({type,message:secret,error:{message:secret}})),{code:'ADAPTER_PROCESS_FAILED'});
  assert.doesNotThrow(()=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{type:'error',message:fallback}})));
});

test('receipt retention is scoped and best-effort failure never replaces original failure',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-process-retention-'));
  try{
    const observation=assistantProcessObservation(context());
    assert.equal(await persistAssistantProcessDiagnostic(base,observation,{code:'CANCELLED'}),false);
    assert.equal(await persistAssistantProcessDiagnostic(path.join(base,'missing'),observation,failure()),false);
    for(let i=0;i<34;i++)assert.equal(await persistAssistantProcessDiagnostic(base,observation,failure()),true);
    const directory=path.join(base,'process-diagnostics');assert.equal((await fs.readdir(directory)).length,32);
    await fs.writeFile(path.join(directory,'operator-note.txt'),'retained');
    assert.equal(await persistAssistantProcessDiagnostic(base,observation,failure()),true);
    assert.equal(await fs.readFile(path.join(directory,'operator-note.txt'),'utf8'),'retained');
    assert.equal((await fs.readdir(directory)).filter(name=>name.endsWith('.json')).length,32);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('URL verification terminal event records provenance and crosses safe bridge after child exit, without retry',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-verification-process-')),home=path.join(base,'run-fixture');
  try{
    await fs.mkdir(home);let calls=0;
    const attempt={input:secret,schema:{type:'object'},instructions:secret,deadline:5000,remainingCalls:1,checkTrace:()=>{}};
    await assert.rejects(runUrlVerificationAttempt({home,cli:'fixture'},attempt,{now:()=>1000,runProcessFn:async(_cli,_args,options)=>{
      calls++;assert.equal(options.timeoutMs,4000);assert.equal(options.maxOutputBytes,2*1024*1024);
      // Match runProcess: an observer exception stops/reaps the child, then
      // rejection adds OS exit facts; the private event itself is not retained.
      try {options.onStdout(JSON.stringify({type:'turn.failed',error:{code:'invalid_json_schema',message:secret}})+'\n');}
      catch(error){error.processExit={exitCode:17};error.outputBytes={stdout:123,stderr:0};throw error;}
      assert.fail('terminal event must reject the observer before the process timeout');
    }}),error=>{
      assert.equal(error.code,'ASSISTANT_FAILED');assert.deepEqual(safeError(error).error.processExit,{exitCode:17});
      assert.doesNotMatch(JSON.stringify(error),/PRIVATE|https:/);return true;
    });
    assert.equal(calls,1);
    const directory=path.join(base,'process-diagnostics'),files=await fs.readdir(directory);
    const diagnostic=JSON.parse(await fs.readFile(path.join(directory,files[0]),'utf8'));
    assert.equal(diagnostic.stage,'url_verification');assert.equal(diagnostic.timeoutMs,4000);
    assert.equal(diagnostic.errorEvents[0].code,'invalid_json_schema');assert.equal(diagnostic.processExit.exitCode,17);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('resource and tool failures keep existing codes and no process retry or invented model cause',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-verification-guard-')),home=path.join(base,'run-fixture');
  try{
    await fs.mkdir(home);
    for(const code of ['ADAPTER_TIMEOUT','CANCELLED','ASSISTANT_ISOLATION_FAILED','ASSISTANT_RESEARCH_LIMIT','ASSISTANT_INVALID_RESPONSE']){
      let calls=0;
      await assert.rejects(runUrlVerificationAttempt({home,cli:'fixture'},
        {input:'{}',schema:{type:'object'},instructions:'Fixture',deadline:5000,remainingCalls:1,checkTrace:()=>{}},
        {now:()=>1000,runProcessFn:async()=>{calls++;throw Object.assign(new Error(secret),{code,processExit:{exitCode:17}});}}),{code});
      assert.equal(calls,1);
    }
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

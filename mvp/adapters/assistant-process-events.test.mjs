import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {assertAssistantEventContinuation} from './assistant-process-events.mjs';
import {runProcess} from './process.mjs';
import {assistantProcessObservation,persistAssistantProcessDiagnostic} from './assistant.mjs';

const secret='PRIVATE https://customer.example/token';
const fallback='Falling back from WebSockets to HTTPS transport. stream disconnected before completion: websocket closed by server before response.completed';

test('terminal event types reject without copying arbitrary upstream details',()=>{
  for(const type of ['turn.failed','error'])assert.throws(
    ()=>assertAssistantEventContinuation({type,message:secret,error:{code:secret,message:secret}}),
    error=>{assert.equal(error.code,'ADAPTER_PROCESS_FAILED');assert.doesNotMatch(error.message+JSON.stringify(error),/PRIVATE|https:/);return true;});
});

test('item errors, tool failures and transport fallback never terminate an active turn',()=>{
  for(const type of ['item.started','item.updated','item.completed'])
    for(const message of [fallback,secret,'invalid_json_schema'])
      assert.doesNotThrow(()=>assertAssistantEventContinuation({type,item:{type:'error',message}}));
  for(const event of [null,{}, {type:'turn.started'}, {type:'turn.completed'},
    {type:'item.completed',item:{type:'web_search',status:'failed',error:{message:secret}}}])
    assert.doesNotThrow(()=>assertAssistantEventContinuation(event));
});

for(const type of ['turn.failed','error'])test(`${type} stops and reaps a still-active child, retaining bounded sanitized diagnostics`,async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-terminal-event-'));
  let pid,buffer='',failure;
  const observation=assistantProcessObservation({input:secret,stage:'stronger_review',timeoutMs:10000});
  const started=performance.now();
  try {
    await assert.rejects(runProcess(process.execPath,['-e',
      `console.log(JSON.stringify({type:'thread.started',pid:process.pid}));console.log(JSON.stringify({type:${JSON.stringify(type)},message:${JSON.stringify(secret)},error:{code:'invalid_json_schema',message:${JSON.stringify(secret)}}}));setInterval(()=>{},1000);`],
      {timeoutMs:10000,onStdout:chunk=>{
        buffer+=chunk;let at;
        while((at=buffer.indexOf('\n'))>=0){
          const event=JSON.parse(buffer.slice(0,at));buffer=buffer.slice(at+1);
          if(event.type==='thread.started')pid=event.pid;
          observation.observe(event);assertAssistantEventContinuation(event);
        }
      }}),error=>{failure=error;assert.equal(error.code,'ADAPTER_PROCESS_FAILED');return true;});
    assert(performance.now()-started<8000,'terminal event must reject before absolute process timeout');
    assert(Number.isSafeInteger(pid)&&pid>0);
    assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
    assert.equal(await persistAssistantProcessDiagnostic(base,observation,failure),true);
    const dir=path.join(base,'process-diagnostics'),names=await fs.readdir(dir);
    assert.equal(names.length,1);
    const raw=await fs.readFile(path.join(dir,names[0]),'utf8'),receipt=JSON.parse(raw);
    assert.equal(receipt.errorCode,'ADAPTER_PROCESS_FAILED');
    assert.equal(receipt.errorEvents[0].eventType,type);
    assert.equal(receipt.errorEvents[0].code,'invalid_json_schema');
    assert.equal(receipt.retryAuthorized,false);assert.equal(receipt.partialOutputAdmitted,false);
    assert(Buffer.byteLength(raw)<=4096);assert.doesNotMatch(raw,/PRIVATE|https:|customer/);
  } finally {
    assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));
    await fs.rm(base,{recursive:true,force:true});
  }
});

test('recoverable item error lets the real child finish with its eventual response',async()=>{
  let buffer='',completed=false;
  const result=await runProcess(process.execPath,['-e',
    `console.log(JSON.stringify({type:'item.completed',item:{type:'error',message:${JSON.stringify(fallback)}}}));setTimeout(()=>console.log(JSON.stringify({type:'turn.completed'})),30);`],
    {timeoutMs:10000,onStdout:chunk=>{
      buffer+=chunk;let at;
      while((at=buffer.indexOf('\n'))>=0){const event=JSON.parse(buffer.slice(0,at));buffer=buffer.slice(at+1);
        assertAssistantEventContinuation(event);if(event.type==='turn.completed')completed=true;}
    }});
  assert.equal(result.code,0);assert.equal(completed,true);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough} from 'node:stream';
import {serveProviderSession} from './provider-session.mjs';
import {dispatchEnvelope} from './bridge.mjs';
import {createTraceRecorder,currentTraceRecorder} from '../cli/trace-recorder.mjs';
import {validateTelemetryEnvelope,validateTraceEvent} from '../cli/trace-contract.mjs';

const context=id=>({version:1,traceId:'trace-'+id,companyKey:'baw-russia',runtimeId:'fixture-runtime',runtimeEpoch:7,
  sourcePin:'a'.repeat(64),parentSpanId:'parent-'+id,ids:{jobId:'job-'+id,logicalOperationId:'operation-'+id},
  lineage:{binarySha256:'b'.repeat(64),coreSha256:'c'.repeat(64)}});
const frame=id=>({id,request:{account:'baw-russia',operation:'context',itemId:id},traceContext:context(id)});
function harness(run,limits={}){
  const input=new PassThrough(),output=new PassThrough();let text='';
  output.on('data',chunk=>{text+=chunk;if(String(chunk).split('\n').filter(Boolean).some(line=>JSON.parse(line).type==='retiring'))input.end();});
  const done=serveProviderSession({account:'baw-russia',input,output,limits,session:{run,close:async()=>{}}});
  return {input,done,send:value=>input.write(JSON.stringify(value)+'\n'),rows:()=>text.split('\n').filter(Boolean).map(JSON.parse)};
}

test('pool isolates overlapping requests and strips metadata before business dispatch',async()=>{
  const observed=[];let release;const gate=new Promise(resolve=>release=resolve);
  const h=harness(async request=>{
    assert.equal(Object.hasOwn(request,'traceContext'),false);assert.equal(Object.hasOwn(request,'telemetry'),false);
    const recorder=currentTraceRecorder();observed.push([request.itemId,recorder.context.traceId]);
    if(request.itemId==='slow')await gate;else release();
    await new Promise(resolve=>setImmediate(resolve));
    assert.equal(currentTraceRecorder(),recorder);return {itemId:request.itemId,privateText:'business-only'};
  });
  h.send(frame('slow'));h.send(frame('fast'));h.input.end();await h.done;
  const rows=h.rows().filter(row=>row.id);assert.equal(rows[0].id,'fast');assert.equal(rows.length,2);
  assert.deepEqual(observed,[['slow','trace-slow'],['fast','trace-fast']]);
  for(const row of rows){const envelope=validateTelemetryEnvelope(row.telemetry,context(row.id));
    assert.equal(envelope.complete,true);assert.equal(envelope.droppedEventCount,0);
    assert.ok(envelope.events.some(event=>event.eventType==='span_end'&&event.stage==='provider.dispatch'));
    assert.ok(envelope.events.every(event=>event.ids.logicalOperationId==='operation-'+row.id));
    assert.doesNotMatch(JSON.stringify(envelope),/business-only|privateText|itemId/);
  }
  assert.equal(currentTraceRecorder(),null);
});

test('invalid or foreign metadata does not alter work, result, or failure retry count',async()=>{
  let calls=0;const h=harness(async request=>{calls++;assert.equal(currentTraceRecorder(),null);if(request.itemId==='failed')throw Object.assign(new Error('private failure'),{code:'TRANSPORT_ERROR'});return 17;});
  const invalid=frame('invalid');invalid.traceContext.rawComment='private';h.send(invalid);
  const foreign=frame('foreign');foreign.traceContext.companyKey='likeavto';h.send(foreign);
  const failed=frame('failed');failed.traceContext.sourcePin='invalid';h.send(failed);h.input.end();await h.done;
  const rows=h.rows().filter(row=>row.id);assert.equal(calls,3);assert.equal(rows.find(row=>row.id==='invalid').result,17);
  assert.equal(rows.find(row=>row.id==='foreign').result,17);assert.equal(rows.find(row=>row.id==='failed').error.code,'TRANSPORT_ERROR');
  assert.ok(rows.every(row=>!Object.hasOwn(row,'telemetry')));assert.doesNotMatch(JSON.stringify(rows),/private failure/);
});

test('response byte budget drops only observation when business result fits',async()=>{
  const h=harness(async()=>({published:true}),{maxResponseBytes:96});h.send(frame('fits'));h.input.end();await h.done;
  const row=h.rows().find(row=>row.id);assert.equal(row.ok,true);assert.deepEqual(row.result,{published:true});assert.equal(row.telemetry,undefined);
  const oversized=harness(async()=> 'x'.repeat(1000),{maxResponseBytes:96});oversized.send(frame('large'));oversized.input.end();await oversized.done;
  assert.equal(oversized.rows().find(row=>row.id).error.code,'ADAPTER_OUTPUT_LIMIT');
});

test('pool ignores inherited trace environment and carries queue wait as a measured span',async()=>{
  const prior=process.env.COMMUNITYHERO_TRACE_CONTEXT;process.env.COMMUNITYHERO_TRACE_CONTEXT=JSON.stringify(context('inherited'));
  try{
    let release;const gate=new Promise(resolve=>release=resolve);const h=harness(async request=>{if(request.itemId==='first')await gate;return request.itemId;},{maxActive:1});
    h.send(frame('first'));h.send(frame('queued'));await new Promise(resolve=>setImmediate(resolve));release();h.input.end();await h.done;
    const queued=h.rows().find(row=>row.id==='queued');validateTelemetryEnvelope(queued.telemetry,context('queued'));
    const wait=queued.telemetry.events.find(event=>event.stage==='provider.queue.wait'&&event.eventType==='span_end');assert.ok(BigInt(wait.elapsedNs)>0n);
    assert.ok(queued.telemetry.events.every(event=>event.traceId==='trace-queued'));
  }finally{if(prior===undefined)delete process.env.COMMUNITYHERO_TRACE_CONTEXT;else process.env.COMMUNITYHERO_TRACE_CONTEXT=prior;}
});

test('short-child envelope connects nested model spans without changing business objects',async()=>{
  const requested={account:'baw-russia',op:'assistant',text:'private input'},recorder=createTraceRecorder({context:context('short')});
  const envelope=await dispatchEnvelope(requested,{recorder,dispatchFn:async request=>{
    assert.equal(request,requested);const span=currentTraceRecorder().start('model.process');await new Promise(resolve=>setImmediate(resolve));span.finish();return {reply:'private output'};
  }});
  assert.deepEqual(envelope.result,{reply:'private output'});const observed=validateTelemetryEnvelope(envelope.telemetry,context('short'));
  const parent=observed.events.find(event=>event.stage==='provider.request'&&event.eventType==='span_start');
  const child=observed.events.find(event=>event.stage==='model.process'&&event.eventType==='span_end');assert.equal(child.parentSpanId,parent.spanId);
  assert.doesNotMatch(JSON.stringify(observed),/private input|private output/);
});

test('short-child observation is omitted before its byte budget can reject a successful result',async()=>{
  const envelope=await dispatchEnvelope({account:'baw-russia',op:'assistant'},
    {maxResponseBytes:64,recorder:createTraceRecorder({context:context('short-budget')}),dispatchFn:async()=>17});
  assert.deepEqual(envelope,{ok:true,result:17});
});

test('bounded loss and missing completion are explicit; unknown counters remain null',()=>{
  const recorder=createTraceRecorder({context:context('bounded'),maxEvents:2});recorder.start('fixture.stage');recorder.marker('fixture.stage');
  const envelope=recorder.finish();assert.equal(envelope.complete,false);assert.ok(envelope.droppedEventCount>0);assert.ok(envelope.events.length<=2);
  validateTelemetryEnvelope(envelope,context('bounded'));
  const missing=createTraceRecorder({context:context('missing')});missing.missing('model.process');assert.equal(missing.finish().complete,false);
  const counts=createTraceRecorder({context:context('counts')});const span=counts.start('fixture.stage');span.finish({measurements:{inputTokens:null,cloneBytes:12,cloneMeasurementClass:'derived',cloneBasis:'logical_serialized_size'}});
  const event=counts.finish().events.find(event=>event.eventType==='span_end');assert.equal(event.measurements.inputTokens,null);
  assert.throws(()=>validateTraceEvent({...event,measurements:null}));assert.throws(()=>validateTraceEvent({...event,links:null}));
  assert.throws(()=>validateTraceEvent({...event,elapsedNs:(BigInt(event.elapsedNs)+1n).toString()}));
});

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {verifyCausalTrace,readTracePin,parseTraceRecords} from '../cli/trace-verifier.mjs';
import {summarizeCausalTrace} from '../cli/trace-metrics.mjs';
const sha=b=>createHash('sha256').update(b).digest('hex');
function span(id,{traceId='trace1',stage='model.process',start=0n,end=10n,clock='clock1',parent,spanClass='activity',reasonCode,lineage={}}={}){
  const base={version:1,traceId,companyKey:'baw-russia',runtimeId:'runtime1',runtimeEpoch:1,sourcePin:'a'.repeat(64),processClockId:clock,processId:1,wallUtc:'2026-10-06T05:00:00.000Z',spanId:id,parentSpanId:parent,stage,spanClass,measurementClass:'measured',lineage,links:[]};
  return [{...base,eventId:id+'-start',eventType:'span_start',monoNs:start.toString()},
    {...base,eventId:id+'-end',eventType:'span_end',monoNs:end.toString(),startNs:start.toString(),endNs:end.toString(),elapsedNs:(end-start).toString(),outcome:'completed',...(reasonCode?{reasonCode}:{})}];
}
test('closed content-free events reject text payloads and inconsistent measured duration',()=>{
  const rows=span('one');assert.equal(verifyCausalTrace(rows).completedSpans.length,1);
  assert.throws(()=>verifyCausalTrace([{...rows[0],text:'secret model content'},rows[1]]),error=>!error.message.includes('secret'));
  assert.throws(()=>verifyCausalTrace([rows[0],{...rows[1],elapsedNs:'11'}]),/INVALID_CAUSAL_TRACE/);
});
test('nested durations remain separate; incomplete stages and cross-clock alignment stay explicit',()=>{
  const rows=[...span('parent',{stage:'fixture.container',start:0n,end:100n,spanClass:'container'}),...span('child',{start:20n,end:70n,parent:'parent'})];
  const verified=verifyCausalTrace(rows,{requiredStages:['provider.request']});
  assert.equal(verified.completedSpans.length,2);assert.equal(verified.criticalPath.available,false);assert.ok(verified.missing.some(row=>row.stage==='provider.request'));
  const cross=verifyCausalTrace([...span('p',{stage:'fixture.container',spanClass:'container'}),...span('c',{clock:'clock2',parent:'p'})]);
  assert.equal(cross.clockDomains.length,2);assert.ok(cross.missing.some(row=>row.reason==='clock_unaligned'));
});
test('identical replayed event IDs deduplicate and causal/parent cycles fail',()=>{
  const one=span('one');assert.equal(verifyCausalTrace([...one,...one]).completedSpans.length,1);
  assert.throws(()=>verifyCausalTrace([one[0],{...one[0],monoNs:'1'},one[1]]),/INVALID_CAUSAL_TRACE/);
  const a=span('a'),b=span('b');a[1].links=[{type:'depends_on',traceId:'trace1',spanId:'b'}];b[1].links=[{type:'depends_on',traceId:'trace1',spanId:'a'}];
  assert.throws(()=>verifyCausalTrace([...a,...b]),/INVALID_CAUSAL_TRACE/);
  assert.throws(()=>verifyCausalTrace([...span('a',{parent:'b'}),...span('b',{parent:'a'})]),/INVALID_CAUSAL_TRACE/);
});
test('nanosecond strings preserve durations larger than safe JS integer and never join different clocks',()=>{
  const n=9007199254741993n,rows=span('huge',{end:n});assert.equal(verifyCausalTrace(rows).completedSpans[0].elapsedNs,n.toString());
  assert.throws(()=>verifyCausalTrace([rows[0],{...rows[1],processClockId:'other-clock'}]),/INVALID_CAUSAL_TRACE/);
});
test('an orphan span end is reported missing and never enters a measured distribution',()=>{
  const verified=verifyCausalTrace([span('orphan')[1]],{requiredStages:['model.process']});
  assert.equal(verified.completedSpans.length,0);assert.ok(verified.missing.some(row=>row.reason==='missing_span_start'));
  assert.ok(verified.missing.some(row=>row.reason==='not_measured'));
});
test('absent binary/core event lineage is explicit even when source pins exist',()=>{
  const verified=verifyCausalTrace(span('one'),{binarySha256:'b'.repeat(64),coreSha256:'c'.repeat(64)});
  assert.ok(verified.missing.some(row=>row.reason==='binary_binding_not_observed'));assert.ok(verified.missing.some(row=>row.reason==='core_binding_not_observed'));
});
async function fixture(t){const root=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-trace-analytics-'));assert.ok(root.startsWith(path.resolve(os.tmpdir())+path.sep));t.after(()=>fs.rm(root,{recursive:true,force:true}));
  async function put(name,bytes){const file=path.join(root,name);await fs.writeFile(file,bytes);return{path:file,sha256:sha(bytes)};}
  const binary=await put('server.exe','synthetic binary'),sourceManifest=await put('source-manifest.json','{"fixture":true}');
  const core=await put('core.json',JSON.stringify({schemaVersion:1,kind:'company-independent-immutable-core',binary:binary.path,binarySha256:binary.sha256,sourceCheckpoint:sourceManifest}));
  const rows=[];for(let i=1;i<=5;i++)for(const e of span('s'+i,{traceId:'trace'+i,end:BigInt(i)*1000000n,reasonCode:i<=2?'full_fallback':undefined,lineage:{binarySha256:binary.sha256,coreSha256:core.sha256}}))rows.push({...e,sourcePin:sourceManifest.sha256});
  const events=await put('events.jsonl',rows.map(row=>JSON.stringify(row)).join('\n'));
  const input={version:1,kind:'communityhero-causal-trace-input',account:'baw-russia',bindings:{binary,core,sourceManifest},
    workload:{id:'fixture',temperature:'warm',population:'synthetic-tooling',count:5,requiredStages:['model.process','provider.request']},
    host:{id:'fixture-host',osBuild:'test',nodeVersion:'24.15.0',pgVersion:'not-run'},pgSettingsDigest:'b'.repeat(64),events};
  return{root,input,put,pin:await put('input.json',JSON.stringify(input))};}
test('pinned analytics reports exact nearest-rank N/p50/p95/max, fallback share, cohort and missing coverage',async t=>{
  const f=await fixture(t), result=await summarizeCausalTrace({repo:f.root,receiptPin:f.pin});
  assert.equal(result.stages['model.process'].N,5);assert.equal(result.stages['model.process'].p50Ns,'3000000');assert.equal(result.stages['model.process'].p95Ns,'5000000');
  assert.equal(result.stages['model.process'].p50Ms,'3.000000');assert.equal(result.stages['model.process'].fallbackShare,null);
  assert.equal(result.stages['model.process'].observedFallbackSpanShare,.4);
  assert.equal(result.stages['model.process'].fallbackUnclassifiedSpans,3);
  assert.equal(result.workload.temperature,'warm');assert.equal(result.missing.length,5);assert.equal(result.telemetryComplete,false);
  assert.equal(result.totals.available,false);assert.equal(result.criticalPath.available,false);
});

test('source fallback reasons count once per measured span; missing markers remain unknown',async t=>{
  const f=await fixture(t),lineage={binarySha256:f.input.bindings.binary.sha256,coreSha256:f.input.bindings.core.sha256};
  const reasons=['full_fallback','source_scope_legacy_fallback','source_scope_ambiguous_research'];
  const rows=reasons.flatMap((reason,i)=>{
    const events=span('fallback'+i,{traceId:'fallback-trace'+i,lineage});
    events[1].measurements={fallbackReason:reason};
    if(i===0)events[1].reasonCode=reason;
    return events;
  }).map(e=>({...e,sourcePin:f.input.bindings.sourceManifest.sha256}));
  const summarize=async(events,name)=>{
    const pin=await f.put(name+'.json',JSON.stringify({...f.input,events:await f.put(name+'.jsonl',events.map(e=>JSON.stringify(e)).join('\n'))}));
    return (await summarizeCausalTrace({repo:f.root,receiptPin:pin})).stages['model.process'];
  };
  const complete=await summarize(rows,'complete-fallback');
  assert.equal(complete.observedFallbackSpans,3);assert.equal(complete.fallbackShare,1);
  const absent=span('absent',{traceId:'absent-trace',lineage}).map(e=>({...e,sourcePin:f.input.bindings.sourceManifest.sha256}));
  absent[1].measurements={fallbackReason:null};
  const unknown=await summarize([...rows,...absent],'unknown-fallback');
  assert.equal(unknown.fallbackShare,null);assert.equal(unknown.observedFallbackSpanShare,.75);
  assert.equal(unknown.fallbackUnclassifiedSpans,1);assert.equal(unknown.fallbackAbsenceReason,'fallback_absence_not_measured');
});
test('pinned metadata rejects altered artifact, wrong company or unexpected secret-bearing input fields',async t=>{
  const f=await fixture(t);await fs.writeFile(f.input.bindings.binary.path,'changed');
  await assert.rejects(summarizeCausalTrace({repo:f.root,receiptPin:f.pin}),/INVALID_CAUSAL_TRACE/);
  await fs.writeFile(f.input.bindings.binary.path,'synthetic binary');
  for(const value of [{...f.input,account:'likeavto'},{...f.input,password:'do not echo'},{...f.input,bindings:{...f.input.bindings,binary:{...f.input.bindings.binary,password:'do not echo'}}}]){
    const pin=await f.put('bad-input.json',JSON.stringify(value));await assert.rejects(summarizeCausalTrace({repo:f.root,receiptPin:pin}),e=>!e.message.includes('do not echo'));
  }
});
test('analytics parser and pins reject malformed JSONL, traversal and credential aliases without echoing content',async t=>{
  const f=await fixture(t);assert.throws(()=>parseTraceRecords(Buffer.from('{"password":"secret')),/INVALID_CAUSAL_TRACE/);
  for(const name of ['../outside.json','credentials/file.json','auth.json','example.txt:stream'])await assert.rejects(readTracePin({path:name,sha256:'a'.repeat(64)},{base:f.root}),/INVALID_CAUSAL_TRACE/);
});
test('analytics refuses a separately pinned but unrelated whole core/source checkpoint',async t=>{
  const f=await fixture(t),core=await f.put('unrelated-core.json',JSON.stringify({schemaVersion:1,kind:'company-independent-immutable-core',binary:f.input.bindings.binary.path,binarySha256:f.input.bindings.binary.sha256,sourceCheckpoint:{...f.input.bindings.sourceManifest,sha256:'c'.repeat(64)}}));
  const pin=await f.put('unrelated-input.json',JSON.stringify({...f.input,bindings:{...f.input.bindings,core}}));
  await assert.rejects(summarizeCausalTrace({repo:f.root,receiptPin:pin}),/INVALID_TRACE_METRICS_INPUT/);
});

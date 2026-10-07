// Read-only analytics over existing closed metadata. No business authority.
import fs from 'node:fs/promises';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { validateTraceEvent, validateTelemetryEnvelope, sameTraceBinding } from './trace-contract.mjs';
export const TRACE_ANALYTICS_LIMITS=Object.freeze({maxBytes:32*1024*1024,maxEvents:32768});
const fail=()=>{throw new TypeError('INVALID_CAUSAL_TRACE');};
const digest=bytes=>createHash('sha256').update(bytes).digest('hex');
export async function readTracePin(pin,{base=process.cwd(),maxBytes=TRACE_ANALYTICS_LIMITS.maxBytes}={}) {
  if(!pin||typeof pin.path!=='string'||!/^[a-f0-9]{64}$/.test(pin.sha256??''))fail();
  if(Object.keys(pin).some(key=>!['path','sha256','bytes'].includes(key))||pin.bytes!==undefined&&(!Number.isSafeInteger(pin.bytes)||pin.bytes<0))fail();
  const parts=pin.path.replaceAll('\\','/').split('/');
  if(parts.some((part,i)=>part==='..'||/[\u0000-\u001f\u007f]/.test(part)||/[ .]$/.test(part)||(part.includes(':')&&!(i===0&&/^[a-z]:$/i.test(part)))||/^(?:private|secrets|credentials|credentials\.toml|auth\.json|operators\.json|owner-access-code\.txt|\.env(?:\..*)?)$/i.test(part)))fail();
  const file=path.resolve(base,pin.path);let component=path.parse(file).root;
  for(const part of file.slice(component.length).split(path.sep).filter(Boolean)){component=path.join(component,part);if((await fs.lstat(component)).isSymbolicLink())fail();}
  const stat=await fs.stat(file);if(!stat.isFile()||stat.size>maxBytes)fail();
  const bytes=await fs.readFile(file);if(bytes.length>maxBytes||digest(bytes)!==pin.sha256||pin.bytes!==undefined&&bytes.length!==pin.bytes)fail();
  return {file,bytes};
}
export function parseTraceRecords(bytes) {
  if(!Buffer.isBuffer(bytes)||bytes.length>TRACE_ANALYTICS_LIMITS.maxBytes)fail();
  try{return bytes.toString('utf8').split(/\r?\n/).filter(line=>line.trim()).map(line=>JSON.parse(line));}catch{fail();}
}
const spanKey=e=>`${e.traceId}/${e.spanId}`;
export function verifyCausalTrace(records,{companyKey,sourcePin,binarySha256,coreSha256,requiredStages=[]}={}) {
  if(!Array.isArray(records)||records.length>TRACE_ANALYTICS_LIMITS.maxEvents)fail();
  const events=[],seen=new Map(),envelopes=[];
  for(const record of records){
    let list;try{list=record?.events?validateTelemetryEnvelope(record).events:[validateTraceEvent(record)];}catch{fail();}
    if(record?.events)envelopes.push({complete:record.complete,droppedEventCount:record.droppedEventCount});
    for(const e of list){
      if(e.spanId&&e.parentSpanId===e.spanId)fail();
      if(companyKey&&e.companyKey!==companyKey||sourcePin&&e.sourcePin!==sourcePin||binarySha256&&e.lineage?.binarySha256!=null&&e.lineage.binarySha256!==binarySha256||coreSha256&&e.lineage?.coreSha256!=null&&e.lineage.coreSha256!==coreSha256)fail();
      const bytes=JSON.stringify(e);if(seen.has(e.eventId)){if(seen.get(e.eventId)!==bytes)fail();continue;}seen.set(e.eventId,bytes);events.push(e);
      if(events.length>TRACE_ANALYTICS_LIMITS.maxEvents)fail();
    }
  }
  const starts=new Map(),ends=new Map(),paired=new Set(),missing=[],clockDomains=new Set(),traces=new Set();
  for(const e of events){
    clockDomains.add(e.processClockId);traces.add(e.traceId);
    if(binarySha256&&e.lineage?.binarySha256==null)missing.push({traceId:e.traceId,stage:e.stage,reason:'binary_binding_not_observed'});
    if(coreSha256&&e.lineage?.coreSha256==null)missing.push({traceId:e.traceId,stage:e.stage,reason:'core_binding_not_observed'});
    if(e.eventType==='span_start'){if(starts.has(spanKey(e)))fail();starts.set(spanKey(e),e);}
    if(e.eventType==='span_end'){
      if(ends.has(spanKey(e)))fail();
      const start=BigInt(e.startNs),end=BigInt(e.endNs);if(end<start||BigInt(e.elapsedNs)!==end-start||BigInt(e.monoNs)<end)fail();ends.set(spanKey(e),e);
    }
    if(e.eventType==='missing')missing.push({traceId:e.traceId,stage:e.stage,reason:e.absenceReason??'not_measured'});
  }
  for(const [key,end] of ends){
    const start=starts.get(key);if(!start){missing.push({traceId:end.traceId,stage:end.stage,reason:'missing_span_start'});continue;}
    if(!sameTraceBinding(start,end)||start.processClockId!==end.processClockId||start.processId!==end.processId||start.stage!==end.stage||start.spanClass!==end.spanClass||start.parentSpanId!==end.parentSpanId||start.monoNs!==end.startNs)fail();
    paired.add(key);
    if(end.parentSpanId){const parent=ends.get(`${end.traceId}/${end.parentSpanId}`);if(!parent)missing.push({traceId:end.traceId,stage:end.stage,reason:'parent_not_observed'});
      else if(parent.processClockId!==end.processClockId)missing.push({traceId:end.traceId,stage:end.stage,reason:'clock_unaligned'});
      else if(BigInt(end.startNs)<BigInt(parent.startNs)||BigInt(end.endNs)>BigInt(parent.endNs))fail();}
  }
  for(const [key,start] of starts)if(!ends.has(key))missing.push({traceId:start.traceId,stage:start.stage,reason:'unfinished_span'});
  const dependencies=new Map([...ends.keys()].map(key=>[key,new Set()]));
  for(const [key,end] of ends)for(const link of end.links??[]){
    if(!['depends_on','fan_in','fan_out'].includes(link.type)||!link.spanId)continue;
    const target=`${link.traceId}/${link.spanId}`;
    if(!ends.has(target)){missing.push({traceId:end.traceId,stage:end.stage,reason:'dependency_not_observed'});continue;}
    if(link.type==='fan_out')dependencies.get(target).add(key);else dependencies.get(key).add(target);
  }
  const visiting=new Set(),visited=new Set();function visit(key){if(visiting.has(key))fail();if(visited.has(key))return;visiting.add(key);for(const parent of dependencies.get(key)??[])visit(parent);visiting.delete(key);visited.add(key);}for(const key of dependencies.keys())visit(key);
  for(const [key,end] of ends){const chain=new Set([key]);let parent=end.parentSpanId?`${end.traceId}/${end.parentSpanId}`:null;while(parent&&ends.has(parent)){if(chain.has(parent))fail();chain.add(parent);const e=ends.get(parent);parent=e.parentSpanId?`${e.traceId}/${e.parentSpanId}`:null;}}
  for(const traceId of traces)for(const stage of requiredStages)if(!events.some(e=>e.traceId===traceId&&e.stage===stage&&(e.eventType==='marker'||e.eventType==='span_end'&&paired.has(spanKey(e)))))missing.push({traceId,stage,reason:'not_measured'});
  const droppedEvents=envelopes.reduce((n,e)=>n+e.droppedEventCount,0)+events.filter(e=>e.eventType==='summary').reduce((n,e)=>n+(e.droppedEventCount??0),0);
  if(!Number.isSafeInteger(droppedEvents))fail();
  return {status:'verified',events,completedSpans:[...ends].filter(([key])=>paired.has(key)).map(([,end])=>end),traceCount:traces.size,clockDomains:[...clockDomains],missing,droppedEvents,
    telemetryComplete:envelopes.length>0&&envelopes.every(e=>e.complete)&&missing.length===0&&droppedEvents===0,
    criticalPath:{available:false,reason:'complete_dependency_and_clock_alignment_not_proven'}};
}

// Canonical, closed, content-free TraceEnvelope.v1. Telemetry grants no authority.
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
export const TRACE_SCHEMA_URL=new URL('./trace-envelope-v1.schema.json',import.meta.url);
const schemaBytes=readFileSync(TRACE_SCHEMA_URL);
export const TRACE_CONTRACT=Object.freeze(JSON.parse(schemaBytes));
export const TRACE_CONTRACT_SHA256=createHash('sha256').update(schemaBytes).digest('hex');
export const TRACE_LIMITS=Object.freeze({maxEvents:256,maxEnvelopeBytes:262144,maxEventBytes:8192,maxContextBytes:4096,maxLinks:32});
const enums=name=>new Set(TRACE_CONTRACT[`x-${name}`]);
const stages=enums('stages'),reasons=enums('reasons'),outcomes=enums('outcomes'),eventTypes=enums('eventTypes'),classes=enums('spanClasses'),linkTypes=enums('linkTypes');
const idKeys=new Set(['jobId','logicalOperationId','dispatchAttemptId','readbackAttemptId','paidAttemptId','proposalId','approvalId','groupId']);
const digestIds=new Set(['localRequestDigest']);
const lineageIds=new Set(['contextVersion','groupVersion']);
const lineageDigests=new Set(['bindingDigest','groupScopeDigest','lineageManifestDigest','bundleDigest','rulesDigest','mediaDigest','binarySha256','coreSha256']);
const numericMeasurements=new Set(TRACE_CONTRACT['x-numericMeasurements']);
const readClasses=new Set(['payloadRead','headerRead','controlRead','discriminatorRead','payloadWrite']);
const boolMeasurements=new Set(['committed','rollbackAcknowledged','connectionReturned','providerCallAttempted']);
const eventKeys=new Set(['version','eventId','eventType','traceId','companyKey','runtimeId','runtimeEpoch','sourcePin','processClockId','processId','monoNs','wallUtc','spanId','parentSpanId','stage','spanClass','startNs','endNs','elapsedNs','links','ids','lineage','measurements','outcome','reasonCode','measurementClass','absenceReason','droppedEventCount','clockUncertaintyNs']);
const contextKeys=new Set(['version','traceId','companyKey','runtimeId','runtimeEpoch','sourcePin','parentSpanId','ids','lineage']);
const fail=()=>{throw new TypeError('INVALID_TRACE_CONTRACT');};
export const isOpaqueId=value=>typeof value==='string'&&/^[A-Za-z0-9_:-]{1,128}$/u.test(value);
export const isDigest=value=>typeof value==='string'&&/^[a-f0-9]{64}$/u.test(value);
export const isNs=value=>typeof value==='string'&&/^(0|[1-9][0-9]{0,29})$/u.test(value);
const plain=value=>value&&typeof value==='object'&&!Array.isArray(value)&&[Object.prototype,null].includes(Object.getPrototypeOf(value));
const keys=(value,allowed)=>{if(!plain(value)||Object.keys(value).some(k=>!allowed.has(k)))fail();};
const count=value=>Number.isSafeInteger(value)&&value>=0;
const optional=(value,test)=>value===undefined||value===null||test(value);
const wall=value=>typeof value==='string'&&/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/u.test(value)&&Number.isFinite(Date.parse(value));
export const isTraceStage=value=>stages.has(value);
export const isTraceReason=value=>reasons.has(value);
export const isTraceOutcome=value=>outcomes.has(value);
function identities(value={}) {
  keys(value,new Set([...idKeys,...digestIds,'proposalRevision']));
  for(const [key,v] of Object.entries(value))if(!optional(v,key==='proposalRevision'?count:digestIds.has(key)?isDigest:isOpaqueId))fail();
}
function lineage(value={}) {
  keys(value,new Set([...lineageIds,...lineageDigests]));
  for(const [key,v] of Object.entries(value))if(!optional(v,lineageDigests.has(key)?isDigest:isOpaqueId))fail();
}
export function validateTraceContext(value) {
  keys(value,contextKeys);
  if(value.version!==1||!isOpaqueId(value.traceId)||!['baw-russia','likeavto'].includes(value.companyKey)||!isOpaqueId(value.runtimeId)||!count(value.runtimeEpoch)||!isDigest(value.sourcePin)||!optional(value.parentSpanId,isOpaqueId))fail();
  identities(value.ids);lineage(value.lineage);
  if(Buffer.byteLength(JSON.stringify(value))>TRACE_LIMITS.maxContextBytes)fail();
  return structuredClone(value);
}
export function validateMeasurements(value={}) {
  keys(value,new Set([...numericMeasurements,...readClasses,...boolMeasurements,'cloneMeasurementClass','cloneBasis','fallbackReason']));
  for(const [key,v] of Object.entries(value)) {
    if(readClasses.has(key)) {keys(v,new Set(['rows','bytes','statements']));for(const n of Object.values(v))if(!optional(n,count))fail();}
    else if(numericMeasurements.has(key)) {if(!optional(v,count))fail();}
    else if(boolMeasurements.has(key)) {if(!optional(v,x=>typeof x==='boolean'))fail();}
    else if(key==='fallbackReason') {if(!optional(v,isTraceReason))fail();}
    else if(key==='cloneMeasurementClass'&&v!=='derived'||key==='cloneBasis'&&v!=='logical_serialized_size')fail();
  }
  if(value.cloneBytes!=null&&(value.cloneMeasurementClass!=='derived'||value.cloneBasis!=='logical_serialized_size'))fail();
  return structuredClone(value);
}
export function validateTraceEvent(value) {
  keys(value,eventKeys);
  validateTraceContext({version:value.version,traceId:value.traceId,companyKey:value.companyKey,runtimeId:value.runtimeId,runtimeEpoch:value.runtimeEpoch,sourcePin:value.sourcePin,ids:value.ids,lineage:value.lineage});
  if(!isOpaqueId(value.eventId)||!eventTypes.has(value.eventType)||!isOpaqueId(value.processClockId)||!count(value.processId)||!isNs(value.monoNs)||!wall(value.wallUtc)||!stages.has(value.stage)||!classes.has(value.spanClass)||!['measured','derived','historical','unknown'].includes(value.measurementClass))fail();
  for(const key of ['spanId','parentSpanId'])if(!optional(value[key],isOpaqueId))fail();
  for(const key of ['startNs','endNs','elapsedNs','clockUncertaintyNs'])if(!optional(value[key],isNs))fail();
  if(!optional(value.outcome,isTraceOutcome)||!optional(value.reasonCode,isTraceReason)||!optional(value.absenceReason,isTraceReason)||!optional(value.droppedEventCount,count))fail();
  const links=value.links===undefined?[]:value.links;if(!Array.isArray(links)||links.length>TRACE_LIMITS.maxLinks)fail();
  for(const link of links){keys(link,new Set(['type','traceId','spanId','durableId']));if(!linkTypes.has(link.type)||!isOpaqueId(link.traceId)||!optional(link.spanId,isOpaqueId)||!optional(link.durableId,isOpaqueId)||!link.spanId&&!link.durableId)fail();}
  validateMeasurements(value.measurements);
  if(['span_start','span_end'].includes(value.eventType)&&!isOpaqueId(value.spanId))fail();
  if(value.eventType==='span_end'&&(!isNs(value.startNs)||!isNs(value.endNs)||!isNs(value.elapsedNs)||!isTraceOutcome(value.outcome)))fail();
  if(value.eventType==='span_end'&&(BigInt(value.endNs)<BigInt(value.startNs)||BigInt(value.endNs)-BigInt(value.startNs)!==BigInt(value.elapsedNs)||value.monoNs!==value.endNs))fail();
  if(Buffer.byteLength(JSON.stringify(value))>TRACE_LIMITS.maxEventBytes)fail();
  return structuredClone(value);
}
export function sameTraceBinding(a,b,{runtime=true}={}) {
  if(a.traceId!==b.traceId||a.companyKey!==b.companyKey||a.sourcePin!==b.sourcePin)return false;
  if(runtime&&(a.runtimeId!==b.runtimeId||a.runtimeEpoch!==b.runtimeEpoch))return false;
  for(const [key,value] of Object.entries(b.ids??{}))if(value!=null&&a.ids?.[key]!==value)return false;
  for(const [key,value] of Object.entries(b.lineage??{}))if(value!=null&&a.lineage?.[key]!==value)return false;
  return true;
}
export function validateTelemetryEnvelope(value,expected) {
  keys(value,new Set(['version','context','events','droppedEventCount','complete']));
  const context=validateTraceContext(value.context);
  if(value.version!==1||!Array.isArray(value.events)||value.events.length>TRACE_LIMITS.maxEvents||!count(value.droppedEventCount)||typeof value.complete!=='boolean'||Buffer.byteLength(JSON.stringify(value))>TRACE_LIMITS.maxEnvelopeBytes)fail();
  if(expected&&(!sameTraceBinding(context,validateTraceContext(expected))||(context.parentSpanId??null)!==(expected.parentSpanId??null)))fail();
  const events=value.events.map(validateTraceEvent);
  if(events.some(e=>!sameTraceBinding(e,context)||e.parentSpanId!=null&&e.parentSpanId===e.spanId))fail();
  return {version:1,context,events,droppedEventCount:value.droppedEventCount,complete:value.complete};
}

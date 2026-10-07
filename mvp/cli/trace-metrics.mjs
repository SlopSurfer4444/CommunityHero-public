import { TRACE_CONTRACT_SHA256, isTraceStage } from './trace-contract.mjs';
import { readTracePin, parseTraceRecords, verifyCausalTrace } from './trace-verifier.mjs';
import path from 'node:path';
const fail=()=>{throw new TypeError('INVALID_TRACE_METRICS_INPUT');};
const exact=(o,names)=>o&&typeof o==='object'&&!Array.isArray(o)&&Object.keys(o).length===names.length&&names.every(n=>Object.hasOwn(o,n));
const scalar=v=>typeof v==='string'&&/^[A-Za-z0-9_.:-]{1,128}$/.test(v);
const ms=ns=>`${ns/1000000n}.${(ns%1000000n).toString().padStart(6,'0')}`;
const fallbackReasons=new Set(['full_fallback','source_scope_legacy_fallback','source_scope_ambiguous_research']);
function fallbackObservations(rows){
  // A missing/null marker does not prove that no fallback occurred. Report the
  // observed lower bound separately from a completely classified population.
  const observed=rows.filter(e=>fallbackReasons.has(e.reasonCode)||fallbackReasons.has(e.measurements?.fallbackReason)).length;
  return {fallbackShare:rows.length&&observed===rows.length?1:null,
    observedFallbackSpanShare:rows.length?observed/rows.length:null,
    observedFallbackSpans:observed,fallbackUnclassifiedSpans:rows.length-observed,
    fallbackPopulation:'completed_measured_spans_of_this_stage',
    fallbackAbsenceReason:rows.length&&observed===rows.length?null:'fallback_absence_not_measured'};
}
function distribution(values){values=values.sort((a,b)=>a<b?-1:a>b?1:0);const pick=p=>values[Math.max(0,Math.ceil(values.length*p)-1)];if(!values.length)return{N:0,p50Ns:null,p95Ns:null,maxNs:null,p50Ms:null,p95Ms:null,maxMs:null};const a=pick(.5),b=pick(.95),c=values.at(-1);return{N:values.length,p50Ns:a.toString(),p95Ns:b.toString(),maxNs:c.toString(),p50Ms:ms(a),p95Ms:ms(b),maxMs:ms(c)};}
export async function summarizeCausalTrace({receiptPin,repo=process.cwd()}) {
  const {bytes}=await readTracePin(receiptPin,{base:repo});let input;try{input=JSON.parse(bytes.toString('utf8'));}catch{fail();}
  if(!exact(input,['version','kind','account','bindings','workload','host','pgSettingsDigest','events'])||input.version!==1||input.kind!=='communityhero-causal-trace-input'||input.account!=='baw-russia')fail();
  if(!exact(input.bindings,['binary','core','sourceManifest'])||!exact(input.workload,['id','temperature','population','count','requiredStages'])||!['cold','warm'].includes(input.workload.temperature)||!scalar(input.workload.id)||!scalar(input.workload.population)||!Number.isSafeInteger(input.workload.count)||input.workload.count<0||!Array.isArray(input.workload.requiredStages)||input.workload.requiredStages.some(s=>!isTraceStage(s)))fail();
  if(!exact(input.host,['id','osBuild','nodeVersion','pgVersion'])||Object.values(input.host).some(v=>!scalar(v))||!/^[a-f0-9]{64}$/.test(input.pgSettingsDigest))fail();
  const artifacts={};for(const [name,pin] of Object.entries(input.bindings))artifacts[name]=await readTracePin(pin,{base:repo,maxBytes:256*1024*1024});
  let core;try{core=JSON.parse(artifacts.core.bytes.toString('utf8'));}catch{fail();}
  if(core.schemaVersion!==1||core.kind!=='company-independent-immutable-core'||core.binarySha256!==input.bindings.binary.sha256||
    typeof core.binary!=='string'||path.resolve(repo,core.binary).toLowerCase()!==artifacts.binary.file.toLowerCase()||
    core.sourceCheckpoint?.sha256!==input.bindings.sourceManifest.sha256||typeof core.sourceCheckpoint?.path!=='string'||
    path.resolve(repo,core.sourceCheckpoint.path).toLowerCase()!==artifacts.sourceManifest.file.toLowerCase())fail();
  const {bytes:records}=await readTracePin(input.events,{base:repo});
  const verified=verifyCausalTrace(parseTraceRecords(records),{companyKey:input.account,sourcePin:input.bindings.sourceManifest.sha256,binarySha256:input.bindings.binary.sha256,coreSha256:input.bindings.core.sha256,requiredStages:input.workload.requiredStages});
  const stages={};for(const stage of [...new Set(verified.completedSpans.map(e=>e.stage))].sort()){
    const rows=verified.completedSpans.filter(e=>e.stage===stage),measured=rows.filter(e=>e.measurementClass==='measured');
    stages[stage]={...distribution(measured.map(e=>BigInt(e.elapsedNs))),spanClasses:[...new Set(rows.map(e=>e.spanClass))],excludedNonMeasured:rows.length-measured.length,
      population:'completed_measured_spans_of_this_stage',bySpanClass:Object.fromEntries([...new Set(measured.map(e=>e.spanClass))].sort().map(kind=>[kind,distribution(measured.filter(e=>e.spanClass===kind).map(e=>BigInt(e.elapsedNs)))])),
      ...fallbackObservations(measured)};
  }
  return {status:'summarized',scope:'read-only existing telemetry; native end-to-end coverage not asserted',contractSha256:TRACE_CONTRACT_SHA256,bindings:input.bindings,
    workload:input.workload,host:input.host,pgSettingsDigest:input.pgSettingsDigest,traceCount:verified.traceCount,clockDomains:verified.clockDomains.length,
    stages,missing:verified.missing,droppedEvents:verified.droppedEvents,telemetryComplete:verified.telemetryComplete,criticalPath:verified.criticalPath,
    exclusiveDurations:{available:false,reason:'aligned_complete_child_union_not_proven'},comparison:{available:false,reason:'single_explicit_cohort_only'},
    commentThroughput:{available:false,reason:'workload_to_completed_comment_mapping_not_proven'},
    totals:{available:false,reason:'nested_and_overlapping_intervals_are_not_additive'}};
}

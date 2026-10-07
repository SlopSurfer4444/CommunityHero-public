import {AsyncLocalStorage} from 'node:async_hooks';
import {randomUUID} from 'node:crypto';
import {TRACE_LIMITS,validateTraceContext,validateTraceEvent,validateMeasurements,sameTraceBinding,isTraceOutcome,isTraceReason} from './trace-contract.mjs';
const scope=new AsyncLocalStorage();
export const currentTraceRecorder=()=>scope.getStore()?.recorder??null;
export function withTraceRecorder(recorder,work){return recorder?scope.run({recorder,parentSpanId:recorder.context.parentSpanId??null},work):work();}
export function traceRecorderFromEnv(env=process.env) {
  try{return env.COMMUNITYHERO_TRACE_CONTEXT?createTraceRecorder({context:JSON.parse(env.COMMUNITYHERO_TRACE_CONTEXT)}):null;}catch{return null;}
}
/** Bounded local observation. No business work/retry or raw data in this recorder. */
export function createTraceRecorder({context,nowNs=()=>process.hrtime.bigint(),wallNow=()=>new Date(),emit,maxEvents=TRACE_LIMITS.maxEvents}={}) {
  const bound=validateTraceContext(context);Object.freeze(bound.ids);Object.freeze(bound.lineage);Object.freeze(bound);
  if(!Number.isSafeInteger(maxEvents)||maxEvents<1||maxEvents>TRACE_LIMITS.maxEvents)throw new TypeError('INVALID_TRACE_LIMIT');
  const clockId=randomUUID(),events=[],open=new Map();let droppedEventCount=0,bytes=0,closed=false,incomplete=false;
  const tick=()=>{const n=nowNs();if(typeof n!=='bigint'||n<0n)throw new TypeError('INVALID_TRACE_CLOCK');return n;};
  const record=(fields,at)=>{
    try{
      if(closed)return null;
      const time=at??tick(),event=validateTraceEvent({...bound,eventId:randomUUID(),eventType:'marker',stage:'trace.marker',spanClass:'milestone',measurementClass:'measured',processClockId:clockId,processId:process.pid,monoNs:time.toString(),wallUtc:wallNow().toISOString(),links:[],...fields});
      if(!sameTraceBinding(event,bound))throw new TypeError('INVALID_TRACE_BINDING');
      const size=Buffer.byteLength(JSON.stringify(event));
      if(events.length>=maxEvents||bytes+size>TRACE_LIMITS.maxEnvelopeBytes-8192){droppedEventCount++;return null;}
      events.push(event);bytes+=size;if(emit)try{emit(structuredClone(event));}catch{droppedEventCount++;}return event;
    }catch{droppedEventCount++;return null;}
  };
  const recorder={context:bound,
    start(stage,{spanClass='activity',parentSpanId,links=[],ids=bound.ids??{},lineage=bound.lineage??{},measurements={}}={}) {
      let start;try{start=tick();}catch{droppedEventCount++;return disabledSpan();}
      const spanId=randomUUID(),parent=parentSpanId??scope.getStore()?.parentSpanId??bound.parentSpanId??null;
      const fields={stage,spanClass,spanId,parentSpanId:parent,ids,lineage,links,measurements};
      const admitted=record({...fields,eventType:'span_start'},start);if(!admitted)return disabledSpan();
      open.set(spanId,fields);let finished=false;const childContext={...bound,parentSpanId:spanId,ids,lineage};
      return {context:childContext,spanId,
        finish({outcome='completed',reasonCode,measurements:finalMeasurements=measurements}={}) {
          if(finished)return false;finished=true;open.delete(spanId);
          try{const end=tick();if(end<start||!isTraceOutcome(outcome)||reasonCode!==undefined&&!isTraceReason(reasonCode))throw new TypeError('INVALID_TRACE_FINISH');
            record({...fields,eventType:'span_end',outcome,...(reasonCode?{reasonCode}:{}),measurements:validateMeasurements(finalMeasurements),startNs:start.toString(),endNs:end.toString(),elapsedNs:(end-start).toString()},end);return true;
          }catch{droppedEventCount++;return false;}
        },
        scope(work){return scope.run({recorder,parentSpanId:spanId},work);}
      };
    },
    marker(stage,fields={}){return record({...fields,stage,eventType:'marker',spanClass:'milestone'});},
    missing(stage,absenceReason='not_measured'){incomplete=true;return record({stage,eventType:'missing',spanClass:'milestone',absenceReason,measurementClass:'unknown'});},
    finish(){if(!closed){if(open.size)incomplete=true;for(const [spanId,fields] of open)record({...fields,eventType:'missing',absenceReason:'unfinished_span',measurementClass:'unknown'});open.clear();closed=true;}return recorder.snapshot();},
    snapshot(){return {version:1,context:structuredClone(bound),events:structuredClone(events),droppedEventCount,complete:closed&&!incomplete&&droppedEventCount===0};}
  };
  record({eventType:'clock_anchor',stage:'trace.clock_anchor',spanClass:'milestone',clockUncertaintyNs:null});return recorder;
}
function disabledSpan(){return {context:null,spanId:null,finish(){return false;},scope(work){return work();}};}

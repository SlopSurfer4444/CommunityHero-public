import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantTimingObservation,prepareAssistantRequest,admitSinglePassResult,singlePassMetadata,generationMetadata} from './assistant.mjs';

function clock(){let at=1000;const observation=assistantTimingObservation({now:()=>at});
  return {observation,at:(elapsed,event)=>{at=1000+elapsed;if(event)observation.observe(event);}};}
const tool=(type,id,action='search')=>({type,item:{id,type:'web_search',action:{type:action},query:'PRIVATE_QUERY',args:'PRIVATE_ARGS'}});

test('local arrival intervals correlate exact tool IDs without exposing them or claiming remote latency',()=>{
  const {observation,at}=clock();
  at(2,{type:'turn.started',thread_id:'PRIVATE_THREAD'});
  at(10,tool('item.started','PRIVATE_TOOL'));
  at(45,tool('item.completed','PRIVATE_TOOL','open_page'));
  at(70,{type:'item.completed',item:{type:'agent_message',id:'PRIVATE_MESSAGE',text:'PRIVATE_TEXT'}});
  at(72,{type:'turn.completed',usage:{input_tokens:100,output_tokens:200},access_token:'PRIVATE_TOKEN'});
  at(75);const result=observation.finish();
  assert.equal(result.basis,'local_event_arrival');assert.equal(result.scope,'initial_generation_only');
  assert.equal(result.elapsedMs,75);assert.equal(result.firstEventAtMs,2);
  assert.equal(result.lastAgentMessageCompletedAtMs,70);assert.equal(result.turnCompletedAtMs,72);
  assert.equal(result.postToolTailMs,27);assert.equal(result.pairedToolDurationSumMs,35);assert.equal(result.toolObservedUnionMs,35);
  assert.deepEqual(result.records,[{ordinal:1,kind:'web_search',action:'open_page',startedAtMs:10,completedAtMs:45,durationMs:35,status:'completed'}]);
  assert.equal(result.completeTrace,true);assert.doesNotMatch(JSON.stringify(result),/PRIVATE|tokens|https?:|args|thread_id|"id"/);
});

test('parallel tools use independent pairs and interval union, never a serial sum as wall time',()=>{
  const {observation,at}=clock();at(10,tool('item.started','a'));at(20,tool('item.started','b'));
  at(50,tool('item.completed','a'));at(70,tool('item.completed','b'));at(80,{type:'turn.completed'});
  const result=observation.finish();assert.deepEqual(result.records.map(row=>row.durationMs),[40,50]);
  assert.equal(result.pairedToolDurationSumMs,90);assert.equal(result.toolObservedUnionMs,60);assert.equal(result.postToolTailMs,10);
});

test('missing, unfinished and reversed pairs remain explicit and cannot manufacture a final tail',()=>{
  const {observation,at}=clock();at(10,tool('item.completed','only-completed'));
  at(20,tool('item.started','only-started'));at(25,tool('item.completed','reversed'));
  at(30,tool('item.started','reversed'));at(40,{type:'turn.completed'});
  const result=observation.finish();assert.deepEqual(result.records.map(row=>row.status),['unpaired_completion','unfinished','out_of_order']);
  assert.ok(result.records.every(row=>row.durationMs===null));assert.equal(result.postToolTailMs,null);assert.equal(result.completeTrace,false);
  assert.equal(result.toolObservedUnionMs,0);assert.equal(result.pairedToolDurationSumMs,0);
});

test('duplicates do not change the original measured interval; finish is immutable',()=>{
  const {observation,at}=clock();at(10,tool('item.started','a'));at(11,tool('item.started','a'));
  at(20,tool('item.completed','a','open_page'));at(30,tool('item.completed','a','search'));at(40,{type:'turn.completed'});
  const first=observation.finish();assert.equal(first.duplicateEventCount,2);assert.equal(first.records[0].durationMs,10);
  assert.equal(first.completeTrace,false);assert.equal(first.postToolTailMs,null,'Repeated ID is ambiguous; no final-tail claim');
  assert.equal(first.records[0].action,'open_page');first.records[0].durationMs=999;
  at(90,tool('item.started','b'));assert.equal(observation.finish().records[0].durationMs,10);assert.equal(observation.finish().elapsedMs,40);
});

test('tool storage is bounded, overflow is counted as events and never becomes an admission cap',()=>{
  const {observation,at}=clock();
  for(let n=0;n<520;n++){at(n*2,tool('item.started',`secret-${n}`));at(n*2+1,tool('item.completed',`secret-${n}`));}
  at(1100,{type:'turn.completed'});const result=observation.finish();
  assert.equal(result.records.length,512);assert.equal(result.capturedToolCount,512);assert.equal(result.overflowEventCount,16);
  assert.equal(result.toolEventCount,1040);assert.equal(result.recordsTruncated,true);assert.equal(result.completeTrace,false);
  assert.equal(result.postToolTailMs,null);assert.ok(Buffer.byteLength(JSON.stringify(result))<100000);
  assert.doesNotMatch(JSON.stringify(result),/secret-/);
});

test('unknown action and malformed identity are sanitized; no tools or no terminal event imply no inferred tail',()=>{
  const {observation,at}=clock();at(1,tool('item.started','a','https://private.example/SECRET'));
  at(2,tool('item.completed','a','SECRET'));at(3,tool('item.started','x'.repeat(257)));
  at(4,{type:'item.completed',item:{type:'web_search'}});
  const result=observation.finish();assert.equal(result.records[0].action,'unknown');assert.equal(result.malformedToolEventCount,2);
  assert.equal(result.turnCompletedAtMs,null);assert.equal(result.postToolTailMs,null);assert.equal(result.completeTrace,false);
  assert.doesNotMatch(JSON.stringify(result),/SECRET|private.example/);
  const empty=clock();empty.at(25,{type:'turn.completed'});assert.equal(empty.observation.finish().postToolTailMs,null);
});

test('single-pass metadata alone carries optional timing without changing decisions or legacy metadata',()=>{
  const prepared=prepareAssistantRequest({purpose:'triage',preparationMode:'single_pass_v1',items:[{id:'i',text:'Context'}]});
  const raw={text:'Held',sources:[],proposals:[],assessments:[{itemId:'i',outcome:'needs_attention',reason:'Human judgment required',tags:[]}],
    generationEditorial:[],evidence:[],decisionEvidence:[{itemId:'i',basis:'unresolved',evidenceIndices:[],dependsOnItemIds:[]}]};
  const reviewed=admitSinglePassResult(raw,prepared,{calls:0,openedUrls:[],completedActivity:[]});
  const timing=clock().observation.finish(),metadata=singlePassMetadata(prepared,reviewed,1,[],timing);
  assert.deepEqual(metadata.timing,timing);assert.equal(singlePassMetadata(prepared,reviewed).timing,undefined);
  assert.equal(generationMetadata(prepared.input,true,1).timing,undefined);
  assert.deepEqual(reviewed.admitted.proposals,[]);assert.equal(reviewed.admitted.assessments[0].outcome,'needs_attention');
});

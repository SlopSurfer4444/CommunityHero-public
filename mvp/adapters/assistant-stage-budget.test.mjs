import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {createHash} from 'node:crypto';
import {ASSISTANT_STAGE_BUDGET,stageBudgetObservation,persistStageBudgetDiagnostic} from './assistant-stage-budget.mjs';
const timeout={code:'ADAPTER_TIMEOUT',message:'PRIVATE credentials and customer text'};
const prepared=()=>({review:true,triage:true,input:'PRIVATE context https://private.example',payload:{items:[{id:'PRIVATE customer'}],firstPass:{assessments:[{itemId:'PRIVATE customer',reason:'PRIVATE reason'}]}}});
test('review diagnostics retain existing limits and only bounded counters, never private event payloads',()=>{
  let clock=1000;const input=prepared(),observation=stageBudgetObservation(input,{now:()=>clock});
  clock=1200;observation.observe({type:'item.completed',item:{type:'web_search',url:'https://private.example',query:'PRIVATE question'}});
  clock=1300;observation.observe({type:'item.completed',item:{type:'agent_message',text:'PRIVATE reply'}});
  clock=901000;const result=observation.snapshot(timeout);
  assert.equal(result.timeoutMs,2700000);assert.equal(result.maxOutputBytes,2097152);
  assert.equal(result.elapsedMs,900000);assert.equal(result.lastEventElapsedMs,300);
  assert.equal(result.itemCount,1);assert.equal(result.firstPassAssessmentCount,1);assert.equal(result.eventCount,2);
  assert.deepEqual(result.completedEvents,{web_search:1,agent_message:1,reasoning:0});
  assert.equal(result.inputSha256,createHash('sha256').update(input.input).digest('hex'));
  assert.equal(result.inputBytes,Buffer.byteLength(input.input));
  assert.equal(result.stage,'stronger_review');assert.equal(result.rootCause,'unknown');
  assert.equal(result.retryAuthorized,false);assert.equal(result.partialOutputAdmitted,false);
  assert.doesNotMatch(JSON.stringify(result),/PRIVATE|https:|credentials|customer/);
  result.completedEvents.web_search=99;assert.equal(observation.snapshot(timeout).completedEvents.web_search,1);
  assert(Object.isFrozen(ASSISTANT_STAGE_BUDGET));
});
test('first pass, research and discussion have distinct stages without extending their deadline',()=>{
  for(const [flags,options,stage]of [[{triage:true},{},'first_pass'],[{}, {research:true},'research'],[{}, {},'discussion']]){
    const result=stageBudgetObservation({input:'{}',payload:{},...flags},options).snapshot(timeout);
    assert.equal(result.stage,stage);assert.equal(result.timeoutMs,2700000);assert.equal(result.firstPassBytes,0);
    assert.equal(result.lastEventElapsedMs,null);
  }
});
test('long silent review and late progress retain one absolute 2700-second budget',()=>{
  let clock=1000;const observation=stageBudgetObservation(prepared(),{now:()=>clock});
  clock=181001;let result=observation.snapshot(timeout);
  assert.equal(result.elapsedMs,180001);assert.equal(result.timeoutMs,2700000);
  assert.equal(result.lastEventElapsedMs,null);
  clock=900999;observation.observe({type:'item.completed',item:{type:'reasoning',text:'PRIVATE reasoning'}});
  clock=901000;result=observation.snapshot(timeout);
  assert.equal(result.elapsedMs,900000);assert.equal(result.timeoutMs,2700000);
  assert.equal(result.lastEventElapsedMs,899999);
  assert.equal(result.completedEvents.reasoning,1);
  assert.equal(result.retryAuthorized,false);assert.equal(result.partialOutputAdmitted,false);
  assert.doesNotMatch(JSON.stringify(result),/PRIVATE/);
});
test('unsupported error metadata and phase never become diagnostic content',()=>{
  const observation=stageBudgetObservation(prepared());
  assert.equal(observation.snapshot({code:'PRIVATE arbitrary'}),null);
  assert.equal(observation.snapshot(timeout,'PRIVATE phase'),null);
  assert.equal(observation.snapshot({code:'CANCELLED'}),null);
  assert.equal(observation.snapshot(timeout,'admission_or_verification').eventScope,'initial_generation_only');
});
test('persisted diagnostics survive run cleanup, retain bounded history and never mask write failure',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-stage-diagnostic-'));
  try{
    const observation=stageBudgetObservation(prepared());
    assert.equal(await persistStageBudgetDiagnostic(base,observation,{code:'CANCELLED'}),false);
    for(let i=0;i<34;i++)assert.equal(await persistStageBudgetDiagnostic(base,observation,timeout,'generation'),true);
    const folder=path.join(base,'stage-diagnostics'),names=await fs.readdir(folder);assert.equal(names.length,32);
    for(const name of names){const raw=await fs.readFile(path.join(folder,name),'utf8');assert(Buffer.byteLength(raw)<4096);assert.doesNotMatch(raw,/PRIVATE|https:/);assert.equal(JSON.parse(raw).errorCode,'ADAPTER_TIMEOUT');}
    assert.equal(await persistStageBudgetDiagnostic(path.join(base,'absent'),observation,timeout),false);
    await fs.writeFile(path.join(folder,'keep.txt'),'retained');
    assert.equal(await persistStageBudgetDiagnostic(base,observation,timeout),true);
    assert.equal(await fs.readFile(path.join(folder,'keep.txt'),'utf8'),'retained');
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

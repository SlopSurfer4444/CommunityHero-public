import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {bindInvocationWireRequest,invocationRequestSha256,parseInvocationEnvelope,withInvocationBudget,runCodexWithInvocationBudget,INVOCATION_BUDGET_CONTRACT,INVOCATION_BUDGET_ENV} from './assistant-invocation-budget.mjs';

let sequence=0;
const request=(wire='{"account":"baw-russia","operation":"assistant","items":[{"id":"комментарий","ratio":0.0}]}')=>{
  const value=JSON.parse(wire);bindInvocationWireRequest(value,Buffer.from(wire));return value;
};
const envelope=(req,count=3)=>JSON.stringify({version:1,contract:INVOCATION_BUDGET_CONTRACT,account:'baw-russia',companyId:'BAW Russia',
  connectorBindingSha256:'a'.repeat(64),queueEpoch:'e4e1d9f2-49b8-4b48-846f-ab113f519f89',policyId:'policy-fixture',policyRevision:1,
  nativeJobId:'job-fixture',originalJobId:'root-fixture',requestSha256:invocationRequestSha256(req),reservationId:'reservation-'+(++sequence),
  slotIds:Array.from({length:count},(_,i)=>'slot-'+sequence+'-'+i)});
const output=(usage={input_tokens:3,cached_input_tokens:1,output_tokens:2})=>({stdout:JSON.stringify({type:'turn.completed',usage})+'\n'});
const spawnOptions={runProcessFn:async()=>output()};

test('exact native raw wire digest retains Unicode and 0.0; parsed object mutation/rebinding fails closed',()=>{
  const wire='{"operation":"assistant","account":"baw-russia","text":"Привет","n":0.0}';
  const req=request(wire);
  assert.equal(invocationRequestSha256(req),createHash('sha256').update(wire).digest('hex'));
  assert.notEqual(invocationRequestSha256(req),createHash('sha256').update(JSON.stringify(req)).digest('hex'));
  assert.throws(()=>bindInvocationWireRequest(req,JSON.stringify(req)),{code:'INVOCATION_WIRE_REBOUND'});
  req.text='Changed';
  assert.throws(()=>invocationRequestSha256(req),{code:'INVOCATION_WIRE_UNBOUND_OR_CHANGED'});
  assert.throws(()=>parseInvocationEnvelope(envelope(request()),{account:'baw-russia',operation:'assistant'}),{code:'INVOCATION_WIRE_UNBOUND_OR_CHANGED'});
});

test('primary, URL verification and visual actual fake spawns each debit one original slot; fourth never starts',async()=>{
  const req=request();let started=0;
  const result=await withInvocationBudget(req,async()=>{
    for(const stage of ['primary','url_verification','visual_followup']){
      await runCodexWithInvocationBudget('never-real-model',[],{}, {stage,runProcessFn:async()=>{started++;return output();}});
    }
    await assert.rejects(runCodexWithInvocationBudget('never-real-model',[],{},spawnOptions),{code:'INVOCATION_BUDGET_EXHAUSTED'});
    return {runMetadata:{invocationBudget:{refundAuthorized:true}}};
  },{raw:envelope(req,3)});
  assert.equal(started,3);
  const proof=result.runMetadata.invocationBudget;
  assert.deepEqual(proof.invoked.map(v=>v.stage),['primary','url_verification','visual_followup']);
  assert.deepEqual(proof.invoked.map(v=>v.ordinal),[1,2,3]);
  assert.equal(proof.refundAuthorized,false);assert.equal(proof.hardTokenCeiling,false);assert.equal(proof.billableWireRequests,null);
});

test('spawn throw keeps armed slot unknown and cannot refund for another model',async()=>{
  const req=request();let started=0;
  const result=await withInvocationBudget(req,async()=>{
    await assert.rejects(runCodexWithInvocationBudget('never-real-model',[],{}, {runProcessFn:async()=>{started++;throw new Error('mock transport lost');}}));
    await assert.rejects(runCodexWithInvocationBudget('never-real-model',[],{},spawnOptions),{code:'INVOCATION_BUDGET_EXHAUSTED'});
    return {};
  },{raw:envelope(req,1)});
  assert.equal(started,1);assert.equal(result.runMetadata.invocationBudget.invoked[0].state,'unknown');
  assert.equal(result.runMetadata.invocationBudget.issuedSlotIds.length,1);
});

test('foreign, oversized, malformed, tampered and replay envelopes never invoke callback',async()=>{
  const req=request();let callbacks=0;const raw=envelope(req,1);const valid=JSON.parse(raw);
  for(const bad of ['x',' '.repeat(16*1024+1),JSON.stringify({...valid,account:'likeavto'}),JSON.stringify({...valid,requestSha256:'b'.repeat(64)}),
    JSON.stringify({...valid,extra:'spoof'}),JSON.stringify({...valid,slotIds:['same','same']})]){
    await assert.rejects(withInvocationBudget(req,async()=>{callbacks++;return {};},{raw:bad}));
  }
  assert.equal(callbacks,0);
  await withInvocationBudget(req,async()=>({}),{raw});
  await assert.rejects(withInvocationBudget(req,async()=>{callbacks++;return {};},{raw}),{code:'INVOCATION_BUDGET_REPLAY'});
  assert.equal(callbacks,0);
});

test('observed, absent, invalid and ambiguous terminal usage is explicit and never a hard token claim',async()=>{
  for(const [stdout,status] of [[output().stdout,'observed'],['not an event','unavailable'],
    [output({input_tokens:-1,output_tokens:2}).stdout,'invalid'],[output().stdout+output().stdout,'ambiguous']]){
    const req=request();
    const result=await withInvocationBudget(req,async()=>{await runCodexWithInvocationBudget('never-real-model',[],{}, {runProcessFn:async()=>({stdout})});return {};},{raw:envelope(req,1)});
    const proof=result.runMetadata.invocationBudget;
    assert.equal(proof.invoked[0].usage.status,status);assert.equal(proof.hardTokenCeiling,false);
  }
});

test('transport metadata is stripped from model child env; cloned visual request inherits scope without rebinding',async()=>{
  const req=request();
  const result=await withInvocationBudget(req,async()=>{
    const clone=structuredClone(req);assert.throws(()=>invocationRequestSha256(clone),{code:'INVOCATION_WIRE_UNBOUND_OR_CHANGED'});
    await runCodexWithInvocationBudget('never-real-model',[],{env:{[INVOCATION_BUDGET_ENV]:'never-forward',EXISTING:'keep'}},
      {runProcessFn:async(_exe,_args,options)=>{assert.equal(options.env[INVOCATION_BUDGET_ENV],undefined);assert.equal(options.env.EXISTING,'keep');return output();},stage:'visual_followup'});
    return {};
  },{raw:envelope(req,1)});
  assert.equal(result.runMetadata.invocationBudget.invoked.length,1);
});

test('separate interactive path remains native-owned and envelope-free; nested scope cannot create new allowance',async()=>{
  const req=request();
  assert.deepEqual(await withInvocationBudget(req,async()=>({interactive:true}),{raw:''}),{interactive:true});
  await withInvocationBudget(req,async()=>{
    await assert.rejects(withInvocationBudget(req,async()=>({}),{raw:envelope(req,1)}),{code:'INVOCATION_BUDGET_NESTED_SCOPE'});
    return {};
  },{raw:envelope(req,1)});
});

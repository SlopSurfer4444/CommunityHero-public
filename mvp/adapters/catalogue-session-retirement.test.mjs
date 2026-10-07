import test from 'node:test';
import assert from 'node:assert/strict';
import {definiteCatalogueConnectFailure as safe} from './provider.mjs';
const row=()=>({status:'failed',code:'TRANSPORT_ERROR',operation:'adapter-catalogue',phase:'catalogue',
  mutationOutcome:'not-attempted',providerCallAttempted:false,providerRetryAllowed:false,
  transportStage:'read-fetch',transportCause:'connect_timeout'});
const receipt=()=>({version:1,operation:'execute',account:'likeavto',accountBinding:{accountKey:'likeavto'},results:[row()]});
test('only complete all-row same-account predispatch catalogue connection proof preserves resources',()=>{
  const result=receipt();assert.equal(safe(result,'likeavto'),true);
  result.results.push(row());assert.equal(safe(result,'likeavto'),true);
  result.results.push({...row(),status:'unknown'});assert.equal(safe(result,'likeavto'),false);
});
test('missing contradictory auth HTTP mutation and scope evidence all fail closed',()=>{
  for(const [key,value] of [['status','verified'],['code','AUTH_REQUIRED'],['operation','close'],['phase','verification'],
    ['mutationOutcome','unknown'],['providerCallAttempted',true],['providerRetryAllowed',true],['providerCallAttempted',undefined],
    ['providerRetryAllowed',undefined],['transportStage','read-auth-proactive-refresh-fetch'],['transportStage','read-json'],
    ['transportCause','tcp'],['transportCause','timeout'],['transportCause',undefined],['httpStatus',500],
    ['oauthDiagnostic',{}],['connectionState',{}],['processExit',{exitCode:1}],['processRole','credential-helper'],
    ['processId',1],['readbackProcessRecovery',{}]]){
    const result=receipt();result.results[0][key]=value;assert.equal(safe(result,'likeavto'),false,key);
  }
  for(const alter of [r=>r.version=2,r=>r.operation='readback',r=>r.account='baw-russia',
    r=>r.accountBinding.accountKey='baw-russia',r=>r.results=[],r=>r.results=[null],r=>r.results=[[]]]){
    const result=receipt();alter(result);assert.equal(safe(result,'likeavto'),false);
  }
});

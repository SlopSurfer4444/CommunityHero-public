import test from 'node:test';
import assert from 'node:assert/strict';
import {dispatch,safeError} from './bridge.mjs';
test('numeric HTTP diagnosis survives both envelopes without private content or retries',async()=>{
 for(const httpStatus of [302,401,403,429,500,503]){
  const provider=safeError({code:'HTTP_ERROR',httpStatus,message:'secret',headers:{authorization:'secret'},body:'secret'});
  let calls=0;
  await assert.rejects(dispatch({op:'read',account:'baw-russia'},{resolvePaths:()=>({}),runProcessFn:async()=>{calls++;return {stdout:JSON.stringify(provider)};}}),error=>{
   assert.deepEqual(safeError(error),{ok:false,error:{code:'HTTP_ERROR',message:'HTTP_ERROR',httpStatus,adapterOperation:'read'}});return true;
  });
  assert.equal(calls,1);
 }
});
test('invalid HTTP diagnostics are excluded',()=>{
 for(const httpStatus of ['401',null,99,600,401.5,Infinity,{},true]) assert.equal(safeError({code:'HTTP_ERROR',httpStatus}).error.httpStatus,undefined);
 assert.equal(safeError({code:'TRANSPORT_ERROR',httpStatus:401}).error.httpStatus,undefined);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {dispatch} from './bridge.mjs';

const safe={account:'baw-russia',socialRequests:0,sendGateOpen:false,retryOriginalOperation:false,
  connectionAvailability:{state:'unverified'}};
const forbidden=()=>{throw new Error('provider transport must not be reached');};
test('protected-session inspect uses only its read-only port without provider subprocess',async()=>{
  let inspected=0;
  const actual=await dispatch({account:'baw-russia',operation:'owner_session_inspect'},
    {ownerSessionInspect:async()=>{inspected++;return safe;},resolvePaths:forbidden,runProcessFn:forbidden});
  assert.deepEqual(actual,safe);assert.equal(inspected,1);
});
test('protected-session inspect refuses caller configuration before entering the protected port',async()=>{
  for(const extra of [{configPath:'foreign'},{ready:true},{generation:375},{actions:[]},{authorizationGrantId:'replacement'}]){
    let inspected=0;
    await assert.rejects(dispatch({account:'baw-russia',operation:'owner_session_inspect',...extra},
      {ownerSessionInspect:async()=>{inspected++;return safe;},resolvePaths:forbidden,runProcessFn:forbidden}),{code:'INVALID_REQUEST'});
    assert.equal(inspected,0);
  }
});
test('protected-session inspect refuses foreign scope or mutation authority claims',async()=>{
  for(const delta of [{account:'likeavto'},{socialRequests:1},{sendGateOpen:true},{retryOriginalOperation:true}]){
    await assert.rejects(dispatch({account:'baw-russia',operation:'owner_session_inspect'},
      {ownerSessionInspect:async()=>({...safe,...delta}),resolvePaths:forbidden,runProcessFn:forbidden}),{code:'INVALID_REQUEST'});
  }
});

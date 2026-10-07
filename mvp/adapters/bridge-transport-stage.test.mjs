import test from 'node:test';
import assert from 'node:assert/strict';
import {dispatch,safeError} from './bridge.mjs';
import {ScopedCredentialApiTransport} from '../connectors/angryspace-provider/src/transport/credential-api-transport.ts';

test('OAuth rejection diagnostic survives actual bridge dispatch projection without secret text',async()=>{
  const diagnostic={version:1,classification:'invalid_grant',responseShape:'json_object',error:'invalid_grant',
    trigger:'after-401',generation:350,observedAt:'2026-09-28T11:20:05.123Z'};
  await assert.rejects(dispatch({operation:'readback',account:'likeavto'},{
    resolvePaths:()=>({providerNode:'synthetic-node',providerRepo:'C:/synthetic',conveyorRepo:'C:/synthetic'}),
    runProcessFn:async()=>({stdout:JSON.stringify({ok:false,error:{code:'HTTP_ERROR',httpStatus:400,
      transportStage:'read-auth-after-401-refresh-http',oauthDiagnostic:{...diagnostic,rawBody:'private-secret'},message:'private-secret'}})})
  }),error=>{
    const final=safeError(error).error;
    assert.deepEqual(final.oauthDiagnostic,diagnostic);assert.equal(final.adapterOperation,'readback');
    assert.equal(final.httpStatus,400);assert.doesNotMatch(JSON.stringify(final),/private-secret|rawBody/);return true;
  });
});

test('closed read stages survive both safe projections with distinct outer operation',async()=>{
  for(const stage of ['read-fetch','read-json','read-auth','read-http','read-auth-proactive-token-state','read-auth-after-401-refresh-lock-timeout',...['proactive','after-401'].flatMap(phase=>['credential-helper-startup', 'credential-helper-timeout-before-connect', 'credential-helper-timeout-after-connect', 'credential-helper-transport', 'credential-helper-response-invalid', 'credential-helper-failed', 'credential-missing', 'credential-value-invalid', 'token-envelope-invalid', 'token-freshness-boundary', 'token-lifecycle-blocked'].map(source=>`read-auth-${phase}-${source}`))]){
    const first=safeError({code:'TRANSPORT_ERROR',operation:stage,message:'private token',body:'private body'});
    assert.equal(first.error.transportStage,stage);
    await assert.rejects(dispatch({operation:'context',account:'baw-russia'},{
      resolvePaths:()=>({providerNode:'synthetic-node',providerRepo:'C:/synthetic',conveyorRepo:'C:/synthetic'}),
      runProcessFn:async()=>({stdout:JSON.stringify(first)})
    }),error=>{
      const final=safeError(error).error;
      assert.equal(final.transportStage,stage);assert.equal(final.adapterOperation,'context');
      assert.equal(final.code,'TRANSPORT_ERROR');assert.ok(!JSON.stringify(final).includes('private'));return true;
    });
  }
});

test('actual read fetch and JSON failures cross the provider error boundary without private cause',async()=>{
  for(const stage of ['read-fetch','read-json']){
    const auth={withAuthorization:async use=>use('synthetic-only')};
    const transport=new ScopedCredentialApiTransport({stableAccountKey:'baw-russia',objectId:'12182'},auth,async()=>{
      if(stage==='read-fetch')throw Object.assign(new Error('private URL/token'),{cause:{code:'UND_ERR_CONNECT_TIMEOUT'}});
      return {status:200,json:async()=>{throw new Error('private body');}};
    });
    await assert.rejects(transport.request({method:'GET',path:'/v1/objects',query:{tags:'true'}}),error=>{
      assert.equal(error.operation,stage);
      const envelope=safeError(error);assert.equal(envelope.error.transportStage,stage);
      assert.equal(envelope.error.transportCause,stage==='read-fetch'?'connect_timeout':undefined);
      assert.ok(!JSON.stringify(envelope).includes('private'));
      assert.equal(envelope.error.cause,undefined);return true;
    });
  }
});

test('closed fetch cause survives two safe projections and arbitrary details do not',async()=>{
  for(const cause of ['dns','tcp','tls','connect_timeout','timeout','abort','unknown']){
    const first=safeError({code:'TRANSPORT_ERROR',operation:'read-fetch',transportCause:cause,message:'private token',url:'private URL'});
    assert.equal(first.error.transportCause,cause);
    await assert.rejects(dispatch({operation:'context',account:'baw-russia'},{
      resolvePaths:()=>({providerNode:'synthetic-node',providerRepo:'C:/synthetic',conveyorRepo:'C:/synthetic'}),
      runProcessFn:async()=>({stdout:JSON.stringify(first)})
    }),error=>{
      const final=safeError(error).error;
      assert.equal(final.transportCause,cause);assert.equal(final.transportStage,'read-fetch');
      assert.equal(final.adapterOperation,'context');
      assert.doesNotMatch(JSON.stringify(final),/private/);return true;
    });
  }
  for(const error of [
    {code:'TRANSPORT_ERROR',operation:'read-fetch',transportCause:'private token'},
    {code:'TRANSPORT_ERROR',operation:'read-json',transportCause:'dns'},
    {code:'HTTP_ERROR',operation:'read-http',transportCause:'dns'},
    {code:'TRANSPORT_ERROR',operation:'private',transportCause:'dns'}
  ]) assert.equal(safeError(error).error.transportCause,undefined);
});

test('unknown stages and unrelated error codes are omitted while upstream HTTP status is independent',()=>{
  for(const stage of ['private','read-fetch/private','read-auth-unknown-token-state','read-auth-proactive-unknown',{},null,'x'.repeat(10000)]){
    const error=safeError({code:'TRANSPORT_ERROR',operation:stage,transportStage:stage,body:'private'}).error;
    assert.equal(error.transportStage,undefined);assert.ok(!JSON.stringify(error).includes('private'));
  }
  assert.equal(safeError({code:'RESPONSE_SCHEMA_ERROR',operation:'read-json',transportStage:'read-json'}).error.transportStage,undefined);
  for(const upstream of [403,429,500]){
    const error=safeError({code:'HTTP_ERROR',httpStatus:upstream,operation:'read-auth-proactive-refresh-http'}).error;
    assert.equal(error.httpStatus,upstream);assert.equal(error.transportStage,'read-auth-proactive-refresh-http');
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {createProviderSession,runProvider} from './provider.mjs';
import {ProviderResources} from './provider-resources.mjs';
import {resolveAdapterPaths} from './config.mjs';
import {PassThrough,Writable} from 'node:stream';
import {serveProviderSession} from './provider-session.mjs';

function harness(overrides={}){
  const account='likeavto',paths=resolveAdapterPaths(account,{env:{COMMUNITYHERO_RUNTIME_MODE:'portable'},conveyorRoot:path.resolve('fixture-conveyor'),providerRoot:path.resolve('fixture-provider'),providerNode:process.execPath,providerConfig:path.resolve('fixture-provider.json'),accountCard:path.resolve('fixture-card.json')});
  const counts={store:0,closed:0,runtime:0,catalogue:0,context:0,gateway:0};let config={scope:{stableAccountKey:account,objectId:'object'},accountObjectIds:['object'],executionMode:{mode:overrides.testExecutionMode??'disabled'},
    tokenReference:{service:'test',account:'token'},oauthClientReference:{service:'test',account:'client'},lockDirectory:path.resolve('fixture-locks')};
  const calls=[],store={close:async()=>{counts.closed++;}};
  const modules={
    'transport/portable-provider-config.ts':{parsePortableProviderConfig:x=>x},
    'transport/windows-credential-manager-store.ts':{},
    'transport/platform-credential-store.ts':{createPlatformCredentialStore:options=>{calls.push(['store',options]);counts.store++;return store;}},
    'transport/linux-secret-store-socket.ts':{createLinuxSecretStoreSocketTransport:async options=>{calls.push(['socket',options]);return {close:async()=>{}};}},
    'transport/credential-runtime.ts':{createCredentialBackedAngrySpaceRuntimeFactory:()=>{counts.runtime++;const transport={};return ()=>({transport});}},
    'provider/read-only-provider.ts':{computeThreadContextEvidenceDigest:()=> 'a'.repeat(64),AngrySpaceReadOnlyProvider:class{
      async listAuthorizedObjects(){counts.catalogue++;await new Promise(resolve=>setImmediate(resolve));
        const error=typeof overrides.testCatalogueError==='function'?overrides.testCatalogueError(counts.catalogue):overrides.testCatalogueError;
        if(error)throw error;return {objectIds:['object']};}
      async getThreadContext(id){counts.context++;if(overrides.testTargetError)throw overrides.testTargetError;
        return {item:{id,status:'new'},officialReplies:[]};}
    }},
    'transport/fast-conveyor-gateway.ts':{fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,
      async runFastConveyorRequest(native,req,deps){counts.gateway++;assert.deepEqual(await deps.readAuthorizedObjectIds(),['object']);
        return overrides.testGatewayResult??{results:req.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:overrides.testReadbackStatus??'verified'}))};}},
  };
  const options={paths,platform:'win32',env:{},moduleLoader:async n=>{assert.ok(modules[n],n);return modules[n];},
    credentialStoreFactory:async()=>{counts.store++;return store;},readFileFn:async p=>JSON.stringify(p===paths.cardFile?{account,provider_root:paths.providerRepo}:config),...overrides};
  const session=createProviderSession(account,options);
  return {counts,calls,session,options,changeConfig:()=>{config={...config,tokenReference:{service:'test',account:'changed-token'}};}};
}
const context=id=>({account:'likeavto',operation:'context',objectId:'object',itemId:id});
test('same-account session reuses runtime but every target context is freshly read',async()=>{
  const h=harness();try{
    await Promise.all([h.session.run(context('one')),h.session.run(context('two'))]);
    assert.deepEqual(h.counts,{store:1,closed:0,runtime:1,catalogue:1,context:2,gateway:0});
    await h.session.run(context('three'));assert.equal(h.counts.catalogue,2);assert.equal(h.counts.context,3);assert.equal(h.counts.runtime,1);
  }finally{await h.session.close();}assert.equal(h.counts.closed,1);
});
test('binding change and cross-account requests fail before additional transport work',async()=>{
  const h=harness();await h.session.run(context('one'));const before={...h.counts};
  await assert.rejects(h.session.run({...context('two'),account:'baw-russia'}),{code:'ACCOUNT_SCOPE_MISMATCH'});
  h.changeConfig();await assert.rejects(h.session.run(context('two')),{code:'PROVIDER_SESSION_SOURCE_CHANGED'});assert.deepEqual(h.counts,before);await h.session.close();
});
test('independent readback shares only its own catalogue proof and closes once',async()=>{
  const h=harness(),action={actionId:'action',objectId:'object',itemId:'target',conversationKey:'object:target',action:'close',contextEvidenceDigest:'a'.repeat(64),expectedStatuses:['new'],workTime:0};
  const result=await h.session.run({account:'likeavto',operation:'readback',actions:[action]});
  assert.equal(result.results[0].status,'verified');assert.equal(h.counts.catalogue,1);assert.equal(h.counts.gateway,1);
  await h.session.close();await h.session.close();assert.equal(h.counts.closed,1);
  await assert.rejects(h.session.run(context('late')),{code:'PROVIDER_SESSION_CLOSED'});
});
test('session rejects assistant and contradictory operation fields without provider setup',async()=>{
  const h=harness();for(const request of [{account:'likeavto',operation:'assistant'},{...context('one'),op:'execute'}])await assert.rejects(h.session.run(request),{code:'UNSUPPORTED_OPERATION'});
  assert.equal(h.counts.store,0);await h.session.close();
});
test('Linux worker requires and binds the explicit socket backend without a Windows fallback',async()=>{
  const missing=harness({platform:'linux',credentialStoreFactory:undefined});
  await assert.rejects(missing.session.run(context('one')),{code:'LINUX_CREDENTIAL_STORE_UNCONFIGURED'});assert.equal(missing.counts.store,0);await missing.session.close();
  const connected=harness({platform:'linux',credentialStoreFactory:undefined,env:{COMMUNITYHERO_LINUX_SECRET_STORE_SOCKET:'/run/user/1000/communityhero/broker.sock'}});
  await connected.session.run(context('one'));assert.deepEqual(connected.calls[0],['socket',{socketPath:'/run/user/1000/communityhero/broker.sock',stableAccountKey:'likeavto'}]);
  assert.equal(connected.calls[1][1].platform,'linux');assert.equal(connected.calls[1][1].stableAccountKey,'likeavto');assert.equal(connected.counts.store,1);await connected.session.close();
});
test('typed UNKNOWN retires poisoned resources without repeating the request',async()=>{
  const h=harness({testReadbackStatus:'unknown'}),action={actionId:'action',objectId:'object',itemId:'target',conversationKey:'object:target',action:'close',contextEvidenceDigest:'a'.repeat(64),expectedStatuses:['new'],workTime:0};
  const result=await h.session.run({account:'likeavto',operation:'readback',actions:[action]});
  assert.equal(result.results[0].status,'unknown');assert.equal(h.session.retiring,true);assert.equal(h.counts.gateway,1);await h.session.close();
});
const executeAction={actionId:'action',objectId:'object',itemId:'target',conversationKey:'object:target',action:'close',contextEvidenceDigest:'a'.repeat(64),expectedStatuses:['new'],workTime:0};
const connectFailure=()=>Object.assign(new Error('private synthetic URL/token'),{code:'TRANSPORT_ERROR',operation:'read-fetch',transportCause:'connect_timeout'});
test('coalesced predispatch catalogue connect failure preserves resources without retrying any action',async()=>{
  const h=harness({testExecutionMode:'reviewed-comment-ops-v1',executionEnabled:true,
    testCatalogueError:attempt=>attempt===1?connectFailure():undefined,
    guardOptions:{runProcessFn:async()=>{throw new Error('mutation guard must not run');}}});
  try{
    const results=await Promise.all([h.session.run({account:'likeavto',operation:'execute',actions:[executeAction]}),
      h.session.run({account:'likeavto',operation:'execute',actions:[{...executeAction,actionId:'second'}]})]);
    assert.equal(h.counts.catalogue,1);assert.equal(h.counts.gateway,0);assert.equal(h.session.retiring,false);
    for(const result of results){const row=result.results[0];assert.equal(row.transportCause,'connect_timeout');
      assert.equal(row.mutationOutcome,'not-attempted');assert.equal(row.providerCallAttempted,false);assert.equal(row.providerRetryAllowed,false);
      assert.doesNotMatch(JSON.stringify(result),/private|synthetic URL|token/);}
    // Only a distinct future READ is issued; neither failed execute is replayed.
    await h.session.run(context('future'));
    assert.equal(h.counts.catalogue,2);assert.equal(h.counts.context,1);assert.equal(h.counts.gateway,0);
    assert.equal(h.counts.store,1);assert.equal(h.counts.runtime,1);assert.equal(h.session.retiring,false);
  }finally{await h.session.close();}assert.equal(h.counts.closed,1);
});
test('coalesced context catalogue failures preserve resources with single-use per-request proof',async()=>{
  const shared=connectFailure(),overrides={testCatalogueError:attempt=>attempt===1?shared:undefined};
  const h=harness(overrides);
  try{
    const results=await Promise.allSettled([h.session.run(context('one')),h.session.run(context('two'))]);
    assert.equal(results[0].status,'rejected');assert.equal(results[1].status,'rejected');
    assert.notEqual(results[0].reason,shared);assert.notEqual(results[0].reason,results[1].reason);
    assert.equal(results[0].reason.code,'TRANSPORT_ERROR');assert.equal(results[0].reason.operation,'read-fetch');
    assert.equal(h.session.retiring,false);assert.equal(h.counts.catalogue,1);assert.equal(h.counts.context,0);
    await h.session.run(context('future'));
    assert.equal(h.counts.catalogue,2);assert.equal(h.counts.context,1);assert.equal(h.counts.gateway,0);
    assert.equal(h.counts.runtime,1);assert.equal(h.counts.store,1);
    // The once-returned private wrapper no longer carries retention authority.
    overrides.testTargetError=results[0].reason;
    await assert.rejects(h.session.run(context('target-reuses-error')),{code:'TRANSPORT_ERROR'});
    assert.equal(h.session.retiring,true);assert.equal(h.counts.catalogue,3);assert.equal(h.counts.context,2);
  }finally{await h.session.close();}
});
test('direct runProvider catalogue errors carry no session retention proof',async()=>{
  const raw=connectFailure(),overrides={testCatalogueError:attempt=>attempt===1?raw:undefined};
  const h=harness(overrides),resources=new ProviderResources('likeavto',h.options);
  try{
    let escaped;
    try{await runProvider({...context('direct'),op:'context'},{...h.options,sessionResources:resources});}
    catch(error){escaped=error;}
    assert.equal(escaped,raw);
    overrides.testTargetError=escaped;
    await assert.rejects(h.session.run(context('target-rethrows-direct-error')),{code:'TRANSPORT_ERROR'});
    assert.equal(h.session.retiring,true);assert.equal(h.counts.gateway,0);
  }finally{await resources.close();await h.session.close();}
});
test('auth-refresh ambiguity, HTTP, nonconnect transport and contradictory process evidence retain retirement',async()=>{
  for(const error of [Object.assign(connectFailure(),{operation:'read-auth-proactive-refresh-fetch'}),
    Object.assign(connectFailure(),{transportCause:'tcp'}),Object.assign(connectFailure(),{transportCause:'unknown'}),
    Object.assign(new Error('private'),{code:'HTTP_ERROR',operation:'read-fetch',httpStatus:401}),
    Object.assign(connectFailure(),{processExit:{exitCode:1}}),Object.assign(connectFailure(),{processRole:'credential-helper',processId:1}),
    Object.assign(connectFailure(),{httpStatus:500}),Object.assign(connectFailure(),{oauthDiagnostic:{}}),
    Object.assign(connectFailure(),{connectionState:{status:'recoverable',reason:'recovery_uncertain'}}),
    Object.assign(connectFailure(),{readbackProcessRecovery:{attempts:2}})]){
    const h=harness({testExecutionMode:'reviewed-comment-ops-v1',executionEnabled:true,testCatalogueError:error});
    try{const result=await h.session.run({account:'likeavto',operation:'execute',actions:[executeAction]});
      assert.equal(result.results[0].mutationOutcome,'not-attempted');assert.equal(h.session.retiring,true);
      assert.equal(h.counts.gateway,0);assert.equal(h.counts.catalogue,1);
    }finally{await h.session.close();}
    const read=harness({testCatalogueError:error});
    try{await assert.rejects(read.session.run(context('failed-read')));
      assert.equal(read.session.retiring,true);assert.equal(read.counts.context,0);assert.equal(read.counts.gateway,0);
    }finally{await read.session.close();}
  }
  const read=harness({testTargetError:connectFailure()});
  try{await assert.rejects(read.session.run(context('same-read')),{code:'TRANSPORT_ERROR'});
    // The same public tuple from the target read is outside catalogue proof.
    assert.equal(read.session.retiring,true);assert.equal(read.counts.catalogue,1);assert.equal(read.counts.context,1);
  }finally{await read.session.close();}
});
test('gateway cannot manufacture catalogue provenance using the public no-effect tuple',async()=>{
  const result={version:1,operation:'execute',results:[{status:'failed',code:'TRANSPORT_ERROR',
    operation:'adapter-catalogue',phase:'catalogue',mutationOutcome:'not-attempted',
    providerCallAttempted:false,providerRetryAllowed:false,transportStage:'read-fetch',transportCause:'connect_timeout'}]};
  const h=harness({testGatewayResult:result});
  try{await h.session.run({account:'likeavto',operation:'readback',actions:[executeAction]});
    assert.equal(h.session.retiring,true);assert.equal(h.counts.gateway,1);assert.equal(h.counts.catalogue,1);
  }finally{await h.session.close();}
});
for(const firstOperation of ['execute','context'])test(`actual worker protocol accepts a future READ after a ${firstOperation} catalogue failure`,async()=>{
  const h=harness({testExecutionMode:'reviewed-comment-ops-v1',executionEnabled:true,
    testCatalogueError:attempt=>attempt===1?connectFailure():undefined});
  const input=new PassThrough(),rows=[],waiting=new Map();
  const output=new Writable({write(chunk,_encoding,callback){
    for(const line of String(chunk).trim().split('\n')){
      const value=JSON.parse(line);rows.push(value);waiting.get(value.id)?.(value);
    }callback();
  }});
  const done=serveProviderSession({account:'likeavto',session:h.session,input,output});
  const request=(id,body)=>new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(new Error('offline protocol did not reply')),2000);
    waiting.set(id,value=>{clearTimeout(timer);waiting.delete(id);resolve(value);});
    input.write(JSON.stringify({id,request:body})+'\n');
  });
  try{
    const failed=await request('failed',firstOperation==='execute'
      ?{account:'likeavto',operation:'execute',actions:[executeAction]}:context('failed-target'));
    if(firstOperation==='execute'){
      assert.equal(failed.ok,true);assert.equal(failed.result.results[0].mutationOutcome,'not-attempted');
    }else{
      assert.equal(failed.ok,false);assert.equal(failed.error.code,'TRANSPORT_ERROR');
      assert.equal(failed.error.transportStage,'read-fetch');assert.equal(failed.error.transportCause,'connect_timeout');
    }
    assert.equal(rows.some(value=>value.type==='retiring'),false);
    const future=await request('future',context('future-target'));
    assert.equal(future.ok,true);assert.equal(future.result.item.id,'future-target');
    assert.equal(rows.some(value=>value.type==='retiring'),false);
    assert.equal(h.counts.catalogue,2);assert.equal(h.counts.context,1);assert.equal(h.counts.gateway,0);
  }finally{input.end();await done;}
  assert.equal(h.counts.closed,1);
});

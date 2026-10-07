import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {createProviderSession} from './provider.mjs';
import {resolveAdapterPaths} from './config.mjs';

function fixture({scopeKey='likeavto'}={}){
  const account='likeavto',objectId='object';
  const paths=resolveAdapterPaths(account,{env:{COMMUNITYHERO_RUNTIME_MODE:'portable'},conveyorRoot:path.resolve('fixture-conveyor'),providerRoot:path.resolve('fixture-provider'),providerNode:process.execPath,providerConfig:path.resolve('fixture-provider.json'),accountCard:path.resolve('fixture-card.json')});
  const config={scope:{stableAccountKey:account,objectId},accountObjectIds:[objectId],executionMode:{mode:'disabled'},
    tokenReference:{service:'fixture',account:'token'},oauthClientReference:{service:'fixture',account:'client'},lockDirectory:path.resolve('fixture-locks')};
  const counts={store:0,closed:0,factory:0,status:0,catalogue:0,context:0,gateway:0};
  const status={version:1,scopeKey,generation:12,expiresAtMs:1000,expiresInSeconds:60,observedAtMs:2000,expired:true,refreshNeeded:true,
    lifecycle:{binding:'verified',phase:'ambiguous',pairPresent:false,pairValid:false,recoveryAdmitted:false,refreshExchangeAdmitted:false}};
  const modules={
    'transport/portable-provider-config.ts':{parsePortableProviderConfig:v=>v},
    'transport/windows-credential-manager-store.ts':{},
    'transport/credential-runtime.ts':{createCredentialBackedAngrySpaceRuntimeFactory:()=>{counts.factory++;return ()=>({transport:{},auth:{async inspectStatus(){counts.status++;return structuredClone(status);}}});}},
    'provider/read-only-provider.ts':{computeThreadContextEvidenceDigest:()=> 'a'.repeat(64),AngrySpaceReadOnlyProvider:class{
      async listAuthorizedObjects(){counts.catalogue++;return {objectIds:[objectId]};}
      async getThreadContext(id){counts.context++;return {item:{id,status:'new'},officialReplies:[]};}
    }},
    'transport/fast-conveyor-gateway.ts':{fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,
      async runFastConveyorRequest(){counts.gateway++;throw Error('status must not invoke provider gateway');}},
  };
  const session=createProviderSession(account,{paths,platform:'win32',env:{},
    credentialStoreFactory:async()=>{counts.store++;return {async close(){counts.closed++;}};},
    moduleLoader:async name=>{assert.ok(modules[name],name);return modules[name];},
    readFileFn:async p=>JSON.stringify(p===paths.cardFile?{account,provider_root:paths.providerRepo}:config)});
  return {session,counts,status};
}

test('explicit auth_status is local and bypasses authenticated catalogue/target paths',async()=>{
  const h=fixture();try{
    for(let i=0;i<2;i++){
      const value=await h.session.run({account:'likeavto',operation:'auth_status'});
      assert.equal(value.readOnly,true);assert.equal(value.authRequests,0);assert.equal(value.credentialMutations,0);assert.equal(value.lockMutations,0);
      assert.equal(value.account,'likeavto');assert.deepEqual(value.status,h.status);
    }
    assert.deepEqual(h.counts,{store:1,closed:0,factory:1,status:2,catalogue:0,context:0,gateway:0});
    assert.equal(h.session.retiring,false);
  }finally{await h.session.close();}assert.equal(h.counts.closed,1);
});

test('normal read does not acquire an extra auth-status traversal',async()=>{
  const h=fixture();try{
    await h.session.run({account:'likeavto',operation:'context',objectId:'object',itemId:'target'});
    assert.equal(h.counts.status,0);assert.equal(h.counts.catalogue,1);assert.equal(h.counts.context,1);
    await h.session.run({account:'likeavto',operation:'auth_status'});
    assert.equal(h.counts.status,1);assert.equal(h.counts.catalogue,1);assert.equal(h.counts.context,1);
  }finally{await h.session.close();}
});

test('auth_status never retargets a company or accepts recovery/URL fields',async()=>{
  const mismatched=fixture({scopeKey:'baw-russia'});try{
    await assert.rejects(mismatched.session.run({account:'likeavto',operation:'auth_status'}),{code:'ACCOUNT_SCOPE_MISMATCH'});
    assert.equal(mismatched.counts.catalogue,0);assert.equal(mismatched.counts.gateway,0);
  }finally{await mismatched.session.close();}
  for(const field of [{url:'https://example.invalid'},{generation:12},{recover:true},{resetClaim:true}]){
    const h=fixture();try{
      await assert.rejects(h.session.run({account:'likeavto',operation:'auth_status',...field}),{code:'INVALID_AUTH_STATUS_REQUEST'});
      assert.equal(h.counts.store,0);assert.equal(h.counts.status,0);
    }finally{await h.session.close();}
  }
});

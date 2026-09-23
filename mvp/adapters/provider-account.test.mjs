import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,rm} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {resolveAdapterPaths} from './config.mjs';
import {guardMutation,mutationGuardOptions,resolveMaxInFlight,runProvider} from './provider.mjs';

const baseAction={actionId:'attempt-1',objectId:'baw-primary',itemId:'comment-1',conversationKey:'baw-primary:comment-1',action:'close',contextEvidenceDigest:'a'.repeat(64),expectedStatuses:['new'],workTime:0};

function harness({account='baw-russia',executionMode='disabled'}={}) {
  const paths=resolveAdapterPaths(account,{env:{},conveyorRoot:'C:/fixture/conveyor',providerRoot:'C:/fixture/provider',providerNode:'C:/fixture/node.exe'});
  const primary=account==='likeavto'?'like-primary':'baw-primary',objectIds=[primary,`${primary}-sibling`],calls=[];
  const readFileFn=async file=>JSON.stringify(path.resolve(file)===path.resolve(paths.cardFile)
    ? {account,provider_root:paths.providerRepo}
    : {scope:{stableAccountKey:account,objectId:primary},accountObjectIds:objectIds,executionMode:{mode:executionMode}});
  class Store {}
  class Reader {
    constructor(_transport,scope){this.scope=scope;calls.push(['reader',scope]);}
    async listAuthorizedObjects(){return {objectIds};}
    async listQueue(){return {items:[],count:0,nextCursor:null};}
  }
  const modules={
    'transport/windows-native-companion-launcher.ts':{loadWindowsNativeCompanionConfig:async file=>{calls.push(['config',file]);return {tokenReference:{},oauthClientReference:{},lockDirectory:'C:/fixture/locks'};}},
    'transport/windows-credential-manager-store.ts':{WindowsCredentialManagerStore:Store},
    'transport/credential-runtime.ts':{createCredentialBackedAngrySpaceRuntime:options=>{calls.push(['runtime',options.scope,options.executionMode]);return {transport:{}};}},
    'provider/read-only-provider.ts':{AngrySpaceReadOnlyProvider:Reader,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)},
    'transport/fast-conveyor-gateway.ts':{
      fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,
      runFastConveyorRequest:async(_native,request)=>{calls.push(['gateway',request]);return request.operation==='capabilities'?{contractVersion:'1.1.0',operation:'capabilities',account}:{version:1,operation:request.operation,account,results:[]};}
    },
  };
  return {paths,readFileFn,calls,moduleLoader:async name=>modules[name]};
}

test('caps is account-bound, local-versioned, side-effect free and does not construct runtime',async()=>{
  const h=harness();
  const result=await runProvider({op:'caps',account:'baw-russia'},{...h,env:{}});
  assert.equal(result.contractVersion,'1.1.0');assert.equal(result.binding.accountKey,'baw-russia');assert.equal(result.provider.contractVersion,'1.1.0');
  assert.equal(result.local.runtimeSideEffects,'disabled');
  assert.ok(!h.calls.some(([kind])=>kind==='runtime'||kind==='reader'));
});

test('read-only head accepts disabled execution mode and binds every reader to requested account',async()=>{
  const h=harness();
  const result=await runProvider({op:'head',account:'baw-russia'},{...h,env:{}});
  assert.equal(result.kind,'open-status-head');
  assert.ok(h.calls.filter(([kind])=>kind==='reader').every(([,scope])=>scope.stableAccountKey==='baw-russia'));
  assert.ok(h.calls.filter(([kind])=>kind==='runtime').every(([,scope,mode])=>scope.stableAccountKey==='baw-russia'&&mode.mode==='disabled'));
});

test('scan forwards only the bounded provider contract and keeps the selected account binding',async()=>{
  const h=harness();
  const result=await runProvider({op:'scan',account:'baw-russia',statuses:['new','deleted'],pageSize:100,maxPages:4,maxItems:250,maxElapsedMs:5000,resume:'opaque',ignored:'drop-me'},{...h,env:{}});
  const request=h.calls.find(([kind])=>kind==='gateway')[1];
  assert.deepEqual(request,{version:1,operation:'scan',account:'baw-russia',statuses:['new','deleted'],pageSize:100,maxPages:4,maxItems:250,maxElapsedMs:5000,resume:'opaque'});
  assert.equal(result.accountBinding.accountKey,'baw-russia');
});

test('execute requires explicit opt-in, preserves account scope, and forwards configured concurrency',async()=>{
  const disabled=harness({executionMode:'reviewed-comment-ops-v1'});
  await assert.rejects(runProvider({op:'execute',account:'baw-russia',actions:[baseAction]},{...disabled,env:{}}),{code:'EXECUTION_DISABLED'});
  const enabled=harness({executionMode:'reviewed-comment-ops-v1'}),lockRoot=await mkdtemp(path.join(os.tmpdir(),'communityhero-provider-lock-'));
  try {
    await runProvider({op:'execute',account:'baw-russia',actions:[baseAction],maxInFlight:7},{...enabled,executionEnabled:true,guardOptions:{lockRoot,runProcessFn:async(_exe,args)=>{const script=args.at(-1);assert.match(script,/baw-russia/);assert.match(script,/baw-russia[\\\\/]provider\.json/);return {stdout:'clear'};}}});
    const request=enabled.calls.find(([kind])=>kind==='gateway')[1];
    assert.equal(request.account,'baw-russia');assert.equal(request.maxInFlight,7);assert.equal(request.actions[0].action,'close');
  } finally {await rm(lockRoot,{recursive:true,force:true});}
});

test('maxInFlight defaults to batch size and admits configured values above one',()=>{
  assert.equal(resolveMaxInFlight({},24,{}),24);
  assert.equal(resolveMaxInFlight({maxInFlight:50},1,{}),50);
  assert.equal(resolveMaxInFlight({},1,{COMMUNITYHERO_MAX_IN_FLIGHT:'100'}),100);
  assert.throws(()=>resolveMaxInFlight({maxInFlight:101},1,{}),{code:'INVALID_MAX_IN_FLIGHT'});
});

test('engine copies sharing one Provider config derive one resource lock root',()=>{
  const native={lockDirectory:'C:/shared/provider-locks'};
  const first=mutationGuardOptions(native),second=mutationGuardOptions(native);
  assert.equal(first.lockRoot,path.resolve('C:/shared/provider-locks/communityhero-resource-locks'));
  assert.equal(second.lockRoot,first.lockRoot);
  assert.equal(mutationGuardOptions(native,{lockRoot:'C:/isolated/test-locks'}).lockRoot,'C:/isolated/test-locks');
  assert.throws(()=>mutationGuardOptions({lockDirectory:'relative'}),{code:'SCOPE_UNAVAILABLE'});
});

test('resource locks admit independent subprocesses while blocking two replies in one conversation',async()=>{
  const lockRoot=await mkdtemp(path.join(os.tmpdir(),'communityhero-resource-lock-')),clear=async()=>({stdout:'clear'});
  let release;const gate=new Promise(resolve=>{release=resolve;}),started=[];
  const independent=Array.from({length:6},(_,index)=>({...baseAction,actionId:`a-${index}`,itemId:`i-${index}`,conversationKey:`baw-primary:thread-${index}`}));
  try {
    const running=independent.map(action=>guardMutation('baw-russia','C:/fixture/baw-russia/provider.json',[action],async()=>{started.push(action.itemId);await gate;return action.itemId;},{lockRoot,runProcessFn:clear}));
    while(started.length<6)await new Promise(resolve=>setImmediate(resolve));
    assert.equal(started.length,6);
    release();assert.equal((await Promise.all(running)).length,6);

    let releaseReply,replyStarted;const replyGate=new Promise(resolve=>{releaseReply=resolve;}),began=new Promise(resolve=>{replyStarted=resolve;});
    const first={...baseAction,actionId:'reply-a',itemId:'reply-item-a',conversationKey:'baw-primary:shared',action:'reply_and_close',reply:'A'};
    const second={...first,actionId:'reply-b',itemId:'reply-item-b',reply:'B'};
    const owner=guardMutation('baw-russia','C:/fixture/baw-russia/provider.json',[first],async()=>{replyStarted();await replyGate;},{lockRoot,runProcessFn:clear});
    await began;
    await assert.rejects(guardMutation('baw-russia','C:/fixture/baw-russia/provider.json',[second],async()=>{}, {lockRoot,runProcessFn:clear}),{code:'ACCOUNT_EXECUTION_BUSY'});
    releaseReply();await owner;

    let releaseItem,itemStarted;const itemGate=new Promise(resolve=>{releaseItem=resolve;}),itemBegan=new Promise(resolve=>{itemStarted=resolve;});
    const sameItem={...baseAction,actionId:'same-item-owner'};
    const itemOwner=guardMutation('baw-russia','C:/fixture/baw-russia/provider.json',[sameItem],async()=>{itemStarted();await itemGate;},{lockRoot,runProcessFn:clear});
    await itemBegan;
    await assert.rejects(guardMutation('baw-russia','C:/fixture/baw-russia/provider.json',[{...sameItem,actionId:'same-item-contender',action:'hide'}],async()=>{}, {lockRoot,runProcessFn:clear}),{code:'ACCOUNT_EXECUTION_BUSY'});
    releaseItem();await itemOwner;
  } finally {release?.();await rm(lockRoot,{recursive:true,force:true});}
});

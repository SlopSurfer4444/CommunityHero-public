import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {ACCOUNT_KEYS,accountBinding,accountDefinition,resolveAdapterPaths,validateScope} from './config.mjs';

const roots={env:{},conveyorRoot:'C:/fixture/conveyor',providerRoot:'C:/fixture/provider',providerNode:'C:/fixture/node.exe'};
const documents=(account,paths,executionMode='disabled')=>async file=>JSON.stringify(
  path.resolve(file)===path.resolve(paths.cardFile)
    ? {account,provider_root:paths.providerRepo}
    : {scope:{stableAccountKey:account,objectId:account==='likeavto'?'like-primary':'baw-primary'},accountObjectIds:[account==='likeavto'?'like-primary':'baw-primary'],executionMode:{mode:executionMode}}
);

test('account allowlist and root overrides derive isolated account paths',()=>{
  assert.deepEqual([...ACCOUNT_KEYS],['likeavto','baw-russia']);
  assert.equal(accountDefinition('likeavto').displayName,'LikeAvto');
  assert.equal(accountDefinition('baw-russia').displayName,'BAW Russia');
  assert.throws(()=>accountDefinition('foreign'),{code:'ACCOUNT_SCOPE_MISMATCH'});
  const like=resolveAdapterPaths('likeavto',roots),baw=resolveAdapterPaths('baw-russia',roots);
  assert.equal(like.providerRepo,path.resolve('C:/fixture/provider'));
  assert.equal(like.providerRootExplicit,true);
  assert.match(like.configFile,/likeavto[\\/]provider\.json$/);
  assert.match(baw.configFile,/baw-russia[\\/]provider\.json$/);
  assert.notEqual(like.configFile,baw.configFile);
});

test('portable bindings and root aliases resolve to explicit absolute paths',()=>{
  const paths=resolveAdapterPaths('baw-russia',{env:{
    COMMUNITYHERO_RUNTIME_MODE:'portable',
    COMMUNITYHERO_CONVEYOR_ROOT:'C:/portable/conveyor',
    COMMUNITYHERO_CONVEYOR_REPO:'C:/portable/conveyor',
    COMMUNITYHERO_PROVIDER_ROOT:'C:/portable/provider',
    COMMUNITYHERO_PROVIDER_REPO:'C:/portable/provider',
    COMMUNITYHERO_PROVIDER_RUNTIME:'C:/portable/private/runtime',
    COMMUNITYHERO_PROVIDER_CONFIG:'C:/portable/secrets/provider.json',
    COMMUNITYHERO_ACCOUNT_CARD:'C:/portable/config/account.json',
    COMMUNITYHERO_PROVIDER_NODE:'C:/portable/node.exe',
  }});
  assert.equal(paths.conveyorRepo,path.resolve('C:/portable/conveyor'));
  assert.equal(paths.providerRepo,path.resolve('C:/portable/provider'));
  assert.equal(paths.providerRootExplicit,true);
  assert.equal(paths.runtime,path.resolve('C:/portable/private/runtime'));
  assert.equal(paths.configFile,path.resolve('C:/portable/secrets/provider.json'));
  assert.equal(paths.cardFile,path.resolve('C:/portable/config/account.json'));
  assert.equal(paths.providerNode,path.resolve('C:/portable/node.exe'));
});

test('portable mode requires external roots, config, card and node',()=>{
  const env={COMMUNITYHERO_RUNTIME_MODE:'portable'};
  assert.throws(()=>resolveAdapterPaths('likeavto',{env}),{code:'SCOPE_UNAVAILABLE'});
  const bindings={...env,
    COMMUNITYHERO_CONVEYOR_REPO:'C:/portable/conveyor',
    COMMUNITYHERO_PROVIDER_REPO:'C:/portable/provider',
    COMMUNITYHERO_PROVIDER_CONFIG:'C:/portable/secrets/provider.json',
    COMMUNITYHERO_ACCOUNT_CARD:'C:/portable/config/likeavto.json',
    COMMUNITYHERO_PROVIDER_NODE:'C:/portable/node.exe',
  };
  assert.equal(resolveAdapterPaths('likeavto',{env:bindings}).configFile,path.resolve(bindings.COMMUNITYHERO_PROVIDER_CONFIG));
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{...bindings,COMMUNITYHERO_PROVIDER_NODE:undefined}}),{code:'SCOPE_UNAVAILABLE'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_RUNTIME_MODE:'unexpected'}}),{code:'INVALID_RUNTIME_MODE'});
});

test('inconsistent aliases, option bindings and relative paths fail closed',()=>{
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_CONVEYOR_ROOT:'C:/one',COMMUNITYHERO_CONVEYOR_REPO:'C:/two'}}),{code:'INVALID_CONVEYOR_ROOT'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_PROVIDER_ROOT:'C:/one',COMMUNITYHERO_PROVIDER_REPO:'C:/two'}}),{code:'INVALID_PROVIDER_ROOT'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_PROVIDER_REPO:'C:/one'},providerRoot:'C:/two'}),{code:'INVALID_PROVIDER_ROOT'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_PROVIDER_CONFIG:'C:/one.json'},providerConfig:'C:/two.json'}),{code:'INVALID_PROVIDER_CONFIG'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_ACCOUNT_CARD:'relative.json'}}),{code:'INVALID_ACCOUNT_CARD'});
  assert.throws(()=>resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_PROVIDER_RUNTIME:''}}),{code:'INVALID_PROVIDER_RUNTIME'});
  const matching=resolveAdapterPaths('likeavto',{env:{COMMUNITYHERO_PROVIDER_REPO:'C:/same'},providerRoot:'C:/same'});
  assert.equal(matching.providerRepo,path.resolve('C:/same'));
});

test('read scope does not require reviewed execution while execute scope does',async()=>{
  const paths=resolveAdapterPaths('baw-russia',roots),readFileFn=documents('baw-russia',paths);
  const read=await validateScope('baw-russia',{paths,readFileFn});
  assert.equal(read.binding.accountKey,'baw-russia');
  await assert.rejects(validateScope('baw-russia',{paths,readFileFn,requireExecution:true}),{code:'SCOPE_UNAVAILABLE'});
  const reviewed=await validateScope('baw-russia',{paths,readFileFn:documents('baw-russia',paths,'reviewed-comment-ops-v1'),requireExecution:true});
  assert.equal(reviewed.binding.primaryObjectId,'baw-primary');
});

test('scope validation rejects cross-account cards and provider roots',async()=>{
  const paths=resolveAdapterPaths('baw-russia',roots);
  await assert.rejects(validateScope('baw-russia',{paths,readFileFn:documents('likeavto',paths)}),{code:'SCOPE_UNAVAILABLE'});
  await assert.rejects(validateScope('likeavto',{paths,readFileFn:documents('likeavto',paths)}),{code:'SCOPE_UNAVAILABLE'});
  const custom=resolveAdapterPaths('baw-russia',{env:{
    COMMUNITYHERO_CONVEYOR_REPO:'C:/portable/conveyor',COMMUNITYHERO_PROVIDER_REPO:'C:/portable/provider',
    COMMUNITYHERO_PROVIDER_CONFIG:'C:/portable/provider.json',COMMUNITYHERO_ACCOUNT_CARD:'C:/portable/account.json',
  }});
  await assert.rejects(validateScope('baw-russia',{paths:custom,readFileFn:async file=>JSON.stringify(
    path.resolve(file)===path.resolve(custom.cardFile)
      ? {account:'baw-russia',provider_root:custom.providerRepo}
      : {scope:{stableAccountKey:'likeavto',objectId:'like-primary'},accountObjectIds:['like-primary']}
  )}),{code:'SCOPE_UNAVAILABLE'});
  assert.throws(()=>accountBinding('baw-russia',{scope:{stableAccountKey:'likeavto',objectId:'x'},accountObjectIds:['x']}),{code:'SCOPE_UNAVAILABLE'});
  const defaults=resolveAdapterPaths('baw-russia',{env:{}});
  const wrongRoot=async file=>JSON.stringify(path.resolve(file)===path.resolve(defaults.cardFile)?{account:'baw-russia',provider_root:'C:/foreign/provider'}:{scope:{stableAccountKey:'baw-russia',objectId:'baw-primary'},accountObjectIds:['baw-primary']});
  await assert.rejects(validateScope('baw-russia',{paths:defaults,readFileFn:wrongRoot}),{code:'SCOPE_UNAVAILABLE'});
});

test('explicit provider relocation accepts an older absolute card root but rejects invalid card roots',async()=>{
  const paths=resolveAdapterPaths('baw-russia',{env:{COMMUNITYHERO_PROVIDER_REPO:'C:/new/provider'},conveyorRoot:'C:/conveyor'});
  const relocated=async file=>JSON.stringify(path.resolve(file)===path.resolve(paths.cardFile)
    ? {account:'baw-russia',provider_root:'C:/old/provider'}
    : {scope:{stableAccountKey:'baw-russia',objectId:'baw-primary'},accountObjectIds:['baw-primary']});
  assert.equal((await validateScope('baw-russia',{paths,readFileFn:relocated})).binding.accountKey,'baw-russia');
  const relative=async file=>JSON.stringify(path.resolve(file)===path.resolve(paths.cardFile)
    ? {account:'baw-russia',provider_root:'relative/provider'}
    : {scope:{stableAccountKey:'baw-russia',objectId:'baw-primary'},accountObjectIds:['baw-primary']});
  await assert.rejects(validateScope('baw-russia',{paths,readFileFn:relative}),{code:'SCOPE_UNAVAILABLE'});
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {runAccountMaterials} from './materials.mjs';

const scope=(account,displayName)=>({
  binding:{accountKey:account,providerAccountId:account,displayName,objectIds:['11390','11391']},
  paths:{conveyorRepo:'C:/scoped-conveyor',cardFile:`C:/custom-cards/${account}.json`},
});

test('materials adapter passes only the selected account and validated object allowlist',async()=>{
  for(const [account,display] of [['likeavto','LikeAvto'],['baw-russia','BAW Russia']]){
    let invocation;
    const result=await runAccountMaterials('materials',{account},120000,{
      validateScopeFn:async value=>{assert.equal(value,account);return scope(account,display);},
      runProcessFn:async(command,args,options)=>{
        invocation={command,args,options};
        return {stdout:JSON.stringify({ok:true,result:{materials:[{id:account,account:display}]}})};
      },
    });
    assert.equal(result.materials[0].account,display);
    assert.match(invocation.command,/scoped-conveyor[\\/]\.venv[\\/]Scripts[\\/]python\.exe$/);
    assert.deepEqual(invocation.args.slice(0,1),['-B']);
    assert.equal(invocation.options.env.COMMUNITYHERO_CONVEYOR_ROOT,'C:/scoped-conveyor');
    assert.equal(invocation.options.env.COMMUNITYHERO_ACCOUNT_CARD,`C:/custom-cards/${account}.json`);
    const request=JSON.parse(invocation.options.input);
    assert.equal(request.account,account);
    assert.equal(request.operation,'materials');
    assert.deepEqual(request.accountObjectIds,['11390','11391']);
  }
});

test('materials adapter rejects unknown and mismatched accounts before starting Python',async()=>{
  let calls=0;
  const runProcessFn=async()=>{calls+=1;throw new Error('must not run');};
  await assert.rejects(runAccountMaterials('materials',{account:'unknown'},1,{runProcessFn}),{code:'ACCOUNT_SCOPE_MISMATCH'});
  await assert.rejects(runAccountMaterials('materials',{account:'baw-russia'},1,{
    runProcessFn,
    validateScopeFn:async()=>scope('likeavto','LikeAvto'),
  }),{code:'ACCOUNT_SCOPE_MISMATCH'});
  assert.equal(calls,0);
});

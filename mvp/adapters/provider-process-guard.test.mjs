import test from 'node:test';
import assert from 'node:assert/strict';
import {assertNoLinuxLegacyConveyor} from './provider-process-guard.mjs';

const options=(args)=>({readdirFn:async()=>['self','123','456'],readFileFn:async p=>Buffer.from((p.includes('/123/')?args:['/usr/bin/unrelated']).join('\0'))});
test('Linux process fence detects only bound retired entrypoints with exact account/config arguments',async()=>{
  for(const args of [['python','-m','commentops_fast','run','--account','likeavto'],['node','/source/fast-conveyor-cli.ts','--config=/private/likeavto.json']])
    await assert.rejects(assertNoLinuxLegacyConveyor('likeavto','/private/likeavto.json',options(args)),{code:'CONCURRENT_CONVEYOR_ACTIVE'});
  for(const args of [['python','-m','commentops_fast','run','--account','baw-russia'],['node','/source/fast-conveyor-cli.ts','--config','/private/baw.json'],['node','provider-session.mjs','--account','likeavto']])
    await assertNoLinuxLegacyConveyor('likeavto','/private/likeavto.json',options(args));
});
test('Linux process fence fails closed on unreadable metadata and tolerates process exit races',async()=>{
  await assert.rejects(assertNoLinuxLegacyConveyor('likeavto','/config',{readdirFn:async()=>{throw new Error('private');}}),{code:'CONVEYOR_INSPECTION_UNAVAILABLE'});
  for(const code of ['EACCES','EIO'])await assert.rejects(assertNoLinuxLegacyConveyor('likeavto','/config',{...options([]),readFileFn:async()=>{throw Object.assign(new Error('private'),{code});}}),{code:'CONVEYOR_INSPECTION_UNAVAILABLE'});
  await assertNoLinuxLegacyConveyor('likeavto','/config',{...options([]),readFileFn:async()=>{throw Object.assign(new Error('gone'),{code:'ENOENT'});}});
});

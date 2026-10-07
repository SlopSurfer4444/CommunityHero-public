import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, writeFile, readFile, rename, rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {replaceCheckpointFile} from '../cli/workflow.mjs';

test('sharing contention retains old BAW intent until the same saved checkpoint atomically replaces it',async()=>{
  const root=await mkdtemp(join(tmpdir(),'ch-baw-journal-'));
  try {
    const target=join(root,'journal.json'),pending=join(root,'journal.pending');
    const prior=JSON.stringify({account:'baw-russia',requestId:'original',phase:'admitting'});
    const next=JSON.stringify({account:'baw-russia',requestId:'original',phase:'acknowledged',jobId:'original-job'});
    await writeFile(target,prior);await writeFile(pending,next);
    const calls=[],waits=[];
    await replaceCheckpointFile(pending,target,{pause:async ms=>waits.push(ms),renameFile:async(from,to)=>{
      calls.push([from,to]);
      assert.equal(await readFile(target,'utf8'),prior);
      assert.equal(await readFile(pending,'utf8'),next);
      if(calls.length<3)throw Object.assign(new Error('sharing'),{code:calls.length===1?'EPERM':'EACCES'});
      await rename(from,to);
    }});
    assert.deepEqual(calls,Array.from({length:3},()=>[pending,target]));
    assert.deepEqual(waits,[5,10]);assert.equal(await readFile(target,'utf8'),next);
    await assert.rejects(readFile(pending),{code:'ENOENT'});
  } finally {await rm(root,{recursive:true,force:true});}
});

test('persistent sharing failure is bounded and returns the original error',async()=>{
  const error=Object.assign(new Error('sharing remains denied'),{code:'EBUSY'});
  let attempts=0;const waits=[];
  await assert.rejects(replaceCheckpointFile('same-temp','same-target',{
    renameFile:async(from,to)=>{assert.deepEqual([from,to],['same-temp','same-target']);attempts++;throw error;},
    pause:async ms=>waits.push(ms)
  }),value=>value===error);
  assert.equal(attempts,6);assert.deepEqual(waits,[5,10,20,40,80]);
});

test('unrelated filesystem failures are never retried',async()=>{
  for(const code of ['ENOENT','ENOSPC','EINVAL']) {
    let attempts=0;
    await assert.rejects(replaceCheckpointFile('temp','target',{
      renameFile:async()=>{attempts++;throw Object.assign(new Error('failed'),{code});},
      pause:async()=>{assert.fail('unexpected retry');}
    }),{code});
    assert.equal(attempts,1);
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {runProcess} from './process.mjs';
test('observer rejection terminates subprocess and preserves reason',async()=>{
 const started=Date.now();
 await assert.rejects(runProcess(process.execPath,['-e',"console.log('event');setInterval(()=>{},1000)"],{
  timeoutMs:10000,onStdout:()=>{throw Object.assign(new Error('Budget reached'),{code:'ASSISTANT_RESEARCH_LIMIT'});}
 }),{code:'ASSISTANT_RESEARCH_LIMIT'});
 assert.ok(Date.now()-started<8000);
});
test('optional stdout observer preserves split UTF-8 and ordinary result',async()=>{
 let observed='';
 const result=await runProcess(process.execPath,['-e',"const b=Buffer.from('Привет');process.stdout.write(b.subarray(0,1));setTimeout(()=>process.stdout.write(b.subarray(1)),25)"],{onStdout:s=>observed+=s});
 assert.equal(result.stdout,'Привет');assert.equal(observed,'Привет');
});

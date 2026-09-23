// Starts only isolated fake-adapter servers; never sends a social action.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile,readFile,access} from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.COMMUNITYHERO_TEST_BINARY||path.join(root,'server/target/auto-check/debug/communityhero-server.exe');
async function check(outcome,port){
 const dir=await mkdtemp(path.join(os.tmpdir(),'communityhero-auto-'));
 const scenario=path.join(dir,'scenario.json'),effects=path.join(dir,'effects.jsonl');
 await writeFile(scenario,JSON.stringify({autoData:true,createdAt:new Date().toISOString(),triageOutcome:outcome}));
 const base=`http://127.0.0.1:${port}`;
 let child,stderr='';
 async function start(){
   child=spawn(binary,[],{windowsHide:true,cwd:path.join(root,'server'),env:{...process.env,COMMUNITYHERO_DATABASE_URL:undefined,COMMUNITYHERO_PORT:String(port),COMMUNITYHERO_DATA_DIR:dir,COMMUNITYHERO_NODE:process.execPath,COMMUNITYHERO_BRIDGE:path.join(root,'tests/fake-bridge.mjs'),COMMUNITYHERO_TEST_SCENARIO:scenario,COMMUNITYHERO_TEST_EFFECTS:effects},stdio:['ignore','ignore','pipe']});
   child.stderr.on('data',b=>stderr+=b.toString());
   await until(async()=>{try{return (await fetch(base+'/api/health')).ok;}catch{return false;}},10000);
 }
 async function stop(){if(child&&child.exitCode===null)await new Promise(resolve=>{child.once('exit',resolve);child.kill();});}
 async function state(){return (await fetch(base+'/api/bootstrap')).json();}
 async function until(predicate,timeout=45000){const end=Date.now()+timeout;while(Date.now()<end){if(child?.exitCode!==null&&child?.exitCode!==undefined)throw Error(stderr||'server exited');const value=await predicate();if(value)return value;await new Promise(r=>setTimeout(r,250));}throw Error(`Timeout for ${outcome}: ${stderr}`);}
 try{
   await start();
   const b=await until(async()=>{const b=await state();return b.items[0]?.autoPreparation?.status===(outcome==='needs_attention'?'needs_attention':'prepared')&&b;});
   assert.equal(b.items[0].workflow,outcome==='needs_attention'?'attention':'prepared');
   assert.ok(b.items[0].autoPreparation.reason);
   assert.equal(b.proposals.length,outcome==='needs_attention'?0:1);
   if(b.proposals.length){assert.equal(b.proposals[0].kind,outcome==='close'?'close':'reply_and_close');assert.ok(b.proposals[0].prepareBundleId);}
   assert.equal(b.items[0].draft,'','AI must not write a human draft');
   const callsBefore=(await readFile(effects+'.prepare','utf8')).trim().split('\n').length;
   const expectedCalls=outcome==='reply'?1:2;
   assert.equal(callsBefore,expectedCalls);
   if(expectedCalls===2){
     const calls=(await readFile(effects+'.prepare','utf8')).trim().split('\n').map(JSON.parse);
     assert.deepEqual(calls.map(c=>c.purpose),['triage','triage_review']);
     assert.equal(calls[1].firstPass.assessments[0].outcome,outcome);
     assert.equal(b.preparationResearch.length,1,'stronger review persists its scoped archive');
     assert.equal(b.preparationResearch[0].review.status,'completed');
   }
   await stop();await start();
   // Startup tick plus an additional scheduler tick must not repeat the model call.
   await new Promise(r=>setTimeout(r,17000));
   assert.equal((await readFile(effects+'.prepare','utf8')).trim().split('\n').length,expectedCalls,'restart/refresh must not repeat completed preparation');
   await assert.rejects(access(effects),'no execute effect is permitted');
   const restored=await state();
   assert.equal(restored.items[0].autoPreparation.status,outcome==='needs_attention'?'needs_attention':'prepared');
   return {outcome,ok:true,modelCalls:expectedCalls,externalEffects:0};
 }finally{await stop();}
}
console.log(JSON.stringify(await Promise.all(['reply','close','needs_attention'].map((outcome,i)=>check(outcome,4193+i)))));

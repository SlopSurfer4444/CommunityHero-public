import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {runProcess} from './process.mjs';
import {ASSISTANT_STAGE_BUDGET,isAssistantProgressEvent} from './assistant-stage-budget.mjs';
import {persistAssistantFailureEvidence} from './assistant-failure-evidence.mjs';
import {withAssistantLane,runUrlVerificationAttempt} from './assistant.mjs';

test('finite model budgets and validated event activity exclude arbitrary noise',()=>{
  assert.equal(ASSISTANT_STAGE_BUDGET.timeoutMs,45*60*1000);
  assert.equal(ASSISTANT_STAGE_BUDGET.idleTimeoutMs,15*60*1000);
  for(const event of [null,'progress',{type:'noise'},{type:'error'},{type:'item.completed',item:{type:'error'}},{type:'item.completed',item:{type:'command_execution'}}])assert.equal(isAssistantProgressEvent(event),false);
  for(const event of [{type:'turn.started'},{type:'turn.completed'},{type:'item.started',item:{type:'web_search'}},{type:'item.updated',item:{type:'agent_message'}}])assert.equal(isAssistantProgressEvent(event),true);
});

test('validated progress renews idle only; caller total remains absolute and reaps child',async()=>{
  let pid,count=0;
  await assert.rejects(runProcess(process.execPath,['-e',"console.log(process.pid);setInterval(()=>console.log('progress'),40)"],{
    timeoutMs:1000,idleTimeoutMs:400,onStdout:chunk=>{pid??=Number(chunk.split('\n')[0]);count++;return true;}
  }),error=>{assert.equal(error.code,'ADAPTER_TIMEOUT');assert.equal(error.timeoutKind,'total');return true;});
  assert(count>2);assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

test('stdout and stderr noise cannot reset idle without caller validation',async()=>{
  let pid;
  await assert.rejects(runProcess(process.execPath,['-e',"console.log(process.pid);setInterval(()=>{console.log('noise');console.error('noise');},30)"],{
    timeoutMs:4000,idleTimeoutMs:600,onStdout:chunk=>{pid??=Number(chunk.split('\n')[0]);return false;}
  }),error=>{assert.equal(error.code,'ADAPTER_TIMEOUT');assert.equal(error.timeoutKind,'idle');return true;});
  assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
});

test('cancellation is preserved with both timer types and never becomes a retry',async()=>{
  let signalled=false;
  await assert.rejects(runProcess(process.execPath,['-e',"console.log('started');setInterval(()=>{},1000)"],{
    timeoutMs:5000,idleTimeoutMs:2000,onStdout:()=>{if(!signalled){signalled=true;process.emit('SIGTERM');}return true;}
  }),{code:'CANCELLED'});
});

test('paid output survives lane cleanup in private quarantine without credential/home copy',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-timeout-evidence-'));
  try{
    let receipt,secured=false;
    await assert.rejects(withAssistantLane(base,'preparation',async home=>{
      await fs.writeFile(path.join(home,'auth.json'),'{"access_token":"DO_NOT_COPY"}');
      await fs.writeFile(path.join(home,'response.json'),'{"text":"Exact paid candidate"}');
      receipt=await persistAssistantFailureEvidence(path.dirname(home),home,{input:'PRIVATE input',stdout:'{"type":"item.completed","item":{"type":"agent_message","text":"Exact paid candidate"}}\n',
        failure:{code:'ADAPTER_TIMEOUT',timeoutKind:'total'},secureDirectory:async dir=>{assert.deepEqual(await fs.readdir(dir),[]);secured=true;}});
      throw Object.assign(Error('original failure'),{code:'ADAPTER_TIMEOUT'});
    }),{code:'ADAPTER_TIMEOUT'});
    assert(secured);assert.equal(receipt.admitted,false);assert.equal(receipt.replayAuthorized,false);assert.equal(receipt.credentialsCopied,false);
    const lane=path.join(base,'preparation');assert.deepEqual(await fs.readdir(lane),['failure-evidence']);
    const root=path.join(lane,'failure-evidence'),dirs=await fs.readdir(root),folder=path.join(root,dirs[0]);
    assert.deepEqual((await fs.readdir(folder)).sort(),['events.jsonl','receipt.json','response.json']);
    assert.equal(await fs.readFile(path.join(folder,'response.json'),'utf8'),'{"text":"Exact paid candidate"}');
    assert.equal(receipt.files.find(f=>f.name==='response.json').sha256,createHash('sha256').update('{"text":"Exact paid candidate"}').digest('hex'));
    assert.doesNotMatch(await fs.readFile(path.join(folder,'receipt.json'),'utf8'),/DO_NOT_COPY|PRIVATE input|Exact paid/);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('credential-shaped content is omitted, secure-directory failure never masks original failure',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-timeout-private-'));
  try{
    const home=path.join(base,'run-owned');await fs.mkdir(home);
    await fs.writeFile(path.join(home,'response.json'),'{"access_token":"DO_NOT_COPY"}');
    const receipt=await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:'{"authorization":"DO_NOT_COPY"}\n',failure:{code:'CANCELLED'},secureDirectory:async()=>{}});
    assert.deepEqual(receipt.files,[]);assert.deepEqual(receipt.omissions,{sensitive_event:1,sensitive_response:1});
    for(const key of ['api_key','apiKey','session','cookie','token','secret']){
      await fs.writeFile(path.join(home,'response.json'),JSON.stringify({[key]:'DO_NOT_COPY'}));
      const omitted=await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:JSON.stringify({[key]:'DO_NOT_COPY'})+'\n',failure:{code:'CANCELLED'},secureDirectory:async()=>{}});
      assert.deepEqual(omitted.files,[]);assert.deepEqual(omitted.omissions,{sensitive_event:1,sensitive_response:1});
    }
    for(const key of ['apiKey','password','cookie','token']){
      const credential=JSON.stringify({[key]:'SYNTHETIC_NEVER_REAL'});
      const event=JSON.stringify({type:'item.completed',item:{type:'agent_message',text:credential}});
      await fs.writeFile(path.join(home,'response.json'),JSON.stringify({text:credential}));
      const omitted=await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:event+'\n',failure:{code:'ADAPTER_TIMEOUT'},secureDirectory:async()=>{}});
      assert.deepEqual(omitted.files,[]);assert.deepEqual(omitted.omissions,{sensitive_event:1,sensitive_response:1});
    }
    const encodedKey='{ "api\\u004bey": "SYNTHETIC_NEVER_REAL" }';
    await fs.writeFile(path.join(home,'response.json'),JSON.stringify({text:JSON.stringify({nested:encodedKey})}));
    const encoded=await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:JSON.stringify({item:{type:'agent_message',text:encodedKey}})+'\n',failure:{code:'ADAPTER_TIMEOUT'},secureDirectory:async()=>{}});
    assert.deepEqual(encoded.files,[]);assert.deepEqual(encoded.omissions,{sensitive_event:1,sensitive_response:1});
    assert.equal(await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:'{"safe":true}\n',failure:{code:'CANCELLED'},secureDirectory:async()=>{throw Error('ACL denied');}}),null);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('failed URL verification retains its stdout and exact response before lane cleanup',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-verification-evidence-'));
  try{
    let receipt,verificationStdout='';
    await assert.rejects(withAssistantLane(base,'preparation',async home=>{
      await fs.writeFile(path.join(home,'auth.json'),'{"apiKey":"SYNTHETIC_DO_NOT_COPY"}');
      await fs.writeFile(path.join(home,'config.toml'),'token="SYNTHETIC_DO_NOT_COPY"');
      try{
        await runUrlVerificationAttempt({home,cli:'synthetic',captureStdout:chunk=>{verificationStdout+=chunk;}},
          {input:'SYNTHETIC verification context',instructions:'offline fixture',schema:{},deadline:performance.now()+900,remainingCalls:null,checkTrace:()=>{}},
          {runProcessFn:(_cli,args,options)=>{
            const result=args[args.indexOf('--output-last-message')+1];
            const script=`require('node:fs').writeFileSync(${JSON.stringify(result)},JSON.stringify({text:'Paid verification candidate'}));console.log(JSON.stringify({type:'turn.started'}));setInterval(()=>{},1000);`;
            return runProcess(process.execPath,['-e',script],options);
          }});
      }catch(failure){
        receipt=await persistAssistantFailureEvidence(path.dirname(home),home,{input:'SYNTHETIC original context',verificationStdout,failure,
          secureDirectory:async directory=>{assert.deepEqual(await fs.readdir(directory),[]);}});
        throw failure;
      }
    }),{code:'ADAPTER_TIMEOUT'});
    assert.match(verificationStdout,/turn.started/);
    assert.deepEqual(receipt.files.map(f=>f.name).sort(),['verification.events.jsonl','verification.response.json']);
    const lane=path.join(base,'preparation');assert.deepEqual(await fs.readdir(lane),['failure-evidence','process-diagnostics']);
    const root=path.join(lane,'failure-evidence'),[directory]=await fs.readdir(root),saved=path.join(root,directory);
    assert.deepEqual((await fs.readdir(saved)).sort(),['receipt.json','verification.events.jsonl','verification.response.json']);
    assert.equal(await fs.readFile(path.join(saved,'verification.response.json'),'utf8'),JSON.stringify({text:'Paid verification candidate'}));
    assert.equal(receipt.admitted,false);assert.equal(receipt.replayAuthorized,false);
    assert.doesNotMatch(await fs.readFile(path.join(saved,'receipt.json'),'utf8'),/Paid verification|SYNTHETIC_DO_NOT_COPY/);
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('URL verification evidence uses the same nested credential filter and fixed file allowlist',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-verification-private-'));
  try{
    const home=path.join(base,'run-owned');await fs.mkdir(home);
    await fs.writeFile(path.join(home,'auth.json'),'{"apiKey":"SYNTHETIC_DO_NOT_COPY"}');
    for(const key of ['apiKey','password','cookie','token']){
      const text=JSON.stringify({[key]:'SYNTHETIC_NEVER_REAL'});
      await fs.writeFile(path.join(home,'verification.response.json'),JSON.stringify({text}));
      const receipt=await persistAssistantFailureEvidence(base,home,{input:'bound',verificationStdout:JSON.stringify({type:'item.completed',item:{type:'agent_message',text}})+'\n',
        failure:{code:'ADAPTER_TIMEOUT'},secureDirectory:async()=>{}});
      assert.deepEqual(receipt.files,[]);assert.deepEqual(receipt.omissions,{sensitive_verification_event:1,sensitive_verification_response:1});
    }
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

test('linked evidence directory is rejected without writing into its target',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-timeout-link-'));
  try{
    const home=path.join(base,'run-owned'),target=path.join(base,'outside');
    await fs.mkdir(home);await fs.mkdir(target);
    await fs.writeFile(path.join(target,'keep.txt'),'unchanged');
    await fs.symlink(target,path.join(base,'failure-evidence'),process.platform==='win32'?'junction':'dir');
    let secured=false;
    assert.equal(await persistAssistantFailureEvidence(base,home,{input:'bound',stdout:'{"safe":true}\n',failure:{code:'ADAPTER_TIMEOUT'},secureDirectory:async()=>{secured=true;}}),null);
    assert.equal(secured,false);assert.deepEqual(await fs.readdir(target),['keep.txt']);
    assert.equal(await fs.readFile(path.join(target,'keep.txt'),'utf8'),'unchanged');
    await fs.unlink(path.join(base,'failure-evidence'));
  }finally{assert(path.resolve(base).startsWith(path.resolve(os.tmpdir())+path.sep));await fs.rm(base,{recursive:true,force:true});}
});

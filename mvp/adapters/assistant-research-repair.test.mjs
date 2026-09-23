import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,admitReviewWithRepair,admitReviewEvidence,admitAssistantEvents,runUrlVerificationAttempt,reviewInstructions,persistResearchDiagnostic,withAssistantLane} from './assistant.mjs';
import {combineResearchTraces,repairInstructionDigest} from './assistant-research-repair.mjs';

const originalUrl='https://manufacturer.example/spec';
const citedUrl=originalUrl+'/';
const source={itemId:'a',url:citedUrl,title:'Specification',claim:'Model Q has an engine'};
const candidate=()=>({text:'Review',sources:[],assessments:[{itemId:'a',outcome:'reply',reason:'Supported by specification',tags:['needs_fact']}],
  proposals:[{itemId:'a',kind:'reply_and_close',text:'This Model Q has an engine.'}],evidence:[{...source}]});
const prepared=()=>prepareAssistantRequest({purpose:'triage_review',account:'likeavto',items:[{id:'a',text:'Does Model Q have an engine?'}],
  posts:[{id:'post',text:'Model Q in market A'}],firstPass:{text:'First',sources:[],proposals:[],
    assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need specification',tags:['needs_fact']}]}});
const initial={calls:4,openedUrls:[originalUrl],completedActivity:[]};
const success={value:{candidateSupported:true,checks:[{evidenceIndex:0,status:'supported'}]},trace:{calls:1,openedUrls:[citedUrl],completedActivity:[]}};
const options=runAttempt=>({originalInstructions:reviewInstructions(),deadline:180000,now:()=>1000,runAttempt});
const event=(id,url=citedUrl,type='item.completed',action='open_page')=>JSON.stringify({type,item:{id,type:'web_search',action:{type:action,url},query:action==='other'?url:undefined}})+'\n';

test('one exact literal verification admits the original unchanged candidate and reproducibly binds both instruction phases',async()=>{
  const value=candidate(),before=structuredClone(value),request=prepared(),originalInput=request.input;
  assert.throws(()=>admitReviewEvidence(value,request,initial),{researchCategory:'UNOBSERVED_URL'});
  let calls=0,attempt;
  const result=await admitReviewWithRepair(value,request,initial,options(async received=>{
    calls++;attempt=received;
    const payload=JSON.parse(received.input);
    assert.equal(payload.originalContext,originalInput);
    assert.deepEqual(payload.requiredEvidence,[{evidenceIndex:0,url:citedUrl}]);
    assert.equal(payload.remainingCalls,4);assert.equal(received.deadline,180000);
    assert.deepEqual(payload.candidate.proposals,before.proposals);
    assert.match(received.instructions,/Do not draft a new answer/);
    return structuredClone(success);
  }));
  assert.equal(calls,1);assert.deepEqual(value,before);assert.equal(request.input,originalInput);
  assert.deepEqual(result.admitted.proposals,before.proposals);assert.equal(result.evidence[0].url,citedUrl);
  assert.equal(result.trace.calls,5);assert.deepEqual(result.trace.openedUrls,[originalUrl,citedUrl]);
  const expected=createHash('sha256').update(JSON.stringify({version:1,phases:[reviewInstructions(),attempt.instructions]})).digest('hex');
  assert.equal(result.verification.instructionSha256,expected);
  assert.equal(repairInstructionDigest(reviewInstructions(),attempt.instructions),expected);
  assert.notEqual(repairInstructionDigest(reviewInstructions()+'x',attempt.instructions),expected);
  assert.notEqual(repairInstructionDigest(reviewInstructions(),attempt.instructions+'x'),expected);
  assert.equal(result.verification.repair.inputSha256,createHash('sha256').update(attempt.input).digest('hex'));
  assert.equal(result.verification.repair.webCalls,1);
});

test('already observed evidence bypasses repair and keeps normal admission',async()=>{
  let calls=0;const result=await admitReviewWithRepair(candidate(),prepared(),success.trace,options(async()=>{calls++;throw Error('unexpected');}));
  assert.equal(calls,0);assert.equal(result.verification,undefined);assert.equal(result.trace.calls,1);
});

test('all fields and recipients validate before repair, including invalid evidence after the unmatched URL',async()=>{
  for(const mutate of [
    value=>value.evidence.push({...source,itemId:'foreign'}),
    value=>value.evidence.push({...source,title:''}),
    value=>value.evidence.push({...source,url:'http://127.0.0.1/private'}),
    value=>value.proposals[0].itemId='foreign',
    value=>value.evidence=Array(31).fill(source),
    value=>value.evidence=[],
  ]){
    const value=candidate();mutate(value);let calls=0;
    await assert.rejects(admitReviewWithRepair(value,prepared(),initial,options(async()=>{calls++;return success;})));
    assert.equal(calls,0);
  }
});

test('anticipated research checks every factual reply before repair even when the first trace has zero calls',async()=>{
  const firstPass={text:'First',sources:[],proposals:[],assessments:['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}))};
  const request=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'},{id:'b'}],firstPass});
  const value=candidate();value.assessments.push({...value.assessments[0],itemId:'b'});value.proposals.push({...value.proposals[0],itemId:'b'});
  let calls=0;
  await assert.rejects(admitReviewWithRepair(value,request,{calls:0,openedUrls:[]},options(async()=>{calls++;return success;})),
    {code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'UNATTRIBUTED_REPLY'});
  assert.equal(calls,0);
  // Context-only admission stays valid; the extra check applies when research is anticipated.
  const contextOnly=candidate();contextOnly.evidence=[];
  const admitted=await admitReviewWithRepair(contextOnly,prepared(),{calls:0,openedUrls:[]},options(async()=>{calls++;return success;}));
  assert.equal(admitted.trace.calls,0);assert.equal(calls,0);
});

test('duplicate URL claims share a literal open but require exact coverage of all evidence rows',async()=>{
  const value=candidate();value.evidence.push({...source,claim:'Second distinct claim'});
  let calls=0;
  const run=async attempt=>{
    calls++;assert.deepEqual(attempt.schema.properties.checks.items.properties.evidenceIndex.enum,[0,1]);
    return {...success,value:{candidateSupported:true,checks:[{evidenceIndex:0,status:'supported'},{evidenceIndex:1,status:'supported'}]}};
  };
  const result=await admitReviewWithRepair(value,prepared(),{...initial,calls:7},options(run));
  assert.equal(calls,1);assert.equal(result.trace.calls,8);assert.equal(result.evidence.length,2);
  await assert.rejects(admitReviewWithRepair(value,prepared(),initial,options(async()=>success)),{code:'ASSISTANT_INVALID_RESEARCH'});
});

test('positive model verdict never substitutes path/query/scheme variants, searches or reference opens',async()=>{
  for(const stdout of [event('w',originalUrl),event('w',citedUrl+'?x=1'),event('w',citedUrl.replace('https:','http:')),
    event('w',citedUrl,'item.started'),event('w',citedUrl,'item.completed','search'),event('w','turn0search0','item.completed','other')]){
    let calls=0;
    await assert.rejects(admitReviewWithRepair(candidate(),prepared(),initial,options(async()=>{
      calls++;return {...success,trace:admitAssistantEvents(stdout,true)};
    })),{code:'ASSISTANT_INVALID_RESEARCH'});
    assert.equal(calls,1);
  }
});

test('unsupported, missing, duplicate, foreign and expanded verdicts fail without a second attempt',async()=>{
  for(const value of [
    {candidateSupported:false,checks:[{evidenceIndex:0,status:'supported'}]},
    {candidateSupported:true,checks:[]},
    {candidateSupported:true,checks:[{evidenceIndex:0,status:'unsupported'}]},
    {candidateSupported:true,checks:[{evidenceIndex:0,status:'unavailable'}]},
    {candidateSupported:true,checks:[{evidenceIndex:1,status:'supported'}]},
    {candidateSupported:true,checks:[{evidenceIndex:'0',status:'supported'}]},
    {candidateSupported:true,checks:[{evidenceIndex:0,status:'supported'},{evidenceIndex:0,status:'supported'}]},
    {candidateSupported:true,checks:[{evidenceIndex:0,status:'supported',url:originalUrl}]},
    {...success.value,proposals:[]}, {...success.value,evidence:[]},
  ]){
    let calls=0;
    await assert.rejects(admitReviewWithRepair(candidate(),prepared(),initial,options(async()=>{calls++;return {...success,value};})),{code:'ASSISTANT_INVALID_RESEARCH'});
    assert.equal(calls,1);
  }
});

test('zero/insufficient budget and expired shared deadline never invoke another process',async()=>{
  for(const [count,secondUrl,clock] of [[8,null,1000],[7,'https://manufacturer.example/another',1000],[4,null,180000]]){
    const value=candidate();if(secondUrl)value.evidence.push({...source,url:secondUrl});let calls=0;
    await assert.rejects(admitReviewWithRepair(value,prepared(),{...initial,calls:count},{...options(async()=>{calls++;return success;}),now:()=>clock}));
    assert.equal(calls,0);
  }
});

test('a repair that overruns the shared deadline cannot admit even positive proof',async()=>{
  let clock=1000;
  await assert.rejects(admitReviewWithRepair(candidate(),prepared(),initial,{...options(async()=>{clock=180000;return success;}),now:()=>clock}),{code:'ADAPTER_TIMEOUT'});
});

test('cross-attempt IDs are not deduplicated and aggregate budget is checked while streaming',async()=>{
  const first=admitAssistantEvents(event('w',originalUrl),true);
  const second=admitAssistantEvents(event('w'),true);
  assert.equal(combineResearchTraces(first,second).calls,2);
  let calls=0;
  await assert.rejects(admitReviewWithRepair(candidate(),prepared(),{...initial,calls:7},options(async attempt=>{
    calls++;attempt.checkTrace({calls:2,openedUrls:[citedUrl]});return success;
  })),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.equal(calls,1);
});

test('cancellation, isolation and runtime failures preserve their category with no retry',async()=>{
  for(const code of ['CANCELLED','ADAPTER_TIMEOUT','ASSISTANT_ISOLATION_FAILED','ASSISTANT_RESEARCH_LIMIT','ASSISTANT_FAILED']){
    let calls=0;
    await assert.rejects(admitReviewWithRepair(candidate(),prepared(),initial,options(async()=>{calls++;throw Object.assign(Error(code),{code});})),{code});
    assert.equal(calls,1);
  }
});

test('input candidate mutation during asynchronous verification cannot replace the frozen proposal or evidence',async()=>{
  const value=candidate(),before=structuredClone(value);
  const result=await admitReviewWithRepair(value,prepared(),initial,options(async()=>{
    value.proposals[0].text='Injected replacement';value.evidence=[];return success;
  }));
  assert.deepEqual(result.admitted.proposals,before.proposals);assert.equal(result.evidence[0].url,citedUrl);
});

test('production attempt wiring uses existing isolation/images, remaining timeout and distinct response file',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'ch-repair-offline-'));
  try {
    const originalOutput=JSON.stringify(candidate());await fs.writeFile(path.join(home,'response.json'),originalOutput);
    const oldVerdict=JSON.stringify(success.value);await fs.writeFile(path.join(home,'verification.response.json'),oldVerdict);
    let calls=0;
    const result=await admitReviewWithRepair(candidate(),prepared(),initial,options(async attempt=>
      runUrlVerificationAttempt({home,cli:'unused-synthetic-cli',imagePaths:['synthetic-comment.png']},attempt,{
        now:()=>1200,runProcessFn:async(cli,args,config)=>{
          calls++;assert.equal(cli,'unused-synthetic-cli');assert.equal(config.timeoutMs,178800);
          assert.equal(args[args.indexOf('--output-last-message')+1],path.join(home,'verification.response.json'));
          assert.ok(args.includes('synthetic-comment.png'));assert.ok(args.includes('shell_tool'));
          assert.ok(args.includes('web_search="live"'));assert.ok(args.includes('--ignore-user-config'));
          assert.equal(await fs.readFile(path.join(home,'verification.instructions.txt'),'utf8'),attempt.instructions);
          await assert.rejects(fs.readFile(path.join(home,'verification.response.json')),{code:'ENOENT'});
          config.onStdout(event('w'));await fs.writeFile(path.join(home,'verification.response.json'),oldVerdict);
          return {stdout:event('w')};
        }
      })));
    assert.equal(calls,1);assert.equal(result.trace.calls,5);
    assert.equal(await fs.readFile(path.join(home,'response.json'),'utf8'),originalOutput);
    // Successful subprocess without a NEW response cannot reuse the old positive verdict.
    await assert.rejects(admitReviewWithRepair(candidate(),prepared(),initial,options(attempt=>
      runUrlVerificationAttempt({home,cli:'unused'},attempt,{now:()=>1000,runProcessFn:async()=>({stdout:event('w')})}))),
      {code:'ASSISTANT_INVALID_RESPONSE',validationCategory:'OUTPUT_JSON'});
  } finally {
    assert.equal(path.dirname(home),os.tmpdir());assert.ok(path.basename(home).startsWith('ch-repair-offline-'));
    await fs.rm(home,{recursive:true,force:true});
  }
});

test('production streaming observer aborts excess and forbidden activity before reading a positive output',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'ch-repair-observer-'));
  try {
    for(const [stdout,expected] of [
      [event('first')+event('second',citedUrl,'item.started'),'ASSISTANT_RESEARCH_LIMIT'],
      [JSON.stringify({type:'item.started',item:{id:'shell',type:'command_execution',command:'must never run'}})+'\n','ASSISTANT_ISOLATION_FAILED'],
    ]) {
      let calls=0,reachedResult=false;
      await assert.rejects(admitReviewWithRepair(candidate(),prepared(),{...initial,calls:7},options(attempt=>
        runUrlVerificationAttempt({home,cli:'unused'},attempt,{now:()=>1000,runProcessFn:async(cli,args,config)=>{
          calls++;config.onStdout(stdout);reachedResult=true;
          await fs.writeFile(path.join(home,'verification.response.json'),JSON.stringify(success.value));
          return {stdout};
        }}))),{code:expected});
      assert.equal(calls,1);assert.equal(reachedResult,false);
    }
  } finally {
    assert.equal(path.dirname(home),os.tmpdir());assert.ok(path.basename(home).startsWith('ch-repair-observer-'));
    await fs.rm(home,{recursive:true,force:true});
  }
});

test('failed verification retains sanitized original and combined diagnostics through private run cleanup',async()=>{
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-repair-receipts-'));
  try {
    const scenarios=[
      {initialCalls:8,status:'budget_exhausted',calls:8,unobserved:1,run:async()=>{throw Error('must not run');}},
      {status:'not_supported',calls:5,unobserved:0,run:async()=>({...success,value:{candidateSupported:false,checks:success.value.checks}})},
      {status:'still_unobserved',calls:5,unobserved:1,run:async()=>({...success,trace:{calls:1,openedUrls:[originalUrl]}})},
      {initialCalls:7,status:'activity_rejected',calls:9,unobserved:0,code:'ASSISTANT_RESEARCH_LIMIT',run:async attempt=>{attempt.checkTrace({calls:2,openedUrls:[citedUrl]});throw Error('must not continue');}},
      {status:'deadline',calls:5,unobserved:0,code:'ADAPTER_TIMEOUT',run:async attempt=>{attempt.checkTrace(success.trace);throw Object.assign(Error('PRIVATE timeout'),{code:'ADAPTER_TIMEOUT'});}},
      {status:'result_invalid',calls:5,unobserved:0,run:async()=>({...success,value:{...success.value,proposals:[]}})},
    ];
    for(const [index,scenario] of scenarios.entries()){
      const local=path.join(base,String(index));await fs.mkdir(local);let observed;
      await assert.rejects(withAssistantLane(local,'preparation',async home=>{
        await fs.writeFile(path.join(home,'private-model-output'),'PRIVATE output');
        try {
          await admitReviewWithRepair(candidate(),prepared(),{...initial,calls:scenario.initialCalls??4},options(scenario.run));
          assert.fail('verification must reject');
        } catch(failure) {
          observed=failure;
          assert.equal(failure.code,scenario.code??'ASSISTANT_INVALID_RESEARCH');
          if(failure.code==='ASSISTANT_INVALID_RESEARCH')assert.equal(failure.researchCategory,'UNOBSERVED_URL');
          assert.equal(failure.researchRepairDiagnostic.status,scenario.status);
          assert.equal(failure.researchDiagnostic.webCalls,scenario.calls);
          assert.equal(failure.researchDiagnostic.unobservedUrlCount,scenario.unobserved);
          assert.equal(failure.researchRepairDiagnostic.original.unobservedUrlCount,1);
          // Receipt projection must independently exclude arbitrary attached diagnostics.
          failure.message='PRIVATE model output';failure.researchDiagnostic.url='https://private.example/secret';
          failure.researchRepairDiagnostic.original.secret='PRIVATE original';
          failure.researchRepairDiagnostic.rawCandidate='PRIVATE candidate';
          assert.equal(await persistResearchDiagnostic(path.dirname(home),failure,'PRIVATE application context',
            'communityhero-drafting-v15-review-exact-url-verification'),true);
          throw failure;
        }
      }),failure=>failure===observed);
      const lane=path.join(local,'preparation');assert.deepEqual(await fs.readdir(lane),['research-diagnostics']);
      const [name]=await fs.readdir(path.join(lane,'research-diagnostics'));
      const raw=await fs.readFile(path.join(lane,'research-diagnostics',name),'utf8');const receipt=JSON.parse(raw);
      assert.equal(receipt.reason,'VERIFICATION_FAILED');assert.equal(receipt.webCalls,scenario.calls);
      assert.equal(receipt.verification.status,scenario.status);assert.equal(receipt.verification.errorCode,scenario.code??'ASSISTANT_INVALID_RESEARCH');
      assert.equal(receipt.verification.original.webCalls,scenario.initialCalls??4);
      assert.equal(receipt.verification.original.unobservedUrlCount,1);
      assert.doesNotMatch(raw,/PRIVATE|https?:|manufacturer|Model Q|secret|rawCandidate/);
      assert.ok(Buffer.byteLength(raw)<=16000);
    }
  } finally {
    assert.equal(path.dirname(base),os.tmpdir());assert.ok(path.basename(base).startsWith('ch-repair-receipts-'));
    await fs.rm(base,{recursive:true,force:true});
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest as prepareCurrentReviewRequest,admitReviewWithRepair,admitReviewEvidence,admitAssistantEvents,runUrlVerificationAttempt,reviewInstructions,persistResearchDiagnostic,withAssistantLane} from './assistant.mjs';
import {combineResearchTraces,repairInstructionDigest,verificationSchema} from './assistant-research-repair.mjs';

// Explicit historical numeric grant: these tests preserve v1 repair semantics.
function prepareAssistantRequest(req) {const p=prepareCurrentReviewRequest(req);p.reviewChunk={version:1,maxWebCalls:8};return p;}

const originalUrl='https://manufacturer.example/spec';
const citedUrl=originalUrl+'/';
const source={itemId:'a',url:citedUrl,title:'Specification',claim:'Model Q has an engine'};
const candidate=()=>({text:'Review',sources:[],assessments:[{itemId:'a',outcome:'reply',reason:'Supported by specification',tags:['needs_fact']}],
  proposals:[{itemId:'a',kind:'reply_and_close',text:'This Model Q has an engine.'}],evidence:[{...source}]});
const prepared=()=>prepareAssistantRequest({purpose:'triage_review',account:'likeavto',items:[{id:'a',text:'Does Model Q have an engine?'}],
  posts:[{id:'post',text:'Model Q in market A'}],firstPass:{text:'First',sources:[],proposals:[],
    assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need specification',tags:['needs_fact']}]}});
const initial={calls:4,openedUrls:[originalUrl],completedActivity:[]};
const verdictFor=(value,globalStatus='valid',status='supported')=>({globalStatus,
  recipients:value.assessments.map(({itemId})=>({itemId,status,evidenceIndices:value.evidence.flatMap((source,index)=>source.itemId===itemId?[index]:[]),dependsOnItemIds:[]})),
  checks:value.evidence.map((_,evidenceIndex)=>({evidenceIndex,status}))});
const success={value:verdictFor(candidate()),trace:{calls:1,openedUrls:[citedUrl],completedActivity:[]}};
const successFor=attempt=>({...success,value:verdictFor(JSON.parse(attempt.input).candidate)});
const options=runAttempt=>({originalInstructions:reviewInstructions(),deadline:180000,now:()=>1000,runAttempt});
const event=(id,url=citedUrl,type='item.completed',action='open_page')=>JSON.stringify({type,item:{id,type:'web_search',action:{type:action,url},query:action==='other'?url:undefined}})+'\n';

const mixedBatch=()=>{
  const request=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'},{id:'b'},{id:'c'}],firstPass:{
    text:'First',sources:[],proposals:[{itemId:'c',kind:'close',text:''}],assessments:[
      ...['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']})),
      {itemId:'c',outcome:'close',reason:'No response needed',tags:[]},
    ]}});
  const value=candidate();value.evidence=[{...source,itemId:'b'}];
  value.assessments.push({itemId:'b',outcome:'reply',reason:'Supported separately',tags:['needs_fact']},
    {itemId:'c',outcome:'close',reason:'No response needed',tags:[]});
  value.proposals.push({itemId:'b',kind:'reply_and_close',text:'Supported reply for b.'},{itemId:'c',kind:'close',text:''});
  return {request,value};
};

const independentBatch=()=>{
  const {request,value}=mixedBatch();
  value.evidence.unshift({...source,url:originalUrl});
  return {request,value};
};
const itemVerdict=(attempt,status='unsupported')=>{
  const value=verdictFor(JSON.parse(attempt.input).candidate);
  value.checks=[{evidenceIndex:1,status}];
  value.recipients.find(item=>item.itemId==='b').status=status;
  return value;
};

test('complete item verdict preserves independent original reply and close while holding the bad recipient',async()=>{
  for(const status of ['unsupported','unavailable']) {
    const {request,value}=independentBatch(),before=structuredClone(value),requestBefore=structuredClone(request);
    let calls=0;
    const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>{
      calls++;
      assert.deepEqual(attempt.schema.properties.checks.items.properties.evidenceIndex.enum,[1]);
      assert.deepEqual(attempt.schema.properties.recipients.items.properties.evidenceIndices.items.enum,[0,1]);
      assert.deepEqual(JSON.parse(attempt.input).requiredEvidence,[{evidenceIndex:1,url:citedUrl}]);
      assert.match(attempt.instructions,/do not reopen them/);
      return {value:itemVerdict(attempt,status),trace:{calls:1,openedUrls:status==='unavailable'?[]:[citedUrl]}};
    }));
    assert.equal(calls,1);assert.equal(result.trace.calls,5);
    assert.deepEqual(value,before);assert.deepEqual(request,requestBefore);
    assert.deepEqual(result.admitted.proposals,[before.proposals[0],before.proposals[2]]);
    assert.deepEqual(result.admitted.assessments.map(item=>item.outcome),['reply','needs_attention','close']);
    assert.deepEqual(result.evidence.map(item=>item.itemId),['a']);
    assert.deepEqual(result.isolatedItemIds,['b']);
    assert.equal(result.verificationRejection.original.webCalls,4);
    assert.equal(result.verificationRejection.observed.webCalls,5);
    assert.equal(result.verification,undefined,'partial evidence must not emit a v1 repair receipt with remapped indices');
    assert.match(result.isolationInstructionSha256,/^[a-f0-9]{64}$/);
    assert.deepEqual(result.verificationRejection.recipients.map(item=>[item.status,item.reason]),
      [['supported','supported'],['held',status],['supported','supported']]);
  }
});

test('shared failed source holds every attributed recipient and retains only independent closure',async()=>{
  const {request,value}=independentBatch();value.evidence[0].url=citedUrl;
  const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>({
    value:{...verdictFor(JSON.parse(attempt.input).candidate),
      recipients:verdictFor(value).recipients.map(item=>({...item,status:item.itemId==='c'?'supported':'unsupported'})),
      checks:[{evidenceIndex:0,status:'unsupported'},{evidenceIndex:1,status:'unsupported'}]},trace:success.trace
  })));
  assert.deepEqual(result.admitted.proposals,[value.proposals[2]]);assert.deepEqual(result.evidence,[]);
  assert.deepEqual(result.isolatedItemIds,['a','b']);
});

test('one shared page cannot be simultaneously unavailable and supporting another recipient',async()=>{
  const {request,value}=independentBatch();value.evidence[0].url=citedUrl;
  await assert.rejects(admitReviewWithRepair(value,request,initial,options(async()=>{
    const verdict=verdictFor(value);
    verdict.checks[1].status='unavailable';verdict.recipients[1].status='unavailable';
    return {...success,value:verdict};
  })),{code:'ASSISTANT_INVALID_RESEARCH',verificationFailure:'result_invalid'});
});

test('recipient verification output satisfies recursive strict schema and exact coverage bounds',()=>{
  const visit=schema=>{
    if(schema.enum)assert.ok(schema.enum.length);
    if(schema.type==='object') {
      assert.equal(schema.additionalProperties,false);
      assert.deepEqual([...schema.required].sort(),Object.keys(schema.properties).sort());
      Object.values(schema.properties).forEach(visit);
    }
    if(schema.type==='array')visit(schema.items);
  };
  for(const schema of [verificationSchema([0],['a']),verificationSchema([1],['a','b','c'],[0,1])]) {
    visit(schema);
    assert.equal(schema.properties.recipients.minItems,schema.properties.recipients.maxItems);
    assert.equal(schema.properties.checks.minItems,schema.properties.checks.maxItems);
  }
});

test('dependent negative decision stays held even if its own sources are supported',async()=>{
  const {request,value}=independentBatch();
  const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>{
    const verdict=itemVerdict(attempt);
    verdict.recipients[2].status='unsupported';verdict.recipients[2].dependsOnItemIds=['b'];
    verdict.recipients[2].evidenceIndices=[1];
    return {...success,value:verdict};
  }));
  assert.deepEqual(result.admitted.proposals,[value.proposals[0]]);
  assert.deepEqual(result.isolatedItemIds,['b','c']);
});

test('literal-open failure propagates through declared decision dependencies',async()=>{
  const {request,value}=independentBatch();
  const result=await admitReviewWithRepair(value,request,initial,options(async()=>{
    const verdict={...verdictFor(value),checks:[{evidenceIndex:1,status:'supported'}]};
    verdict.recipients[0].dependsOnItemIds=['b'];
    verdict.recipients[2].dependsOnItemIds=['a'];
    return {value:verdict,trace:{calls:1,openedUrls:[originalUrl]}};
  }));
  assert.deepEqual(result.admitted.proposals,[]);
  assert.deepEqual(result.evidence,[]);
  assert.deepEqual(result.recipients.map(item=>item.reason),['dependency_held','unobserved_url','dependency_held']);
  assert.equal(result.trace.calls,5);
});

test('missing, duplicate, foreign, cyclic or contradictory recipient verdicts admit nothing',async()=>{
  for(const mutate of [
    value=>delete value.recipients,
    value=>value.recipients.pop(),
    value=>value.recipients[2]={...value.recipients[0]},
    value=>value.recipients[2].itemId='foreign',
    value=>value.recipients[2].status='unknown',
    value=>value.recipients[2].draft='replacement',
    value=>value.recipients[0].evidenceIndices=[],
    value=>value.recipients[0].evidenceIndices=[0,0],
    value=>value.recipients[0].evidenceIndices=[0,99],
    value=>value.recipients[0].dependsOnItemIds=['foreign'],
    value=>value.recipients[0].dependsOnItemIds=['a'],
    value=>value.recipients[0].dependsOnItemIds=['b','b'],
    value=>{value.recipients[0].dependsOnItemIds=['b'];value.recipients[1].dependsOnItemIds=['a'];},
    value=>value.recipients[2].evidenceIndices=[1],
    value=>{value.recipients[1].status='unsupported';value.recipients[0].dependsOnItemIds=['b'];},
    value=>value.checks[0].status='unsupported',
    value=>value.globalStatus='unknown',
    value=>{delete value.globalStatus;value.candidateSupported=true;},
  ]) {
    const {request,value}=independentBatch();let calls=0;
    await assert.rejects(admitReviewWithRepair(value,request,initial,options(async()=>{
      calls++;const verdict={...verdictFor(value),checks:[{evidenceIndex:1,status:'supported'}]};mutate(verdict);
      return {...success,value:verdict};
    })),{code:'ASSISTANT_INVALID_RESEARCH',verificationFailure:'result_invalid'});
    assert.equal(calls,1);
  }
});

test('partial verification persists bounded text-free recipient reasons through diagnostic projection',async()=>{
  const {request,value}=independentBatch();
  const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>({...success,value:itemVerdict(attempt)})));
  const lane=await fs.mkdtemp(path.join(os.tmpdir(),'ch-item-verdict-'));
  try {
    const rejection=result.verificationRejection;
    assert.equal(await persistResearchDiagnostic(lane,{code:'ASSISTANT_INVALID_RESEARCH',researchDiagnostic:rejection.observed,
      researchRepairDiagnostic:{...rejection,recipients:[...rejection.recipients.map(item=>({...item,secret:'PRIVATE'})),
        {itemIdSha256:'PRIVATE',status:'held',reason:'unsupported'}]}},request.input,'communityhero-drafting-v19-review-intent-scoped-evidence'),true);
    const [filename]=await fs.readdir(path.join(lane,'research-diagnostics'));
    const raw=await fs.readFile(path.join(lane,'research-diagnostics',filename),'utf8');
    assert.deepEqual(JSON.parse(raw).verification.recipients,rejection.recipients);
    assert.doesNotMatch(raw,/PRIVATE|https?:|manufacturer|Model Q|engine|secret/);
    assert.ok(Buffer.byteLength(raw)<=16000);
    const diagnostic={...rejection.observed,openedUrlSha256:Array(8).fill('a'.repeat(64)),
      unobserved:Array(30).fill({urlSha256:'b'.repeat(64),comparison:'no_completed_literal_open'}),
      completedActivity:Array(8).fill({action:'open_page',locatorKind:'absolute_url',urlSha256:'c'.repeat(64)})};
    const recipients=Array.from({length:100},(_,index)=>({itemIdSha256:createHash('sha256').update(String(index)).digest('hex'),
      status:'held',reason:'global_ambiguous'}));
    assert.equal(await persistResearchDiagnostic(lane,{code:'ASSISTANT_INVALID_RESEARCH',researchDiagnostic:diagnostic,
      researchRepairDiagnostic:{status:'not_supported',original:diagnostic,recipients}},request.input,
      'communityhero-drafting-v19-review-intent-scoped-evidence'),true);
    const files=await fs.readdir(path.join(lane,'research-diagnostics'));
    const largeRaw=await fs.readFile(path.join(lane,'research-diagnostics',files.find(file=>file!==filename)),'utf8');
    assert.deepEqual(JSON.parse(largeRaw).verification.recipients,recipients);
    assert.ok(Buffer.byteLength(largeRaw)<=32768);
  } finally {
    assert.equal(path.dirname(lane),os.tmpdir());assert.ok(path.basename(lane).startsWith('ch-item-verdict-'));
    await fs.rm(lane,{recursive:true,force:true});
  }
});

test('supported dependency cannot rely on a justified needs-attention decision or a pre-isolated factual hold',async()=>{
  for(const missingAttribution of [false,true]) {
    const {request,value}=independentBatch();
    if(missingAttribution)value.evidence=value.evidence.filter(source=>source.itemId!=='a');
    else {
      value.assessments[0]={itemId:'a',outcome:'needs_attention',reason:'Unknown fact',tags:['needs_fact']};
      value.proposals=value.proposals.filter(proposal=>proposal.itemId!=='a');
    }
    await assert.rejects(admitReviewWithRepair(value,request,initial,options(async attempt=>{
      const verdict=verdictFor(JSON.parse(attempt.input).candidate);
      verdict.checks=JSON.parse(attempt.input).requiredEvidence.map(({evidenceIndex})=>({evidenceIndex,status:'supported'}));
      verdict.recipients.find(item=>item.itemId==='c').dependsOnItemIds=['a'];
      return {...success,value:verdict};
    })),{code:'ASSISTANT_INVALID_RESEARCH',verificationFailure:'result_invalid'});
  }
});

test('one unattributed factual reply is held without losing valid batch decisions or retained sources',async()=>{
  const {request,value}=mixedBatch(),before=structuredClone(value),beforeRequest=structuredClone(request);
  let calls=0;
  const result=await admitReviewWithRepair(value,request,success.trace,options(async()=>{calls++;throw Error('unexpected');}));
  assert.equal(calls,0);assert.deepEqual(result.isolatedItemIds,['a']);
  assert.deepEqual(value,before);assert.deepEqual(request,beforeRequest);
  assert.equal(result.admitted.text,before.text);
  assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
  assert.deepEqual(result.admitted.assessments[0].tags,['needs_fact']);
  assert.match(result.admitted.assessments[0].reason,/не имеет источника/);
  assert.equal(JSON.stringify(result.admitted.assessments.slice(1)),JSON.stringify(before.assessments.slice(1)));
  assert.equal(JSON.stringify(result.admitted.proposals),JSON.stringify(before.proposals.slice(1)));
  assert.deepEqual(result.evidence,[{...before.evidence[0],trust:'source_only'}]);
  assert.equal(result.verification,undefined);assert.equal(result.rejectedSources,undefined);
});

test('multiple unattributed factual replies are all held without fabricating sources',async()=>{
  const {request,value}=mixedBatch();value.evidence=[];
  const result=await admitReviewWithRepair(value,request,success.trace,options(async()=>{throw Error('unexpected');}));
  assert.deepEqual(result.isolatedItemIds,['a','b']);assert.deepEqual(result.evidence,[]);
  assert.deepEqual(result.admitted.proposals,[value.proposals[2]]);
  assert.deepEqual(result.admitted.assessments.map(a=>a.outcome),['needs_attention','needs_attention','close']);
});

test('every original field and recipient validates before attribution isolation can discard a proposal',async()=>{
  for(const mutate of [
    value=>value.proposals[0].text='',
    value=>value.proposals[0].itemId='foreign',
    value=>value.proposals.push({...value.proposals[0]}),
    value=>value.assessments[0].outcome='close',
    value=>value.assessments[0].reason='',
    value=>value.assessments[0].tags=['invalid'],
    value=>value.assessments[1].itemId='a',
    value=>value.assessments.pop(),
    value=>value.evidence.push({...source,itemId:'foreign'}),
    value=>value.evidence.push({...source,url:'http://127.0.0.1/private'}),
    value=>value.evidence.push({...source,title:''}),
    value=>value.evidence.push({...source,claim:''}),
    value=>value.evidence=Array(31).fill({...source,itemId:'b'}),
    value=>value.evidence=null,
  ]) {
    const {request,value}=mixedBatch();mutate(value);let calls=0;
    await assert.rejects(admitReviewWithRepair(value,request,success.trace,options(async()=>{calls++;return success;})),
      failure=>['ASSISTANT_INVALID_RESPONSE','ASSISTANT_INVALID_RESEARCH'].includes(failure.code)
        &&failure.researchCategory!=='UNATTRIBUTED_REPLY');
    assert.equal(calls,0);
  }
});

test('single-recipient missing attribution still fails explicitly, while no-web supplied facts stay valid',async()=>{
  const value=candidate();value.evidence=[];
  await assert.rejects(admitReviewWithRepair(value,prepared(),success.trace,options(async()=>{throw Error('unexpected');})),
    {code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'UNATTRIBUTED_REPLY'});
  const {request,value:batch}=mixedBatch();batch.evidence=[];
  const result=await admitReviewWithRepair(batch,request,{calls:0,openedUrls:[]},options(async()=>{throw Error('unexpected');}));
  assert.deepEqual(result.admitted.proposals,batch.proposals);assert.deepEqual(result.admitted.assessments,batch.assessments);
  assert.equal(result.isolatedItemIds,undefined);
  // Web activity does not impose attribution on replies which never resolved a factual hold.
  request.payload.firstPass.assessments[0].tags=['missing_context'];
  batch.evidence=[{...source,itemId:'b'}];
  const ordinary=await admitReviewWithRepair(batch,request,success.trace,options(async()=>{throw Error('unexpected');}));
  assert.deepEqual(ordinary.admitted.proposals,batch.proposals);assert.equal(ordinary.isolatedItemIds,undefined);
});

test('combined attribution and URL isolation retains both holds and existing rejected-source diagnostics',async()=>{
  for(const exhausted of [false,true]) {
    const {request,value}=mixedBatch(),before=structuredClone(value);let calls=0;
    const trace={calls:exhausted?8:3,openedUrls:[],completedActivity:[]};
    const result=await admitReviewWithRepair(value,request,trace,options(async attempt=>{
      calls++;assert.deepEqual(JSON.parse(attempt.input).candidate.proposals,before.proposals.slice(1));
      return {...successFor(attempt),trace:{calls:1,openedUrls:[originalUrl],completedActivity:[]}};
    }));
    assert.equal(calls,exhausted?0:1);assert.deepEqual(value,before);
    assert.deepEqual(result.isolatedItemIds,['a','b']);
    assert.deepEqual(result.admitted.assessments.map(a=>a.outcome),['needs_attention','needs_attention','close']);
    assert.match(result.admitted.assessments[0].reason,/не имеет источника/);
    assert.match(result.admitted.assessments[1].reason,exhausted?/не был открыт/:/Повторная проверка/);
    assert.deepEqual(result.admitted.assessments[2],before.assessments[2]);
    assert.deepEqual(result.admitted.proposals,[before.proposals[2]]);assert.deepEqual(result.evidence,[]);
    assert.equal(result.rejectedSources.reason,exhausted?'budget_exhausted':'still_unobserved');
    assert.deepEqual(result.rejectedSources.sources.map(source=>source.itemId),['b']);
    assert.equal(result.rejectedSources.webCallsUsed,exhausted?8:4);
    assert.deepEqual(result.trace.openedUrls,[]);
  }
});

test('attribution isolation does not hide fatal URL-repair outcomes or relax the shared deadline',async()=>{
  for(const code of ['CANCELLED','ADAPTER_TIMEOUT','ASSISTANT_ISOLATION_FAILED','ASSISTANT_RESEARCH_LIMIT','ASSISTANT_FAILED']) {
    const {request,value}=mixedBatch();let calls=0;
    await assert.rejects(admitReviewWithRepair(value,request,initial,options(async()=>{
      calls++;throw Object.assign(Error(code),{code});
    })),{code});
    assert.equal(calls,1);
  }
  const {request,value}=mixedBatch();let calls=0;
  await assert.rejects(admitReviewWithRepair(value,request,initial,{...options(async()=>{calls++;return success;}),now:()=>180000}),
    {code:'ADAPTER_TIMEOUT'});
  assert.equal(calls,0);
  await assert.rejects(admitReviewWithRepair(value,request,initial,options(async()=>({...success,
    value:{...success.value,proposals:[]}}))),
    {code:'ASSISTANT_INVALID_RESEARCH',verificationFailure:'result_invalid'});
});

test('complete negative verification holds the whole current chunk and charges the observed call',async()=>{
  for(const globalStatus of ['invalid','ambiguous']) {
    const {request,value}=mixedBatch(),before=structuredClone(value);
    const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>({
      ...success,value:verdictFor(JSON.parse(attempt.input).candidate,globalStatus)})));
    assert.deepEqual(value,before);
    assert.equal(result.trace.calls,5);
    assert.deepEqual(result.trace.openedUrls,[originalUrl,citedUrl]);
    assert.deepEqual(result.admitted.proposals,[]);
    assert.deepEqual(result.evidence,[]);
    assert.deepEqual(result.admitted.assessments.map(a=>a.outcome),['needs_attention','needs_attention','needs_attention']);
    assert.match(result.admitted.assessments[2].reason,/требуется проверка оператором/);
    assert.equal(result.verificationRejection.status,'not_supported');
    assert.equal(result.verificationRejection.negativeScope,'global');
    assert.match(result.isolationInstructionSha256,/^[a-f0-9]{64}$/);
    assert.equal(result.rejectedSources,undefined);
  }
});

test('single-recipient negative at the last reserved call is held with a private bounded receipt',async()=>{
  const request=prepared();
  const result=await admitReviewWithRepair(candidate(),request,{...initial,calls:7},options(async()=>({
    ...success,value:verdictFor(candidate(),'invalid')
  })));
  assert.equal(result.trace.calls,8);assert.deepEqual(result.admitted.proposals,[]);
  assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
  const lane=await fs.mkdtemp(path.join(os.tmpdir(),'ch-negative-verification-'));
  try {
    const rejection=result.verificationRejection;
    assert.equal(await persistResearchDiagnostic(lane,{
      code:'ASSISTANT_INVALID_RESEARCH',researchDiagnostic:rejection.observed,
      researchRepairDiagnostic:{status:rejection.status,negativeScope:rejection.negativeScope,
        negativeStatus:rejection.negativeStatus,original:rejection.original}
    },request.input,'communityhero-drafting-v19-review-intent-scoped-evidence'),true);
    const files=await fs.readdir(path.join(lane,'research-diagnostics'));
    assert.equal(files.length,1);
    const raw=await fs.readFile(path.join(lane,'research-diagnostics',files[0]),'utf8');
    const receipt=JSON.parse(raw);
    assert.equal(receipt.verification.status,'not_supported');
    assert.equal(receipt.verification.negativeScope,'global');
    assert.equal(receipt.verification.negativeStatus,'global');
    assert.equal(receipt.webCalls,8);
    assert.doesNotMatch(raw,/https?:|manufacturer|Model Q|engine/i);
  } finally {await fs.rm(lane,{recursive:true,force:true});}
});

test('negative output cannot hold a chunk after an over-budget verification trace',async()=>{
  await assert.rejects(admitReviewWithRepair(candidate(),prepared(),{...initial,calls:7},
    options(async()=>({value:verdictFor(candidate(),'invalid'),
      trace:{calls:2,openedUrls:[citedUrl],completedActivity:[]}}))),
    failure=>failure.code==='ASSISTANT_RESEARCH_LIMIT'
      &&failure.researchRepairDiagnostic?.status==='activity_rejected');
});

test('valid global negative never preserves another recipient when literal open is still absent',async()=>{
  const {request,value}=mixedBatch();
  const result=await admitReviewWithRepair(value,request,initial,options(async attempt=>({
    value:verdictFor(JSON.parse(attempt.input).candidate,'invalid','unavailable'),
    trace:{calls:1,openedUrls:[originalUrl],completedActivity:[]}
  })));
  assert.equal(result.trace.calls,5);
  assert.deepEqual(result.admitted.proposals,[]);
  assert.deepEqual(result.admitted.assessments.map(a=>a.outcome),['needs_attention','needs_attention','needs_attention']);
  assert.equal(result.verificationRejection.negativeScope,'global');
});

test('malformed negative checks fail before any hold or URL isolation',async()=>{
  const {request,value}=mixedBatch();
  value.evidence.push({...source,itemId:'a',url:'https://manufacturer.example/another'});
  const trace={calls:2,openedUrls:[],completedActivity:[]};
  for(const checks of [
    [{evidenceIndex:0,status:'supported'},{evidenceIndex:0,status:'supported'}],
    [{evidenceIndex:0,status:'supported'},{evidenceIndex:99,status:'unsupported'}],
    [{evidenceIndex:0,status:'supported'},{evidenceIndex:1,status:'wrong'}],
  ]) {
    await assert.rejects(admitReviewWithRepair(value,request,trace,options(async()=>({
      value:{...verdictFor(value,'invalid'),checks},
      trace:{calls:1,openedUrls:[],completedActivity:[]}
    }))),failure=>failure.code==='ASSISTANT_INVALID_RESEARCH'
      &&failure.verificationFailure==='result_invalid');
  }
});

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
  assert.equal(result.verification.repair.version,1);
});

test('already observed evidence bypasses repair and keeps normal admission',async()=>{
  let calls=0;const result=await admitReviewWithRepair(candidate(),prepared(),success.trace,options(async()=>{calls++;throw Error('unexpected');}));
  assert.equal(calls,0);assert.equal(result.verification,undefined);assert.equal(result.trace.calls,1);
});

test('a failed literal verification holds only the unobserved recipient in a batch',async()=>{
  const observedUrl='https://manufacturer.example/observed';
  const firstPass={text:'First',sources:[],proposals:[],assessments:['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}))};
  const request=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'},{id:'b'}],firstPass});
  const value=candidate();
  value.assessments.push({itemId:'b',outcome:'reply',reason:'Observed source',tags:['needs_fact']});
  value.proposals.push({itemId:'b',kind:'reply_and_close',text:'Reply from observed source'});
  value.evidence.push({itemId:'b',url:observedUrl,title:'Observed',claim:'Fact for b'});
  const trace={calls:3,openedUrls:[observedUrl],completedActivity:[]};
  const result=await admitReviewWithRepair(value,request,trace,options(async()=>({
    value:{...verdictFor(value),checks:[{evidenceIndex:0,status:'supported'}]},
    trace:{calls:1,openedUrls:[originalUrl],completedActivity:[]},
  })));
  assert.deepEqual(result.isolatedItemIds,['a']);
  assert.equal(result.trace.calls,4,'verification attempt is still accounted for');
  assert.deepEqual(result.trace.openedUrls,[observedUrl],'URL variant is never admitted');
  assert.deepEqual(result.evidence.map(source=>source.itemId),['b']);
  assert.deepEqual(result.admitted.proposals.map(proposal=>proposal.itemId),['b']);
  assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
  assert.equal(result.admitted.assessments[1].outcome,'reply');
  assert.match(result.isolationInstructionSha256,/^[a-f0-9]{64}$/);
  assert.deepEqual(result.rejectedSources,{
    version:1,reason:'still_unobserved',webCallsUsed:4,webCallsLimit:8,
    openedUrlSha256:[observedUrl,originalUrl].map(url=>createHash('sha256').update(url).digest('hex')),
    sources:[{itemId:'a',candidateUrlSha256:createHash('sha256').update(citedUrl).digest('hex'),comparison:'path_variant',
      openedUrlSha256:createHash('sha256').update(originalUrl).digest('hex')}]
  });
  assert.doesNotMatch(JSON.stringify(result.rejectedSources),/https?:|manufacturer|Specification|engine/i);
});

test('insufficient URL budget holds only unsupported citations without another call',async()=>{
  const request=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'},{id:'b'}],firstPass:{
    text:'First',sources:[],proposals:[],assessments:['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}))}});
  const value=candidate();
  value.assessments.push({itemId:'b',outcome:'reply',reason:'Needs another source',tags:['needs_fact']});
  value.proposals.push({itemId:'b',kind:'reply_and_close',text:'Unverified'});
  value.evidence.push({itemId:'b',url:'https://manufacturer.example/another',title:'Another',claim:'Fact for b'});
  let calls=0;
  const result=await admitReviewWithRepair(value,request,{calls:7,openedUrls:[],completedActivity:[]},
    options(async()=>{calls++;throw Error('must not verify');}));
  assert.equal(calls,0);
  assert.deepEqual(result.isolatedItemIds,['a','b']);
  assert.deepEqual(result.admitted.proposals,[]);
  assert.deepEqual(result.evidence,[]);
  assert.equal(result.rejectedSources.reason,'budget_exhausted');
  assert.equal(result.rejectedSources.webCallsUsed,7);assert.equal(result.rejectedSources.webCallsLimit,8);
  assert.deepEqual(result.rejectedSources.openedUrlSha256,[]);
  assert.deepEqual(result.rejectedSources.sources.map(source=>[source.itemId,source.comparison]),
    [['a','no_completed_literal_open'],['b','no_completed_literal_open']]);
  assert.doesNotMatch(JSON.stringify(result.rejectedSources),/https?:|manufacturer|Specification|engine/i);
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

test('anticipated research isolates an unattributed factual reply before repair even when the first trace has zero calls',async()=>{
  const firstPass={text:'First',sources:[],proposals:[],assessments:['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}))};
  const request=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'},{id:'b'}],firstPass});
  const value=candidate();value.assessments.push({...value.assessments[0],itemId:'b'});value.proposals.push({...value.proposals[0],itemId:'b'});
  let calls=0;
  const result=await admitReviewWithRepair(value,request,{calls:0,openedUrls:[]},options(async attempt=>{
    calls++;
    const repaired=JSON.parse(attempt.input).candidate;
    assert.deepEqual(repaired.proposals,[value.proposals[0]]);
    assert.equal(repaired.assessments[1].outcome,'needs_attention');
    return successFor(attempt);
  }));
  assert.equal(calls,1);
  assert.deepEqual(result.isolatedItemIds,['b']);
  assert.deepEqual(result.admitted.proposals,[value.proposals[0]]);
  // Context-only admission stays valid; the extra check applies when research is anticipated.
  const contextOnly=candidate();contextOnly.evidence=[];
  const admitted=await admitReviewWithRepair(contextOnly,prepared(),{calls:0,openedUrls:[]},options(async()=>{calls++;return success;}));
  assert.equal(admitted.trace.calls,0);assert.equal(calls,1);
});

test('duplicate URL claims share a literal open but require exact coverage of all evidence rows',async()=>{
  const value=candidate();value.evidence.push({...source,claim:'Second distinct claim'});
  let calls=0;
  const run=async attempt=>{
    calls++;assert.deepEqual(attempt.schema.properties.checks.items.properties.evidenceIndex.enum,[0,1]);
    return successFor(attempt);
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

test('missing, duplicate, foreign and expanded verdicts fail without a second attempt',async()=>{
  for(const value of [
    {...success.value,checks:[]},
    {...success.value,checks:[{evidenceIndex:1,status:'supported'}]},
    {...success.value,checks:[{evidenceIndex:'0',status:'supported'}]},
    {...success.value,checks:[{evidenceIndex:0,status:'supported'},{evidenceIndex:0,status:'supported'}]},
    {...success.value,checks:[{evidenceIndex:0,status:'supported',url:originalUrl}]},
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
          if(failure.code==='ASSISTANT_INVALID_RESEARCH')assert.equal(failure.researchCategory,
            ['budget_exhausted','still_unobserved'].includes(scenario.status)?'UNOBSERVED_URL':undefined);
          assert.equal(failure.researchRepairDiagnostic.status,scenario.status);
          assert.equal(failure.researchDiagnostic.webCalls,scenario.calls);
          assert.equal(failure.researchDiagnostic.unobservedUrlCount,scenario.unobserved);
          assert.equal(failure.researchRepairDiagnostic.original.unobservedUrlCount,1);
          // Receipt projection must independently exclude arbitrary attached diagnostics.
          failure.message='PRIVATE model output';failure.researchDiagnostic.url='https://private.example/secret';
          failure.researchRepairDiagnostic.original.secret='PRIVATE original';
          failure.researchRepairDiagnostic.rawCandidate='PRIVATE candidate';
          assert.equal(await persistResearchDiagnostic(path.dirname(home),failure,'PRIVATE application context',
            'communityhero-drafting-v19-review-intent-scoped-evidence'),true);
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

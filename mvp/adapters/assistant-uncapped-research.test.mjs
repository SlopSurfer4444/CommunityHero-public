import test from 'node:test';
import assert from 'node:assert/strict';
import {admitAssistantEvents,admitPublicResearchResult,publicResearchInstructions,reviewInstructions,
  reviewProfile,reviewChunkRequestSha256,validateReviewChunk,prepareAssistantRequest,admitReviewWithRepair,
  outputSchema,assistantCliArgs} from './assistant.mjs';
import {combineResearchTraces} from './assistant-research-repair.mjs';

const sources=()=>Array.from({length:80},(_,i)=>({itemId:'a',url:`https://manufacturer.example/source-${i}`,title:'Primary source',claim:`Observed public fact ${i}`}));
const events=()=>sources().map((s,i)=>JSON.stringify({type:'item.completed',item:{id:`web-${i}`,type:'web_search',action:{type:'open_page',url:s.url}}})).join('\n');
test('new research/review accepts more than fifty events and sources; v1 grants remain enforced',()=>{
  const trace=admitAssistantEvents(events(),true,null);
  assert.equal(trace.calls,80);assert.equal(trace.openedUrls.length,80);assert.equal(trace.webCallLimit,null);
  assert.equal(admitPublicResearchResult({text:'Supported answer',sources:sources()},trace).sources.length,80);
  assert.equal(combineResearchTraces({...trace,calls:80},trace,null).calls,160);
  assert.throws(()=>admitAssistantEvents(events(),true,8),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.throws(()=>combineResearchTraces({...trace,calls:8},{calls:1,openedUrls:[]},8),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.throws(()=>admitPublicResearchResult({text:'Answer',sources:sources()}, {...trace,openedUrls:[]}),{code:'ASSISTANT_INVALID_RESEARCH'});
  assert.throws(()=>admitPublicResearchResult({text:'Answer',sources:sources().map(s=>({...s,claim:'x'.repeat(2000)}))},trace),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.throws(()=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{type:'command_execution'}}),true,null),{code:'ASSISTANT_ISOLATION_FAILED'});
});

test('new profile binds null grant and has no count instruction; editorial remains without web',async()=>{
  const profile=await reviewProfile();assert.equal(profile.version,3);assert.equal(profile.webCallLimit,null);
  const request={purpose:'triage_review',items:[{id:'a'}],firstPass:{text:'First',sources:[],proposals:[],assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}]}};
  const bound={...request,reviewChunk:{version:2,attemptId:'attempt-new',chunkId:'chunk-new',maxWebCalls:null,profileSha256:profile.profileSha256,requestSha256:reviewChunkRequestSha256(request)}};
  assert.deepEqual(validateReviewChunk(bound,profile),bound.reviewChunk);
  assert.throws(()=>validateReviewChunk({...bound,reviewChunk:{...bound.reviewChunk,maxWebCalls:8}},profile),{code:'ASSISTANT_INVALID_REQUEST'});
  for(const instructions of [reviewInstructions(),publicResearchInstructions()]){
    assert.doesNotMatch(instructions,/eight-call|eight calls|eight web tool|at most 3 focused|1-3 sources/);
    assert.match(instructions,/no numerical/);
    assert.match(instructions,/Cite the exact URL you explicitly opened/);
  }
  assert.equal(outputSchema(new Set(['a']),true,true).properties.evidence.maxItems,undefined);
  assert.ok(assistantCliArgs('home',false,[],false,'sol61_high_v2').includes('web_search="disabled"'));
});

test('fresh review repair observes eighty exact opens within the shared deadline and preserves candidate',async()=>{
  const p=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'}],firstPass:{text:'First',sources:[],proposals:[],assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need fact',tags:['needs_fact']}]}});
  const candidate={text:'Review',sources:[],proposals:[{itemId:'a',kind:'reply_and_close',text:'Supported answer.'}],assessments:[{itemId:'a',outcome:'reply',reason:'Sources support it',tags:['needs_fact']}],evidence:sources()};
  const before=structuredClone(candidate);let invoked=0;
  const result=await admitReviewWithRepair(candidate,p,{calls:70,openedUrls:[]},{originalInstructions:reviewInstructions(),deadline:10000,now:()=>1,
    runAttempt:async attempt=>{invoked++;assert.equal(attempt.remainingCalls,null);assert.equal(attempt.deadline,10000);
      assert.match(attempt.instructions,/no numerical web-call limit/);
      return {trace:{calls:80,openedUrls:sources().map(s=>s.url)},value:{globalStatus:'valid',recipients:[{itemId:'a',status:'supported',evidenceIndices:sources().map((_,i)=>i),dependsOnItemIds:[]}],checks:sources().map((_,i)=>({evidenceIndex:i,status:'supported'}))}};}});
  assert.equal(invoked,1);assert.deepEqual(candidate,before);assert.equal(result.trace.calls,150);assert.equal(result.evidence.length,80);
  assert.equal(result.verification.repair.version,2);assert.equal(result.verification.repair.webCalls,80);assert.equal(result.verification.repair.webCallLimit,null);
  await assert.rejects(admitReviewWithRepair(candidate,p,{calls:70,openedUrls:[]},{originalInstructions:reviewInstructions(),deadline:1,now:()=>2,runAttempt:async()=>{throw Error('must not run');}}),{code:'ADAPTER_TIMEOUT'});
});

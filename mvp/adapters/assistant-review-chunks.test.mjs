import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {runAssistant,reviewProfile,reviewChunkRequestSha256,validateReviewChunk,
  prepareAssistantRequest,limitedReviewInstructions,reviewInstructions,admitAssistantEvents} from './assistant.mjs';
import {combineResearchTraces,verifyExactUrls} from './assistant-research-repair.mjs';

const firstPass={text:'First',sources:[],proposals:[],assessments:[
  {itemId:'item-1',outcome:'needs_attention',reason:'Needs fact',tags:['needs_fact']} ]};
const request=()=>({purpose:'triage_review',account:'likeavto',items:[{id:'item-1',text:'Question'}],firstPass});
const bound=(base,profile,maxWebCalls=2)=>({...base,reviewChunk:{version:1,attemptId:'attempt-1',chunkId:'chunk-1',
  profileSha256:profile.profileSha256,requestSha256:reviewChunkRequestSha256(base),maxWebCalls}});
const event=id=>JSON.stringify({type:'item.completed',item:{id,type:'web_search',action:{type:'search'},query:'example'}});
const trace=(calls,openedUrls=[])=>({calls,openedUrls,completedActivity:[]});
const sha256=value=>createHash('sha256').update(value).digest('hex');

test('review profile is local and changes with account',async()=>{
  const previous=process.env.COMMUNITYHERO_RUNTIME_MODE;
  process.env.COMMUNITYHERO_RUNTIME_MODE='portable';
  try {
    const like=await runAssistant({purpose:'review_profile',account:'likeavto'});
    const baw=await reviewProfile('baw-russia');
    assert.equal(like.account,'likeavto');
    assert.equal(baw.account,'baw-russia');
    assert.equal(like.reasoningEffort,'medium');
    for(const field of ['cliSha256','instructionSha256','toolsProfileSha256','runtimeSha256','profileSha256'])
      assert.match(like[field],/^[a-f0-9]{64}$/);
    assert.notEqual(like.instructionSha256,baw.instructionSha256);
    assert.notEqual(like.profileSha256,baw.profileSha256);
  } finally {
    if(previous===undefined)delete process.env.COMMUNITYHERO_RUNTIME_MODE;
    else process.env.COMMUNITYHERO_RUNTIME_MODE=previous;
  }
});

test('chunk binding rejects foreign profile, stale request and malformed contract before model work',async()=>{
  const profile=await reviewProfile('likeavto');
  const good=bound(request(),profile);
  assert.deepEqual(validateReviewChunk(good,profile),good.reviewChunk);
  const prepared=prepareAssistantRequest(good);
  assert.deepEqual(prepared.payload.reviewChunk,good.reviewChunk);
  assert.deepEqual(prepared.reviewChunk,good.reviewChunk);
  const foreign=await reviewProfile('baw-russia');
  assert.throws(()=>validateReviewChunk(good,foreign),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>validateReviewChunk({...good,instruction:'changed'},profile),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({...good,reviewChunk:{...good.reviewChunk,chunkId:'../escape'}}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({...good,reviewChunk:{...good.reviewChunk,maxWebCalls:0}}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({...good,reviewChunk:{...good.reviewChunk,extra:true}}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({...good,purpose:'triage'}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('chunk request still rejects invalid first pass',async()=>{
  const profile=await reviewProfile('likeavto');
  const base={...request(),firstPass:{...firstPass,assessments:[]}};
  assert.throws(()=>prepareAssistantRequest(bound(base,profile)),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('exact Rust request bytes bind equivalent numeric JSON without leaking the duplicate to model context',async()=>{
  const profile=await reviewProfile('likeavto');
  const base={...request(),numericProbe:{scale:1,channels:{'2':'two','10':'ten'}}};
  const raw=JSON.stringify(base).replace('"scale":1','"scale":1.0')
    .replace('"channels":{"2":"two","10":"ten"}','"channels":{"10":"ten","2":"two"}');
  assert.notEqual(raw,JSON.stringify(base));
  assert.equal(JSON.parse(raw).numericProbe.scale,1);
  const supplied={...base,reviewChunkRequestJson:raw,reviewChunk:{version:1,attemptId:'attempt-1',chunkId:'chunk-1',
    profileSha256:profile.profileSha256,requestSha256:sha256(raw),maxWebCalls:2}};
  assert.deepEqual(validateReviewChunk(supplied,profile),supplied.reviewChunk);
  const prepared=prepareAssistantRequest(supplied);
  assert.deepEqual(prepared.payload.reviewChunk,supplied.reviewChunk);
  assert.equal(prepared.payload.reviewChunkRequestJson,undefined);
  assert.equal(prepared.input.includes(raw),false);
  assert.throws(()=>validateReviewChunk({...supplied,reviewChunk:{...supplied.reviewChunk,requestSha256:reviewChunkRequestSha256(base)}},profile),
    {code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>validateReviewChunk({...supplied,reviewChunkRequestJson:raw.replace('Question','Changed')},profile),
    {code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({...base,reviewChunkRequestJson:raw}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('one- and two-call caps govern prompt and event admission',()=>{
  assert.match(limitedReviewInstructions('likeavto',8),/At most eight web tool calls/);
  assert.equal(limitedReviewInstructions('likeavto',null),reviewInstructions('likeavto'));
  assert.match(limitedReviewInstructions('likeavto',1),/At most 1 web tool call total/);
  assert.match(limitedReviewInstructions('likeavto',2),/At most 2 web tool calls total/);
  assert.equal(admitAssistantEvents(event('a'),true,1).calls,1);
  assert.throws(()=>admitAssistantEvents([event('a'),event('b')].join('\n'),true,1),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.equal(admitAssistantEvents([event('a'),event('b')].join('\n'),true,2).calls,2);
  assert.throws(()=>admitAssistantEvents([event('a'),event('b'),event('c')].join('\n'),true,2),{code:'ASSISTANT_RESEARCH_LIMIT'});
});

test('repair cannot exceed the original plus verification cap',async()=>{
  assert.equal(combineResearchTraces(trace(1),trace(1),2).calls,2);
  assert.throws(()=>combineResearchTraces(trace(1),trace(2),2),{code:'ASSISTANT_RESEARCH_LIMIT'});
  const url='https://example.com/fact';
  const base={candidate:{text:'x',assessments:[{itemId:'item-1',outcome:'reply'}]},evidence:[{itemId:'item-1',url,title:'Source',claim:'Fact'}],
    context:'{}',originalInstructions:'Review',deadline:performance.now()+60_000,maxWebCalls:2};
  let calls=0;
  await assert.rejects(()=>verifyExactUrls({...base,trace:trace(2),runAttempt:async()=>{calls++;return {};}}),
    failure=>failure.verificationFailure==='budget_exhausted');
  assert.equal(calls,0);
  await assert.rejects(()=>verifyExactUrls({...base,trace:trace(1),runAttempt:async attempt=>{
    calls++;
    assert.equal(attempt.remainingCalls,1);
    return {trace:trace(2,[url]),value:{globalStatus:'valid',
      recipients:[{itemId:'item-1',status:'supported',evidenceIndices:[0],dependsOnItemIds:[]}],checks:[{evidenceIndex:0,status:'supported'}]}};
  }}),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.equal(calls,1);
});

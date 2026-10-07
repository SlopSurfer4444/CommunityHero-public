import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,generationMetadata,prepareAssistantRequest,reviewInstructions} from './assistant.mjs';

const binding={id:'angryspace-baw-russia-v1',workspaceId:'local-pilot',accountId:'BAW Russia',
  connector:'angryspace',revision:1,providerAccountId:'baw-russia'};
const hash=character=>character.repeat(64);
const policy=(mode='full_audio_visual')=>({version:1,mode,fullAudioRequired:true,
  visualRequired:mode==='full_audio_visual',ownerAuthorizedAudioOnly:mode==='full_audio_only',
  account:'BAW Russia',connectorBinding:structuredClone(binding),sourceVersion:hash('a'),policySha256:hash('b')});
const request=()=>({account:'baw-russia',purpose:'triage',connectorBinding:structuredClone(binding),
  items:[{id:'audio-item',postId:'audio-post',postKey:'vk:audio'},{id:'visual-item',postId:'visual-post',postKey:'vk:visual'}],
  posts:[
    {id:'audio-post',postKey:'vk:audio',mediaPolicy:policy('full_audio_only'),visualContextStatus:'missing',secret:'drop'},
    {id:'visual-post',postKey:'vk:visual',mediaPolicy:policy(),visualContextStatus:'complete'},
    {id:'text-post',postKey:'vk:text',visualContextStatus:'not_applicable'},
  ]});

test('typed per-post audio-only exception reaches the model without inventing visual context',()=>{
  const prepared=prepareAssistantRequest(request());
  const audio=prepared.payload.posts[0],visual=prepared.payload.posts[1],text=prepared.payload.posts[2];
  assert.deepEqual(audio.mediaPolicy,policy('full_audio_only'));
  assert.equal(audio.visualContextStatus,'missing');
  assert.equal(visual.mediaPolicy.mode,'full_audio_visual');assert.equal(visual.visualContextStatus,'complete');
  assert.deepEqual(text,{attachmentStatus:'unknown',id:'text-post',postKey:'vk:text',visualContextStatus:'not_applicable'});
  assert.doesNotMatch(prepared.input,/"secret"|"drop"/);
  const changed=request();changed.posts[0].mediaPolicy.policySha256=hash('c');
  assert.notEqual(generationMetadata(prepared.input,true,0,'baw-russia').inputSha256,
    generationMetadata(prepareAssistantRequest(changed).input,true,0,'baw-russia').inputSha256);
});

test('media policy projection rejects mismatched authority, tenant, binding and malformed state',()=>{
  const mutations=[
    r=>delete r.posts[0].visualContextStatus,
    r=>delete r.posts[0].mediaPolicy,
    r=>r.posts[2].mediaPolicy=policy('full_audio_only'),
    r=>r.posts[0].visualContextStatus='not_applicable',
    r=>r.posts[0].mediaPolicy.account='LikeAvto',
    r=>r.posts[0].mediaPolicy.connectorBinding.id='other',
    r=>r.posts[0].mediaPolicy.sourceVersion=hash('A'),
    r=>r.posts[0].mediaPolicy.visualRequired=true,
    r=>r.posts[0].mediaPolicy.ownerAuthorizedAudioOnly=false,
    r=>r.posts[0].mediaPolicy.private='hidden',
    r=>r.posts[0].mediaPolicy.connectorBinding.private='hidden',
  ];
  for(const mutate of mutations){const value=request();mutate(value);
    assert.throws(()=>prepareAssistantRequest(value),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});}
  assert.equal(prepareAssistantRequest({items:[{id:'legacy'}],posts:[{id:'legacy-post'}]}).payload.posts[0].mediaPolicy,undefined);
});

test('drafting and review prompts hold unanswered visual facts under an explicit audio-only exception',()=>{
  for(const instructions of [assistantInstructions(true,'baw-russia'),reviewInstructions('baw-russia')]){
    assert.match(instructions,/full_audio_only with ownerAuthorizedAudioOnly=true/);
    assert.match(instructions,/visual context is\s+unavailable/);
    assert.match(instructions,/depends on appearance, on-screen text, objects,\s+actions or another unanswered visual fact, return needs_attention with missing_context/);
    assert.match(instructions,/Never infer this exception from media type, missing data or another post/);
  }
});

test('probed duration threshold preserves strict 180-second boundary without owner-override claims',()=>{
  for(const durationMs of [179999,180000,180001]){
    const req=request(),p=req.posts[0].mediaPolicy;
    p.mode=durationMs>180000?'full_audio_only':'full_audio_visual';
    p.visualRequired=durationMs<=180000;p.ownerAuthorizedAudioOnly=false;
    p.decisionBasis={kind:'probed_duration_threshold',audioOnlyAboveSeconds:180,durationMs,sourceSha256:hash('c')};
    assert.deepEqual(prepareAssistantRequest(req).payload.posts[0].mediaPolicy,p);
  }
  for(const mutate of [
    p=>p.decisionBasis.durationMs=180000,
    p=>p.decisionBasis.durationMs=null,
    p=>p.decisionBasis.sourceSha256='unknown',
    p=>p.decisionBasis.audioOnlyAboveSeconds=0,
    p=>p.decisionBasis.kind='provider_duration_hint',
    p=>p.ownerAuthorizedAudioOnly=true,
  ]){
    const req=request(),p=req.posts[0].mediaPolicy;p.ownerAuthorizedAudioOnly=false;
    p.decisionBasis={kind:'probed_duration_threshold',audioOnlyAboveSeconds:180,durationMs:180001,sourceSha256:hash('c')};
    mutate(p);assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});
  }
});

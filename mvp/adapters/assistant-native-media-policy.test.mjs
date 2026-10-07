import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {prepareAssistantRequest} from './assistant.mjs';
import {dispatch} from './bridge.mjs';

const hash=character=>character.repeat(64);
const request=(account='likeavto')=>{
  const display=account==='likeavto'?'LikeAvto':'BAW Russia';
  const binding={id:`angryspace-${account}-v1`,workspaceId:'local-pilot',accountId:display,
    connector:'angryspace',revision:1,providerAccountId:account};
  const common={version:1,mode:'full_audio_only',fullAudioRequired:true,visualRequired:false,
    ownerAuthorizedAudioOnly:false,decisionBasis:{kind:'default_full_audio_text'},
    account:display,connectorBinding:binding,sourceVersion:hash('a')};
  return {account,connectorBinding:binding,purpose:'triage',preparationMode:'single_pass_v1',
    responseContract:'compact_decisions_v1',modelContextContract:'shared_moderation_v1',
    researchPolicy:'context_sufficient_v1',decisionMediaContract:'communityhero-decision-media-v1',
    moderationContext:{version:1,account:display,connectorBinding:binding,ruleRefs:[]},
    items:[{id:'item',postId:'post',postKey:'vk:post',moderationCapabilities:{hide:'supported',delete:'supported'}}],
    posts:[{id:'post',postKey:'vk:post',mediaPolicy:{...common,policySha256:hash('b')},
      preparationMediaPolicy:{...common,purpose:'preparation',policySha256:hash('c')},
      visualContextStatus:'missing',decisionMediaEvidence:{sourceVersion:hash('a'),policySha256:hash('c'),
        audioReady:false,visualReady:false,audioProvided:false,visualProvided:false,
        ownerAudioRequired:false,ownerVisualRequired:false}}],
    branches:[],materials:[],knowledgeManifest:[]};
};
const preflight=async value=>{
  const serializedRequest=JSON.stringify(value);
  return dispatch({account:value.account,operation:'assistant_preflight',requests:[{serializedRequest,
    requestSha256:createHash('sha256').update(serializedRequest,'utf8').digest('hex')}]},
  {resolvePaths:()=>{throw Error('provider paths forbidden');},runProcessFn:()=>{throw Error('provider process forbidden');}});
};
const expectInvalid=value=>assert.throws(()=>prepareAssistantRequest(value),
  {code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});

test('native defaults from both companies pass pure preflight without promoting missing media',async()=>{
  for(const account of ['likeavto','baw-russia']){
    const value=request(account),before=structuredClone(value),prepared=prepareAssistantRequest(value);
    assert.deepEqual(prepared.payload.posts[0].mediaPolicy,value.posts[0].mediaPolicy);
    assert.deepEqual(prepared.payload.posts[0].decisionMediaEvidence,value.posts[0].decisionMediaEvidence);
    assert.equal(prepared.payload.posts[0].visualContextStatus,'missing');
    assert.equal((await preflight(value)).results[0].status,'fits');
    assert.deepEqual(value,before);
  }
});

test('native default retains a verified short duration without claiming an owner exception',async()=>{
  for(const durationMs of [179999,180000]){
    const value=request();
    value.posts[0].mediaPolicy.decisionBasis={kind:'default_full_audio_text',audioOnlyAboveSeconds:180,
      durationMs,sourceSha256:hash('d')};
    assert.deepEqual(prepareAssistantRequest(value).payload.posts[0].mediaPolicy,value.posts[0].mediaPolicy);
    assert.equal((await preflight(value)).results[0].status,'fits');
  }
});

test('exact native owner overrides retain audio and visual floors in preflight',async()=>{
  for(const mode of ['full_audio_only','full_audio_visual']){
    const value=request(),post=value.posts[0];
    for(const policy of [post.mediaPolicy,post.preparationMediaPolicy]){
      policy.mode=mode;policy.visualRequired=mode==='full_audio_visual';
      policy.ownerAuthorizedAudioOnly=mode==='full_audio_only';policy.decisionBasis={kind:'exact_owner_override'};
    }
    post.decisionMediaEvidence.ownerAudioRequired=true;
    post.decisionMediaEvidence.ownerVisualRequired=mode==='full_audio_visual';
    const prepared=prepareAssistantRequest(value);
    assert.deepEqual(prepared.payload.posts[0].decisionMediaEvidence,post.decisionMediaEvidence);
    assert.equal(prepared.payload.posts[0].decisionMediaEvidence.audioReady,false);
    assert.equal((await preflight(value)).results[0].status,'fits');
  }
});

test('archived owner policy without decisionBasis and exact duration-threshold policies remain readable',()=>{
  for(const mode of ['full_audio_only','full_audio_visual']){
    const value=request(),p=value.posts[0].mediaPolicy;
    p.mode=mode;p.visualRequired=mode==='full_audio_visual';p.ownerAuthorizedAudioOnly=mode==='full_audio_only';
    delete p.decisionBasis;
    assert.deepEqual(prepareAssistantRequest(value).payload.posts[0].mediaPolicy,p);
  }
  for(const durationMs of [179999,180000,180001]){
    const value=request(),p=value.posts[0].mediaPolicy;
    p.mode=durationMs>180000?'full_audio_only':'full_audio_visual';p.visualRequired=durationMs<=180000;
    p.decisionBasis={kind:'probed_duration_threshold',audioOnlyAboveSeconds:180,durationMs,sourceSha256:hash('d')};
    assert.deepEqual(prepareAssistantRequest(value).payload.posts[0].mediaPolicy,p);
  }
});

test('new native forms reject fabricated authority, cross-account/source and extra evidence',()=>{
  for(const mutate of [
    p=>p.ownerAuthorizedAudioOnly=true,
    p=>{p.mode='full_audio_visual';p.visualRequired=true;},
    p=>p.visualRequired=true,
    p=>p.account='BAW Russia',
    p=>p.connectorBinding={...p.connectorBinding,id:'foreign'},
    p=>p.sourceVersion=hash('A'),
    p=>p.decisionBasis.extra=true,
    p=>p.decisionBasis={kind:'default_full_audio_text',audioOnlyAboveSeconds:180,durationMs:180001,sourceSha256:hash('d')},
    p=>p.decisionBasis={kind:'default_full_audio_text',audioOnlyAboveSeconds:180,durationMs:1,sourceSha256:'unknown'},
    p=>p.decisionBasis={kind:'exact_owner_override',durationMs:1},
    p=>p.decisionBasis={kind:'exact_owner_override'},
    p=>p.decisionBasis={kind:'unknown'},
  ]){const value=request();mutate(value.posts[0].mediaPolicy);expectInvalid(value);}
  const stale=request();stale.posts[0].mediaPolicy.sourceVersion=hash('d');expectInvalid(stale);
});

test('photo posts and absent archived policies require no fabricated video track',async()=>{
  const value=request(),post=value.posts[0];
  delete value.decisionMediaContract;
  for(const key of ['mediaPolicy','preparationMediaPolicy','decisionMediaEvidence'])delete post[key];
  post.visualContextStatus='not_applicable';post.attachments=[{type:'photo',url:'https://cdn.example/photo.png'}];
  const prepared=prepareAssistantRequest(value);
  assert.equal(prepared.payload.posts[0].mediaPolicy,undefined);
  assert.equal((await preflight(value)).results[0].status,'fits');
});

// ROOT supplies an actual native production-policy export and an absolute,
// immutable R4d assistant.mjs closure, imported only for its pure validator.
// ACQUISITION_CAPTURE may supply the genuine current native acquisition policy.
// The historical before/after regression below uses a labelled synthetic policy;
// no current native DTO is relabelled as historical evidence.
const capturePath=process.env.COMMUNITYHERO_NATIVE_MEDIA_POLICY_CAPTURE;
const baselinePath=process.env.COMMUNITYHERO_NATIVE_MEDIA_POLICY_BASELINE;
const acquisitionPath=process.env.COMMUNITYHERO_NATIVE_ACQUISITION_POLICY_CAPTURE;
test('current native policy validates exactly; synthetic historical policy preserves the R4d regression',
  {skip:!capturePath||!baselinePath},async()=>{
    assert.ok(path.isAbsolute(capturePath));assert.ok(path.isAbsolute(baselinePath));
    const captured=JSON.parse(await fs.readFile(capturePath,'utf8')).value.preparationPolicy;
    const capturedBefore=structuredClone(captured);
    assert.ok(['default_full_audio_text','default_full_video_speech'].includes(captured.decisionBasis.kind));
    const value=request(captured.connectorBinding.providerAccountId),post=value.posts[0];
    value.connectorBinding=structuredClone(captured.connectorBinding);
    value.moderationContext.connectorBinding=structuredClone(captured.connectorBinding);
    post.preparationMediaPolicy=structuredClone(captured);
    post.decisionMediaEvidence.sourceVersion=captured.sourceVersion;
    post.decisionMediaEvidence.policySha256=captured.policySha256;
    delete post.mediaPolicy;delete post.visualContextStatus;
    // Genuine current native preparation bytes are checked only against the
    // current validator: R4d predates default_full_video_speech.
    assert.deepEqual(prepareAssistantRequest(value).payload.posts[0].preparationMediaPolicy,captured);
    if(acquisitionPath){
      assert.ok(path.isAbsolute(acquisitionPath));
      const acquisition=JSON.parse(await fs.readFile(acquisitionPath,'utf8')).value;
      assert.ok(['default_full_audio_text','default_full_video_speech'].includes(acquisition.decisionBasis.kind));
      assert.equal(acquisition.sourceVersion,captured.sourceVersion);
      assert.deepEqual(acquisition.connectorBinding,captured.connectorBinding);
      post.mediaPolicy=structuredClone(acquisition);post.visualContextStatus='missing';
      const current=prepareAssistantRequest(value).payload.posts[0];
      assert.deepEqual(current.mediaPolicy,acquisition);
      assert.deepEqual(current.preparationMediaPolicy,captured);
      assert.equal((await preflight(value)).results[0].status,'fits');
    }
    const baseline=await import(pathToFileURL(baselinePath).href);
    // SYNTHETIC historical policy fixture. It reproduces R4d's supported
    // vocabulary and acquisition mismatch; it is not a historical native
    // receipt, and the actual captured DTO above is never rewritten.
    const historical=structuredClone(value),historicalPost=historical.posts[0];
    const historicalPolicy={...structuredClone(captured),
      decisionBasis:{kind:'default_full_audio_text'},policySha256:hash('c')};
    historicalPost.preparationMediaPolicy=historicalPolicy;
    historicalPost.decisionMediaEvidence.policySha256=historicalPolicy.policySha256;
    delete historicalPost.mediaPolicy;delete historicalPost.visualContextStatus;
    assert.deepEqual(baseline.prepareAssistantRequest(historical).payload.posts[0].preparationMediaPolicy,historicalPolicy);
    assert.deepEqual(prepareAssistantRequest(historical).payload.posts[0].preparationMediaPolicy,historicalPolicy);
    historicalPost.mediaPolicy={...structuredClone(historicalPolicy),policySha256:hash('b')};
    delete historicalPost.mediaPolicy.purpose;historicalPost.visualContextStatus='missing';
    assert.throws(()=>baseline.prepareAssistantRequest(historical),
      {code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});
    assert.deepEqual(prepareAssistantRequest(historical).payload.posts[0].mediaPolicy,historicalPost.mediaPolicy);
    assert.equal((await preflight(historical)).results[0].status,'fits');
    assert.deepEqual(captured,capturedBefore,'actual native policy bytes cannot become a synthetic historical capture');
  });

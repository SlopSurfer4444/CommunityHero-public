import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,prepareAssistantRequest,reviewInstructions} from './assistant.mjs';

const sha=character=>character.repeat(64);
const connector={id:'angryspace-baw-russia-v1',workspaceId:'local-pilot',accountId:'BAW Russia',
  connector:'angryspace',revision:1,providerAccountId:'baw-russia'};
const targetVersion=sha('a'),sourceVersion=sha('b'),transcriptHash=sha('c');
const mediaPolicy={version:1,mode:'full_audio_only',fullAudioRequired:true,visualRequired:false,
  ownerAuthorizedAudioOnly:true,account:'BAW Russia',connectorBinding:connector,
  sourceVersion:targetVersion,policySha256:sha('d')};
const edge={match:'owner_confirmed_audio_equivalence',authorization:'owner_confirmed_same_video',
  equivalenceSha256:sha('e'),equivalenceRevision:4,targetPostId:'target',postKey:'12182:target',
  targetSourceVersion:targetVersion,sourcePostId:'source',sourcePostKey:'12185:source',sourceVersion,
  account:'BAW Russia',connectorBinding:connector,
  transcript:{entryId:'entry',versionId:'version',hash:transcriptHash},identities:[],byteEqualityClaimed:false};

function request(){
  return {account:'baw-russia',purpose:'triage',connectorBinding:structuredClone(connector),
    items:[{id:'comment',postId:'target',postKey:'12182:target'}],
    // Real preparation selects the target post only. The source is pinned by the
    // Rust-resolved edge and its attached immutable transcript material.
    posts:[{id:'target',postKey:'12182:target',mediaPolicy:structuredClone(mediaPolicy),visualContextStatus:'missing'}],
    materials:[{id:'speech',account:'BAW Russia',postKey:'12185:source',title:'Full source audio',kind:'transcript',
      trust:'source_only',text:'Complete original source words',knowledgeEntryId:'entry',knowledgeVersionId:'version',
      transcription:{sourceVersion,partial:false,coverage:'full_audio',mediaDurationSeconds:1200,audioDurationSeconds:1200},
      audioEquivalence:[structuredClone(edge)]}],
    knowledgeManifest:[{entryId:'entry',versionId:'version',hash:transcriptHash,kind:'transcript',trust:'source_only',
      mediaBinding:[structuredClone(edge)]}]};
}

test('owner-confirmed directional audio equivalence reaches the model with complete provenance',()=>{
  const req=request();req.materials[0].audioEquivalence[0].private='drop';
  req.knowledgeManifest[0].mediaBinding[0].private='drop';
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'AUDIO_EQUIVALENCE'});

  const {payload,input}=prepareAssistantRequest(request());
  assert.deepEqual(payload.materials[0].audioEquivalence,[edge]);
  assert.deepEqual(payload.knowledgeManifest[0].mediaBinding,[edge]);
  assert.deepEqual(payload.materials[0].transcription,{sourceVersion,partial:false,coverage:'full_audio',
    mediaDurationSeconds:1200,audioDurationSeconds:1200});
  assert.equal(payload.materials[0].text,'Complete original source words');
  assert.equal(payload.posts.length,1);assert.equal(payload.posts[0].id,'target');
  assert.equal(input.includes('sourcePostId'),true);
  assert.equal(input.includes('byteEqualityClaimed'),true);
});

test('prompts keep audio equivalence directional and never turn it into identity or visual evidence',()=>{
  for(const instructions of [assistantInstructions(true,'baw-russia'),reviewInstructions('baw-russia')]){
    assert.match(instructions,/directional owner attestation/);
    assert.match(instructions,/not byte equality,\s+shared media identity, visual evidence/);
    assert.match(instructions,/Do not transfer visual observations, infer identities, or apply it to another post/);
    assert.match(instructions,/return needs_attention with missing_context/);
  }
});

test('audio equivalence rejects tenant, authority, target, source and transcript drift',()=>{
  const mutations=[
    r=>r.materials[0].audioEquivalence[0].authorization='model_inferred_same_video',
    r=>r.materials[0].audioEquivalence[0].account='LikeAvto',
    r=>r.materials[0].audioEquivalence[0].connectorBinding.id='foreign',
    r=>r.materials[0].audioEquivalence[0].targetPostId='other',
    r=>r.materials[0].audioEquivalence[0].postKey='12182:other',
    r=>r.materials[0].audioEquivalence[0].targetSourceVersion=sha('f'),
    r=>r.materials[0].audioEquivalence[0].sourcePostId='target',
    r=>r.materials[0].audioEquivalence[0].sourcePostKey='12182:target',
    r=>r.materials[0].audioEquivalence[0].sourceVersion=sha('f'),
    r=>r.materials[0].audioEquivalence[0].transcript.entryId='other',
    r=>r.materials[0].audioEquivalence[0].transcript.versionId='other',
    r=>r.materials[0].audioEquivalence[0].transcript.hash=sha('f'),
    r=>r.materials[0].audioEquivalence[0].identities=['yt:invented'],
    r=>r.materials[0].audioEquivalence[0].byteEqualityClaimed=true,
    r=>r.materials[0].audioEquivalence[0].equivalenceRevision=0,
    r=>r.materials[0].postKey='12185:other',
    r=>delete r.materials[0].account,
    r=>r.materials[0].text=' ',
    r=>r.materials[0].trust='verified',
    r=>r.materials[0].transcription.partial=true,
    r=>r.materials[0].transcription.coverage='partial',
    r=>r.materials[0].transcription.sourceVersion=sha('f'),
    r=>r.materials[0].transcription.audioDurationSeconds=1199,
    r=>{r.posts[0].mediaPolicy.mode='full_audio_visual';r.posts[0].mediaPolicy.visualRequired=true;
      r.posts[0].mediaPolicy.ownerAuthorizedAudioOnly=false;},
    r=>r.posts[0].mediaPolicy.sourceVersion=sha('f'),
  ];
  for(const mutate of mutations){
    const req=request();mutate(req);
    assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'AUDIO_EQUIVALENCE'});
  }
});

test('material and manifest edges must be exact, paired and free of unknown fields',()=>{
  const mutations=[
    r=>r.knowledgeManifest[0].mediaBinding[0].equivalenceRevision=5,
    r=>delete r.knowledgeManifest[0].mediaBinding,
    r=>delete r.materials[0].audioEquivalence,
    r=>r.materials[0].audioEquivalence=[],
    r=>r.materials[0].audioEquivalence[0].private='hidden',
    r=>r.knowledgeManifest[0].mediaBinding[0].private='hidden',
    r=>r.materials[0].audioEquivalence[0].connectorBinding.private='hidden',
    r=>r.materials[0].audioEquivalence[0].transcript.private='hidden',
  ];
  for(const mutate of mutations){
    const req=request();mutate(req);
    assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'AUDIO_EQUIVALENCE'});
  }
  const legacy=request();delete legacy.materials[0].audioEquivalence;delete legacy.knowledgeManifest[0].mediaBinding;
  const projected=prepareAssistantRequest(legacy).payload;
  assert.equal(projected.materials[0].audioEquivalence,undefined);
  assert.equal(projected.knowledgeManifest[0].mediaBinding,undefined);
});

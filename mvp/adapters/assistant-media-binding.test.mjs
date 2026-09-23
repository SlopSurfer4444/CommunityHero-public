import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,assistantInstructions,reviewInstructions,generationMetadata} from './assistant.mjs';

const binding={postKey:'target:video',sourcePostKey:'source:video',match:'exact_normalized_title',normalizedTitle:'один точный заголовок',authorization:'account_scoped_exact_title_reuse',identities:[]};
function fixture(){return {account:'likeavto',purpose:'triage',items:[{id:'item',postKey:'target:video'}],posts:[{id:'post',postKey:'target:video',title:'Один точный заголовок'}],
  materials:[{id:'material',account:'LikeAvto',kind:'transcript',postKey:'source:video',text:'Attributed words',knowledgeEntryId:'entry',knowledgeVersionId:'version',transcription:{partial:true,coverage:'unknown'}}],
  knowledgeManifest:[{entryId:'entry',versionId:'version',kind:'transcript',trust:'source_only',hash:'a'.repeat(64),mediaBinding:[structuredClone(binding)]}]};}

test('serialized model input retains exact cross-post attribution without granting factual or coverage authority',()=>{
  const req=fixture();req.knowledgeManifest[0].mediaBinding[0].privateNote='must disappear';
  const {input}=prepareAssistantRequest(req);const payload=JSON.parse(input);
  assert.deepEqual(payload.knowledgeManifest[0].mediaBinding,[binding]);
  assert.equal(payload.materials[0].postKey,'source:video');assert.equal(payload.posts[0].postKey,'target:video');
  assert.equal(payload.materials[0].transcription.partial,true);assert.equal(payload.knowledgeManifest[0].trust,'source_only');
  assert.ok(!input.includes('must disappear'));
  for(const instructions of [assistantInstructions(true),reviewInstructions()]){
    assert.match(instructions,/owner-accepted, account-scoped cross-post reuse/);
    assert.match(instructions,/Neither form verifies the\nsource's factual claims or full coverage/);
    assert.match(instructions,/Do not invent a binding from a similar\nvehicle/);
  }
  assert.equal(generationMetadata(input,true).promptVersion,'communityhero-drafting-v14-imported-rule-semantics');
});

test('shared identity attribution preserves supported identities and removes duplicates',()=>{
  const req=fixture();req.knowledgeManifest[0].mediaBinding=[{postKey:'target:video',sourcePostKey:'source:video',identities:['yt:AbCdEf123_-','sha:'+'a'.repeat(64),'yt:AbCdEf123_-'],private:'discard'}];
  assert.deepEqual(JSON.parse(prepareAssistantRequest(req).input).knowledgeManifest[0].mediaBinding,[{postKey:'target:video',sourcePostKey:'source:video',identities:['yt:AbCdEf123_-','sha:'+'a'.repeat(64)]}]);
});

test('malformed, unbound and foreign attribution never reaches the model',()=>{
  for(const mutate of [
    r=>r.knowledgeManifest[0].mediaBinding[0].postKey='unattached',
    r=>r.knowledgeManifest[0].mediaBinding[0].sourcePostKey='unattached',
    r=>r.knowledgeManifest[0].versionId='another-version',
    r=>r.knowledgeManifest[0].entryId='another-entry',
    r=>r.knowledgeManifest[0].kind='rule',
    r=>r.knowledgeManifest[0].mediaBinding[0].authorization='infer_from_similar_vehicle',
    r=>r.knowledgeManifest[0].mediaBinding[0].normalizedTitle='',
    r=>r.knowledgeManifest[0].mediaBinding[0].identities=['canonical:unrelated'],
    r=>r.knowledgeManifest[0].mediaBinding[0].sourcePostKey='source:\nvideo',
    r=>r.knowledgeManifest[0].mediaBinding=Array(101).fill(binding),
    r=>r.knowledgeManifest[0].mediaBinding=[{postKey:'target:video',sourcePostKey:'source:video',identities:[]}],
    r=>r.knowledgeManifest[0].mediaBinding=[{postKey:'target:video',sourcePostKey:'source:video',identities:['invented narrative']}],
    r=>r.materials[0].account='BAW Russia'
  ]){const req=fixture();mutate(req);assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});}
});

test('unbound old material remains unbound even when a similarly titled target is attached',()=>{
  const req=fixture();delete req.knowledgeManifest[0].mediaBinding;
  assert.equal(JSON.parse(prepareAssistantRequest(req).input).knowledgeManifest[0].mediaBinding,undefined);
});

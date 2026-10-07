import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {DECISION_MEDIA_CONTRACT,prepareAssistantRequest,compactOutputSchema,expandCompactOutput,
  admitSinglePassResult,singlePassMetadata,singlePassInstructions,editorialInstructions,
  editorialOutputSchema,validateEditorialResult,editorialMetadata} from './assistant.mjs';

const sha=value=>createHash('sha256').update(value).digest('hex');
const pass={intent:'pass',companyRules:'pass',factualScope:'pass'};
const independent={audio:'independent',visual:'independent'};
const binding={id:'angryspace-baw-russia-v1',workspaceId:'local-pilot',accountId:'BAW Russia',
  connector:'angryspace',revision:1,providerAccountId:'baw-russia'};
const policy=()=>({version:1,purpose:'preparation',mode:'full_audio_only',fullAudioRequired:true,
  visualRequired:false,ownerAuthorizedAudioOnly:false,decisionBasis:{kind:'default_full_audio_text'},
  account:'BAW Russia',connectorBinding:structuredClone(binding),sourceVersion:sha('source'),policySha256:sha('policy')});
const evidence=()=>({sourceVersion:sha('source'),policySha256:sha('policy'),audioReady:false,visualReady:false,
  audioProvided:false,visualProvided:false,ownerAudioRequired:false,ownerVisualRequired:false});
const request=()=>({account:'baw-russia',connectorBinding:structuredClone(binding),purpose:'triage',
  preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',decisionMediaContract:DECISION_MEDIA_CONTRACT,
  items:['a','b','c'].map(id=>({id,postId:'p',postKey:'vk:p',text:'Спасибо за историю!'})),
  posts:[{id:'p',postKey:'vk:p',preparationMediaPolicy:policy(),decisionMediaEvidence:evidence()}]});
const row=itemId=>({itemId,action:'reply_and_close',text:`Спасибо, ${itemId}!`,reason:'Supplied comment suffices',tags:[],
  editorial:{decision:'accept',reason:'Exact gratitude needs no post speech or scene',checks:{...pass},mediaDependency:{...independent}},
  basis:'context',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[]});
const output=()=>({text:'Prepared exact replies',evidence:[],decisions:['a','b','c'].map(row)});
const trace={calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
const admit=(value,req=request())=>{
  const prepared=prepareAssistantRequest(req);
  return admitSinglePassResult(expandCompactOutput(value,prepared.ids,false,false,false,prepared.decisionMedia),prepared,trace);
};
const candidate=(itemId='a',tagged=true)=>({proposalId:`proposal-${itemId}`,proposalRevision:2,itemId,kind:'reply_and_close',
  text:'Спасибо за историю!',textSha256:sha('Спасибо за историю!'),contextDigest:sha('capture '+itemId),rulesDigest:sha('rules'),
  ...(tagged?{decisionMediaContract:DECISION_MEDIA_CONTRACT,decisionMediaEvidence:[{postId:'p',...evidence()}]}:{})});
const review=(candidates=[candidate()])=>{const req=request();delete req.preparationMode;delete req.responseContract;
  req.purpose='editorial_review';req.editorialCandidates=candidates;return req;};
const verdict=c=>({proposalId:c.proposalId,proposalRevision:c.proposalRevision,itemId:c.itemId,textSha256:c.textSha256,
  contextDigest:c.contextDigest,rulesDigest:c.rulesDigest,decision:'accept',reason:'Exact text only',proposedText:null,
  checks:{...pass},mediaDependency:{...independent}});

test('absent opt-in preserves legacy schema and prompt bytes; new contract changes captured input',()=>{
  const req=request(),legacy=structuredClone(req);delete legacy.decisionMediaContract;
  assert.equal(prepareAssistantRequest(legacy).decisionMedia,false);
  assert.notEqual(sha(prepareAssistantRequest(req).input),sha(prepareAssistantRequest(legacy).input));
  assert.equal(sha(singlePassInstructions()),'de674f4a4981abca1bc8df2dbde65caa0d12fc0a5d210ee736a084506837368a');
  assert.equal(compactOutputSchema(new Set(['a'])).properties.decisions.items.properties.editorial.anyOf[1].properties.mediaDependency,undefined);
  assert.throws(()=>expandCompactOutput(output(),new Set(['a','b','c'])),{code:'ASSISTANT_INVALID_RESPONSE'});
  for(const mutate of [r=>r.decisionMediaContract='unknown',r=>r.purpose='discussion',r=>delete r.responseContract]){
    const changed=request();mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

test('new compact wire requires exact dependency and retains same final text receipt without mutation',()=>{
  const value=output(),before=structuredClone(value),prepared=prepareAssistantRequest(request()),result=admit(value);
  assert.deepEqual(value,before);assert.equal(result.admitted.proposals.length,3);
  assert.deepEqual(result.editorialEvidence.entries[0].mediaDependency,independent);
  assert.equal(result.editorialEvidence.entries[0].textSha256,sha(value.decisions[0].text));
  assert.equal(singlePassMetadata(prepared,result).decisionMediaContract,DECISION_MEDIA_CONTRACT);
  for(const mutate of [v=>delete v.decisions[0].editorial.mediaDependency,
    v=>v.decisions[0].editorial.mediaDependency.audio='optional',
    v=>v.decisions[0].editorial.mediaDependency.extra=true]){
    const changed=output();mutate(changed);assert.throws(()=>admit(changed),{code:'ASSISTANT_INVALID_RESPONSE'});
  }
});

test('missing required or unknown media holds only affected recipients and declared dependents',()=>{
  for(const dependency of [{audio:'required',visual:'independent'},{audio:'independent',visual:'required'},
    {audio:'unknown',visual:'independent'}]){
    const value=output();value.decisions[0].editorial.mediaDependency=dependency;value.decisions[1].dependsOnItemIds=['a'];
    const result=admit(value);assert.deepEqual(result.admitted.proposals.map(p=>p.itemId),['c']);
    assert.deepEqual(result.editorialEvidence.entries.map(p=>p.itemId),['c']);
  }
});

test('ready visual proof is never treated as pixels supplied; current speech must actually be supplied',()=>{
  const req=request();req.posts[0].decisionMediaEvidence.visualReady=true;
  const value=output();value.decisions[0].editorial.mediaDependency.visual='required';
  assert.deepEqual(admit(value,req).admitted.proposals.map(p=>p.itemId),['b','c']);
  req.posts[0].decisionMediaEvidence.audioReady=true;req.posts[0].decisionMediaEvidence.audioProvided=true;
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});
  req.materials=[{id:'transcript',kind:'transcript',postKey:'vk:p',text:'Actual matching speech'}];
  value.decisions[0].editorial.mediaDependency={audio:'required',visual:'independent'};
  assert.equal(admit(value,req).admitted.proposals.length,3);
});

test('owner floor and malformed source captures fail closed without granting action exemptions',()=>{
  const req=request();req.posts[0].preparationMediaPolicy.decisionBasis.kind='exact_owner_override';
  req.posts[0].preparationMediaPolicy.ownerAuthorizedAudioOnly=true;req.posts[0].decisionMediaEvidence.ownerAudioRequired=true;
  assert.equal(admit(output(),req).admitted.proposals.length,0);
  req.posts[0].decisionMediaEvidence.audioReady=true;
  assert.equal(admit(output(),req).admitted.proposals.length,3);
  req.posts[0].preparationMediaPolicy.mode='full_audio_visual';req.posts[0].preparationMediaPolicy.visualRequired=true;
  req.posts[0].preparationMediaPolicy.ownerAuthorizedAudioOnly=false;req.posts[0].decisionMediaEvidence.ownerVisualRequired=true;
  assert.equal(admit(output(),req).admitted.proposals.length,0);
  req.posts[0].decisionMediaEvidence.visualReady=true;
  assert.equal(admit(output(),req).admitted.proposals.length,3);
  for(const mutate of [r=>r.posts[0].decisionMediaEvidence.sourceVersion=sha('foreign'),
    r=>r.posts[0].decisionMediaEvidence.extra=true,r=>r.posts[0].decisionMediaEvidence.visualProvided=true,
    r=>r.posts[0].decisionMediaEvidence.ownerAudioRequired=true,r=>delete r.posts[0].decisionMediaEvidence]){
    const changed=request();mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

test('dedicated and mixed reviews bind exact candidate evidence and require dependency on every row',()=>{
  const candidates=[candidate('a'),candidate('b',false)],req=review(candidates),prepared=prepareAssistantRequest(req);
  assert.deepEqual(prepared.payload.editorialCandidates,candidates);
  const value={text:'Reviewed',sources:[],proposals:[],editorial:candidates.map(verdict)};
  assert.deepEqual(validateEditorialResult(value,prepared).editorial[0].mediaDependency,independent);
  assert.ok(editorialOutputSchema(prepared).properties.editorial.items.required.includes('mediaDependency'));
  assert.equal(editorialMetadata(prepared,editorialInstructions('baw-russia',true)).decisionMediaContract,DECISION_MEDIA_CONTRACT);
  value.editorial[0].mediaDependency.audio='required';
  assert.equal(validateEditorialResult(value,prepared).editorial[0].decision,'hold');
  assert.equal(validateEditorialResult(value,prepared).editorial[1].decision,'accept');
  delete value.editorial[1].mediaDependency;
  assert.throws(()=>validateEditorialResult(value,prepared),{code:'ASSISTANT_INVALID_RESPONSE'});
  for(const mutate of [r=>r.editorialCandidates[0].decisionMediaEvidence=[],
    r=>r.editorialCandidates[0].decisionMediaEvidence[0].postId='foreign',
    r=>r.editorialCandidates[0].decisionMediaEvidence[0].audioReady=true]){
    const changed=review();mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

test('contract instructions distinguish exact text and extracted OCR from unavailable scene evidence',()=>{
  for(const instructions of [singlePassInstructions('baw-russia',true,false,false,false,true),editorialInstructions('baw-russia',true)]){
    assert.match(instructions,/Extracted OCR\s+is supplied text, not viewed pixels/);
    assert.match(instructions,/audioReady AND audioProvided/);
    assert.match(instructions,/Missing ASR alone does not hold a text-independent/);
    assert.match(instructions,/Legacy candidates without this contract gain no media exemption/);
  }
});

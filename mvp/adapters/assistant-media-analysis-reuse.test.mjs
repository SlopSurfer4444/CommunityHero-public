import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest} from './assistant.mjs';
const digest=character=>character.repeat(64);
const stable=value=>JSON.stringify(value,(_,row)=>row&&typeof row==='object'&&!Array.isArray(row)
  ?Object.fromEntries(Object.entries(row).sort(([a],[b])=>a<b?-1:a>b?1:0)):row);
const hash=value=>createHash('sha256').update(stable(value)).digest('hex');
const attachmentIdentity=value=>hash(['type','sourceUrl','source_url','url','id','canonicalMediaId']
  .map(key=>typeof value?.[key]==='string'?value[key]:null));

// Synthetic source-shaped fixtures derived from media_analysis_reuse::bindings.
// They prove JS mechanics; actual native result parity requires ROOT's captured
// fixture file and is separately marked skipped when that evidence is absent.
function fixture(coverage='full_audio') {
  const connectorBinding={id:'likeavto-main',workspaceId:'isolated',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'likeavto'};
  const attachment={type:'video',url:'https://fixture.invalid/exact.mp4'};
  const mediaPolicy={version:1,mode:'full_audio_only',fullAudioRequired:true,visualRequired:false,ownerAuthorizedAudioOnly:false,
    decisionBasis:{kind:'default_full_audio_text'},account:'LikeAvto',connectorBinding,sourceVersion:digest('a'),policySha256:digest('d')};
  const material={id:'donor-audio',account:'LikeAvto',kind:'transcript',trust:'source_only',postKey:'post:donor',mediaSha256:digest('f'),
    knowledgeEntryId:'transcript-entry',knowledgeVersionId:'transcript-version',text:coverage==='full_audio'?'Синтетические исходные слова.':'Звуковая дорожка отсутствует.',
    transcription:{partial:false,sourcePostKey:'post:donor',sourceVersion:digest('b'),coverage,mediaDurationSeconds:120,
      audioDurationSeconds:coverage==='full_audio'?120:null,...(coverage==='no_audio_stream'?{audioStatus:'no_audio_stream'}:{})}};
  const edge={schemaVersion:1,match:'verified_exact_file_analysis_reuse',postKey:'post:target',targetPostId:'target',sourcePostKey:'post:donor',
    companyId:'LikeAvto',connectorBinding,target:{connectorBinding,postId:'target',postKey:'post:target',sourceVersion:digest('a'),
      attachmentIndex:0,attachmentIdentity:attachmentIdentity(attachment),aliasRevision:1},
    verifiedFile:{sha256:digest('f'),bytes:256,receiptSha256:digest('1'),probeSha256:digest('2')},proofSha256:digest('3'),
    resultSha256:digest('4'),specSha256:digest('5'),normalizedOutput:{sha256:digest('6'),bytes:64},
    transcript:{entryId:material.knowledgeEntryId,versionId:material.knowledgeVersionId,hash:digest('7')},
    originalSourceVersion:digest('b'),coverage:{kind:coverage,durationMs:120000},screenReuse:false};
  material.exactFileAnalysisReuse=[edge];
  return {account:'likeavto',connectorBinding,posts:[{id:'target',postKey:'post:target',attachments:[attachment],mediaPolicy,visualContextStatus:'missing'},
    {id:'donor',postKey:'post:donor',attachments:[],text:'Исходный пост'}],materials:[material],
    knowledgeManifest:[{entryId:material.knowledgeEntryId,versionId:material.knowledgeVersionId,kind:'transcript',trust:'source_only',hash:digest('7'),mediaBinding:[edge]}]};
}
function reviewRequest(input,purpose='triage_review') {
  const edge=input.knowledgeManifest.find(entry=>entry.mediaBinding?.some(edge=>edge.match==='verified_exact_file_analysis_reuse')).mediaBinding
    .find(edge=>edge.match==='verified_exact_file_analysis_reuse');
  // Native evidence supplies scoped items, capabilities and their branch. Keep
  // that production boundary intact; only source-shaped unit fixtures need an
  // artificial recipient. First-pass judgments below remain test route data.
  const items=input.items??[{id:'selected',postId:edge.targetPostId,postKey:edge.postKey,branchId:'selected-branch',text:'Содержательный вопрос'}];
  const branches=input.branches??[{id:'selected-branch',postId:edge.targetPostId,messages:[{id:'selected',role:'participant',text:'Содержательный вопрос'}]}];
  return {...input,purpose,items,branches,
    ...(purpose==='triage_review'?{firstPass:{text:'Проверка',sources:[],proposals:[],assessments:items.map(item=>({itemId:item.id,outcome:'needs_attention',tags:['question'],reason:'Нужна проверка точного источника.'}))}}
      :{preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1'})};
}
function verify(input,purpose='triage_review') {
  const req=reviewRequest(input,purpose),before=structuredClone(req),prepared=prepareAssistantRequest(req),model=JSON.parse(prepared.input);
  const original=input.materials.find(material=>material.exactFileAnalysisReuse?.length),projected=model.materials.find(material=>material.id===original.id);
  assert.ok(projected,'exact donor transcript retained for selected target');
  assert.equal(projected.text,original.text);
  assert.equal(projected.mediaSha256,original.mediaSha256);
  assert.equal(projected.transcription.sourceVersion,original.transcription.sourceVersion,'donor sourceVersion not rewritten');
  assert.deepEqual(projected.exactFileAnalysisReuse,original.exactFileAnalysisReuse);
  assert.deepEqual(model.knowledgeManifest.find(entry=>entry.entryId===original.knowledgeEntryId).mediaBinding,
    input.knowledgeManifest.find(entry=>entry.entryId===original.knowledgeEntryId).mediaBinding);
  assert.ok(model.posts.some(post=>post.id===original.exactFileAnalysisReuse[0].targetPostId));
  if(input.items!==undefined){
    assert.deepEqual(req.items,input.items,'native scoped recipient evidence retained');
    assert.deepEqual(req.branches,input.branches,'native branch evidence retained');
    assert.deepEqual(model.items.map(item=>item.id),input.items.map(item=>item.id));
    if(purpose==='triage'&&input.moderationContext){
      assert.deepEqual(model.moderationContext,input.moderationContext);
      for(const item of input.items)assert.deepEqual(model.items.find(row=>row.id===item.id).moderationCapabilities,item.moderationCapabilities);
    }
  }
  if(original.transcription.coverage==='no_audio_stream') {
    assert.equal(projected.transcription.audioStatus,'no_audio_stream');
    assert.equal(projected.transcription.audioDurationSeconds,null);
  }
  assert.deepEqual(req,before,'request and native source pins remain immutable');
  return model;
}
for(const coverage of ['full_audio','no_audio_stream'])test(`source-shaped ${coverage} reaches actual preparation and scoped review input`,()=>verify(fixture(coverage)));
test('request wrapper retains scoped native recipients, capabilities and branch references',()=>{
  const input=fixture(),edge=input.materials[0].exactFileAnalysisReuse[0];
  input.items=[{id:'native-target-item',postId:edge.targetPostId,postKey:edge.postKey,branchId:'native-target-branch',text:'A question about this upload',
    moderationCapabilities:{hide:'unknown',delete:'unknown'}}];
  input.branches=[{id:'native-target-branch',postId:edge.targetPostId,contextComplete:true,messages:[{id:input.items[0].id,role:'participant',text:input.items[0].text}]}];
  input.moderationContext={version:1,account:'LikeAvto',connectorBinding:input.connectorBinding,ruleRefs:[]};
  const before=structuredClone(input);
  for(const purpose of ['triage','triage_review'])verify(input,purpose);
  assert.deepEqual(input,before,'native input is never adapted in place');
  const missingCaps=reviewRequest(structuredClone(input),'triage');delete missingCaps.items[0].moderationCapabilities;
  assert.throws(()=>prepareAssistantRequest(missingCaps),{code:'ASSISTANT_INVALID_REQUEST',message:'Invalid scoped moderation context'});
  const wrongCompany=reviewRequest(structuredClone(input),'triage');wrongCompany.moderationContext.account='BAW Russia';
  assert.throws(()=>prepareAssistantRequest(wrongCompany),{code:'ASSISTANT_INVALID_REQUEST',message:'Invalid scoped moderation context'});
});
test('typed acquisition and preparation policy slots remain distinct across both preparation routes',()=>{
  for(const coverage of ['full_audio','no_audio_stream'])for(const purpose of ['triage','triage_review']){
    const input=fixture(coverage),post=input.posts[0];
    post.preparationMediaPolicy={...structuredClone(post.mediaPolicy),purpose:'preparation'};
    verify(input,purpose);
    const swapped=structuredClone(input);swapped.posts[0].mediaPolicy=structuredClone(swapped.posts[0].preparationMediaPolicy);
    assert.throws(()=>prepareAssistantRequest(reviewRequest(swapped,purpose)),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'},
      'a preparation policy cannot masquerade as the acquisition contract');
    const absentStatus=structuredClone(input);delete absentStatus.posts[0].visualContextStatus;
    assert.throws(()=>prepareAssistantRequest(reviewRequest(absentStatus,purpose)),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});
    if(purpose==='triage'){
      const malformedPreparation=structuredClone(input);malformedPreparation.posts[0].preparationMediaPolicy.purpose='acquisition';
      assert.throws(()=>prepareAssistantRequest(reviewRequest(malformedPreparation,purpose)),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});
    }
  }
});
test('exact ASR reuse preserves no donor OCR or visual authority in the selected target context',()=>{
  const input=fixture();input.materials.push({id:'donor-ocr',kind:'ocr',postKey:'post:donor',text:'DONOR_SCREEN_MUST_NOT_TRANSFER'},
    {id:'donor-scene',kind:'visual_context',postKey:'post:donor',text:'DONOR_SCENE_MUST_NOT_TRANSFER'});
  const model=verify(input);
  assert.ok(!model.materials.some(material=>['donor-ocr','donor-scene'].includes(material.id)));
  assert.equal(model.posts.find(post=>post.id==='target').visualContextStatus,undefined);
  assert.equal(model.videoEvidenceProjection.frameObservationsProvided,false);
});
test('reuse guards reject missing manifest and changed source, coverage, company and attachment pins',()=>{
  const mutations=[
    input=>delete input.knowledgeManifest,
    input=>{input.materials[0].transcription.partial=true;},
    input=>{input.materials[0].transcription.sourceVersion=digest('8');},
    input=>{input.materials[0].mediaSha256=digest('9');},
    input=>{input.materials[0].exactFileAnalysisReuse[0].screenReuse=true;},
    input=>{input.materials[0].exactFileAnalysisReuse[0].companyId='BAW Russia';},
    input=>{input.posts[0].attachments[0].url='https://fixture.invalid/different.mp4';},
    input=>{input.connectorBinding.revision=0;},
    input=>{input.materials[0].exactFileAnalysisReuse[0].normalizedOutput.privateText='MUST_NOT_ENTER_MODEL';}
  ];
  for(const mutation of mutations) {
    const input=fixture(),req=reviewRequest(input);mutation(req);
    assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});
  }
  const silent=fixture('no_audio_stream');silent.materials[0].transcription.audioStatus='unknown';
  assert.throws(()=>prepareAssistantRequest(reviewRequest(silent)),{code:'ASSISTANT_INVALID_REQUEST'});
});
test('explicit locator identity avoids numeric and object-key serializer drift while keeping every locator bound',()=>{
  const input=fixture();
  const attachment=input.posts[0].attachments[0];
  Object.assign(attachment,{sourceUrl:'https://fixture.invalid/a',source_url:'https://fixture.invalid/b',width:1.0,ratio:1e0});
  const edge=input.materials[0].exactFileAnalysisReuse[0];
  edge.target.attachmentIdentity=attachmentIdentity(attachment);
  verify(input);
  for(const key of ['sourceUrl','source_url','url']){
    const changed=structuredClone(input);changed.posts[0].attachments[0][key]='https://fixture.invalid/changed';
    assert.throws(()=>prepareAssistantRequest(reviewRequest(changed)),{code:'ASSISTANT_INVALID_REQUEST'});
  }
  const versionChanged=structuredClone(input);versionChanged.posts[0].mediaPolicy.sourceVersion=digest('8');
  assert.throws(()=>prepareAssistantRequest(reviewRequest(versionChanged)),{code:'ASSISTANT_INVALID_REQUEST'});
});
test('ROOT-captured native exact analysis fixtures cross the real JS preparation boundary',
  {skip:!process.env.COMMUNITYHERO_MEDIA_NATIVE_FIXTURE_FILE},async()=>{
    const fixtures=JSON.parse(await fs.readFile(process.env.COMMUNITYHERO_MEDIA_NATIVE_FIXTURE_FILE,'utf8'));
    assert.ok(Array.isArray(fixtures));assert.equal(fixtures.length,2);
    assert.deepEqual(new Set(fixtures.map(row=>row.coverage)),new Set(['full_audio','no_audio_stream']));
    for(const input of fixtures){
      const edge=input.materials.find(material=>material.exactFileAnalysisReuse?.length).exactFileAnalysisReuse[0];
      const target=input.posts.find(post=>post.id===edge.targetPostId);
      assert.ok(target?.mediaPolicy,'native wrapper must include the acquisition policy');
      assert.equal(target.mediaPolicy.purpose,undefined,'native acquisition policy must not contain preparation purpose');
      assert.equal(target.preparationMediaPolicy?.purpose,'preparation','native wrapper must include the separate preparation policy');
      assert.ok(['missing','complete'].includes(target.visualContextStatus),'native wrapper must include actual lookup readiness');
      for(const purpose of ['triage','triage_review'])verify(input,purpose);
    }
  });

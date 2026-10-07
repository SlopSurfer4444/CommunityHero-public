import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest, generationMetadata} from './assistant.mjs';
import {stageAssistantImages} from './assistant-images.mjs';
import {projectValidatedModelContext} from './assistant-model-context.mjs';

const sha = text => createHash('sha256').update(text).digest('hex');
const assessment = id => ({itemId:id,outcome:'needs_attention',reason:'Check the supplied source',tags:['question']});
function request() {
  return {account:'likeavto',purpose:'triage_review',
    items:[{id:'a',branchId:'ba',postId:'pa',postKey:'post:a',text:'А комплектация?',preview:'А комплектация?',attachments:[]}],
    posts:['a','b','c'].map(id=>({id:`p${id}`,postKey:`post:${id}`,text:`Пост ${id} `+'условия '.repeat(500),
      body:`Пост ${id} `+'условия '.repeat(500),attachments:[]})),
    branches:['a','b','c'].map(id=>({id:`b${id}`,postId:`p${id}`,contextComplete:false,missingParentIds:['unavailable-root'],
      contextTruncated:true,messages:[
        {id:`parent-${id}`,text:'Есть ли подогрев?',role:'participant',attachments:[]},
        {id:`brand-${id}`,parentId:`parent-${id}`,text:'Да, в этой комплектации.',role:'brand',attachments:[]},
        {id,parentId:`brand-${id}`,text:'А комплектация?',role:'participant',attachments:[]},
        {id:`sibling-${id}`,parentId:`parent-${id}`,text:'Это ответ мне?',role:'participant',attachments:[]}
      ]})),
    materials:[{id:'rule',kind:'rule',text:'Не выдумывать условия. Пример ответа не является фактом.'},
      {id:'fact',kind:'reference',postKey:'outside:selected',text:'Проверенный факт компании.'},
      ...['a','b','c'].map(id=>({id:`speech-${id}`,kind:'transcript',postKey:`post:${id}`,knowledgeEntryId:`entry-${id}`,
        knowledgeVersionId:`version-${id}`,text:`Речь ${id}: цена от 2 350 000 ₽, доставка отдельно. `+'речь '.repeat(800),
        transcription:{partial:false,coverage:'full_audio',sourcePostKey:`post:${id}`}})),
      {id:'ocr-a',kind:'ocr',postKey:'post:a',text:'На экране 2 400 000 ₽; часть надписи не читается.'}],
    knowledgeManifest:['a','b','c'].map(id=>({entryId:`entry-${id}`,versionId:`version-${id}`,kind:'transcript',trust:'source_only',hash:sha(id)})),
    firstPass:{text:'Check',sources:[],proposals:[],assessments:[assessment('a')]}};
}

test('current adapter payload baseline shrinks without losing selected branch, facts, ASR or OCR', t=>{
  const req=request(), before=structuredClone(req), prepared=prepareAssistantRequest(req);
  const model=JSON.parse(prepared.input);
  // This is the CURRENT adapter payload after its preexisting bounded attachment
  // and visual projections, not the old archived 386 KB measurement.
  const beforeBytes=Buffer.byteLength(JSON.stringify(prepared.payload));
  const afterBytes=Buffer.byteLength(prepared.input);
  t.diagnostic(`current-projection synthetic three-post review: ${beforeBytes} -> ${afterBytes} UTF-8 bytes; saved ${beforeBytes-afterBytes}`);
  assert.ok(afterBytes < beforeBytes*0.55);
  assert.deepEqual(req,before,'source request is immutable');
  assert.equal(prepared.payload.posts.length,3,'full payload remains for staging/admission');
  assert.equal(prepared.payload.materials.length,6);
  assert.deepEqual(model.items.map(item=>item.id),['a']);
  assert.deepEqual(model.branches,[prepared.payload.branches[0]],'whole branch including siblings, brand reply, parents and missing context');
  assert.deepEqual(model.posts.map(post=>post.id),['pa']);
  assert.deepEqual(model.materials.map(material=>material.id),['rule','fact','speech-a','ocr-a']);
  assert.deepEqual(model.materials,prepared.payload.materials.filter(material=>!['speech-b','speech-c'].includes(material.id)));
  assert.deepEqual(model.knowledgeManifest,[prepared.payload.knowledgeManifest[0]]);
  assert.equal(model.posts[0].body,undefined);assert.equal(model.posts[0].text,req.posts[0].text);
  assert.equal(model.items[0].preview,undefined);assert.equal(model.items[0].text,req.items[0].text);
  assert.deepEqual(model.firstPass,prepared.payload.firstPass);
  assert.equal(generationMetadata(prepared.input,true).inputSha256,sha(prepared.input));
});

test('projection retains differing copies, all company policies and unknown material scope',()=>{
  const req=request();req.items[0].preview='Потенциально важная прежняя версия';req.posts[0].body='Другая существенная оговорка';
  req.materials.push({id:'unknown-source',kind:'transcript',postKey:'not-in-posts',text:'Unresolved association'},
    {id:'item-bound-media',kind:'transcript',postKey:'post:b',itemIds:['a'],text:'Explicitly bound to this recipient despite a different post'},
    {id:'new-kind',kind:'future-evidence',postKey:'post:b',text:'Unknown applicability'},
    {id:'scoped-rule',kind:'rule',postKey:'post:b',text:'Canonical rules select applicability, not this serializer'});
  const prepared=prepareAssistantRequest(req),model=JSON.parse(prepared.input);
  assert.equal(model.items[0].preview,req.items[0].preview);assert.equal(model.posts[0].body,req.posts[0].body);
  for(const id of ['unknown-source','item-bound-media','new-kind','scoped-rule','fact','rule'])assert.ok(model.materials.some(material=>material.id===id));
});

test('original validation rejects tampered unrelated evidence before model scoping',()=>{
  const foreign=request();foreign.materials[3].account='BAW Russia';
  assert.throws(()=>prepareAssistantRequest(foreign),{code:'ASSISTANT_INVALID_REQUEST'});
  const overlong=request();overlong.materials[3].text='x'.repeat(24001);
  assert.throws(()=>prepareAssistantRequest(overlong),{code:'ASSISTANT_CONTEXT_TOO_LARGE'});
});

test('discussion and first-pass scopes stay intact; unresolved legacy review sources fail open',()=>{
  for(const purpose of ['discussion','triage']){
    const req=request();req.purpose=purpose;delete req.firstPass;
    const prepared=prepareAssistantRequest(req),model=JSON.parse(prepared.input);
    assert.equal(model.posts.length,3);assert.equal(model.branches.length,3);assert.equal(model.materials.length,6);
  }
  for(const item of [{id:'a',text:'Legacy'}, {id:'a',branchId:'missing',postId:'pa'}, {id:'a',postKey:'missing'}]){
    const req=request();req.items=[item];const model=JSON.parse(prepareAssistantRequest(req).input);
    assert.equal(model.branches.length,3);assert.equal(model.posts.length,3);assert.equal(model.materials.length,6);
  }
});

test('post-key-only recipients retain all branches belonging to their post',()=>{
  const req=request();delete req.items[0].branchId;delete req.items[0].postId;
  req.branches.push({id:'other-branch-a',postId:'pa',messages:[{id:'old-reply',role:'brand',text:'Уже обсуждали'}]});
  const model=JSON.parse(prepareAssistantRequest(req).input);
  assert.deepEqual(model.branches.map(branch=>branch.id),['ba','other-branch-a']);
  assert.deepEqual(model.posts.map(post=>post.id),['pa']);
});

test('directional media bindings retain exact donor evidence and its source post, not unrelated visuals',()=>{
  // Projection consumes an already validated payload; full equivalence contract
  // validation is additionally covered by assistant-audio-equivalence.test.mjs.
  const payload=prepareAssistantRequest(request()).payload;
  payload.materials[2].postKey='post:b';
  const edge={match:'owner_confirmed_audio_equivalence',targetPostId:'pa',postKey:'post:a',sourcePostId:'pb',
    sourcePostKey:'post:b',byteEqualityClaimed:false,transcript:{entryId:'entry-a',versionId:'version-a',hash:sha('a')}};
  payload.materials[2].audioEquivalence=[edge];payload.knowledgeManifest[0].mediaBinding=[edge];
  payload.materials.push({id:'visual-b',kind:'visual_context',postKey:'post:b',text:'Do not transfer donor visual evidence'});
  const model=projectValidatedModelContext(payload);
  assert.deepEqual(model.posts.map(post=>post.id),['pa','pb']);
  assert.deepEqual(model.materials.find(material=>material.id==='speech-a').audioEquivalence,[edge]);
  assert.deepEqual(model.knowledgeManifest[0].mediaBinding,[edge]);
  assert.ok(!model.materials.some(material=>material.id==='visual-b'));
  assert.ok(!model.materials.some(material=>material.id==='speech-b'),'a donor alias is not permission for another transcript');
});

test('model request order keeps shared evidence before changing recipient IDs',()=>{
  const first=request(),second=request();second.items[0].id='a-next';second.firstPass.assessments=[assessment('a-next')];
  const a=prepareAssistantRequest(first).input,b=prepareAssistantRequest(second).input;
  const sharedEnd=a.indexOf('"purpose"');assert.ok(sharedEnd>1000);
  assert.equal(a.slice(0,sharedEnd),b.slice(0,sharedEnd));
  assert.deepEqual(projectValidatedModelContext(JSON.parse(a)),JSON.parse(a),'projection is idempotent');
});

test('image staging keeps exact original comment/post bytes and compact review context',async t=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-model-context-'));
  t.after(()=>fs.rm(home,{recursive:true,force:true}));
  const bytes=await fs.readFile(new URL('./fixtures/red-2x2.png',import.meta.url));
  const req=request();req.items[0].attachments=[{type:'photo',url:'https://cdn.example/comment.png',sizes:[{irrelevant:'x'.repeat(10000)}]}];
  req.posts[0].attachments=[{type:'photo',url:'https://cdn.example/post.png'}];
  req.posts[1].attachments=[{type:'photo',url:'https://cdn.example/unrelated.png'}];
  const prepared=prepareAssistantRequest(req),downloads=[];
  const images=await stageAssistantImages(prepared,home,{download:async url=>{
    downloads.push(url);return {bytes,mime:'image/png'};
  }});
  assert.deepEqual(downloads,['https://cdn.example/comment.png','https://cdn.example/post.png']);
  const model=JSON.parse(prepared.input);
  assert.deepEqual(model.imageEvidence,prepared.payload.imageEvidence);
  assert.deepEqual(model.imageEvidence.images.map(image=>image.origin),['comment_attachment','post_attachment']);
  for(const image of model.imageEvidence.images)assert.equal(image.sha256,sha(bytes));
  assert.deepEqual(model.imageEvidence.images[1].itemIds,['a']);
  assert.deepEqual(model.posts.map(post=>post.id),['pa']);
  assert.equal(prepared.payload.posts.length,3);
  assert.equal(model.items[0].attachments[0].url,req.items[0].attachments[0].url);
  assert.equal(model.items[0].attachments[0].sizes,undefined,'preexisting attachment projection already removed transport sizes');
  assert.equal(images.paths.length,2);
  for(const file of images.paths)assert.deepEqual(await fs.readFile(file),bytes);
});

test('ordinary video projection omits all scene materials and pins, preserving ASR/OCR and canonical proof',()=>{
  const payload=prepareAssistantRequest(request()).payload;
  payload.materials.push({id:'frames',kind:'visual_context',postKey:'post:a',knowledgeEntryId:'frames-entry',knowledgeVersionId:'frames-version',
    text:'SCENE_SUMMARY_MUST_NOT_BE_SENT',visualEvidence:{schemaVersion:2,modelProjectionVersion:1,
      source:{postKey:'post:a',durationMs:1000},coverage:{kind:'all_frames_fast_selected_neural'},
      aggregate:[{observation:{scene:'DETAILED_SCENE_MUST_NOT_BE_SENT'}}]}});
  payload.knowledgeManifest.push({entryId:'frames-entry',versionId:'frames-version',kind:'visual_context',hash:sha('frames')});
  payload.posts[0].visualContextStatus='complete';
  payload.posts[0].attachments=[{type:'photo',url:'https://cdn.example/direct-photo.jpg'}];
  payload.imageEvidence={status:'attached',images:[{imageNumber:1,origin:'post_attachment',postId:'pa',itemIds:['a'],sha256:sha('actual-photo')}],branchImagesAttached:false};
  const before=structuredClone(payload);
  for(const purpose of ['triage','triage_review','discussion','editorial_review']){
    const model=projectValidatedModelContext({...payload,purpose});
    assert.ok(!model.materials.some(material=>material.kind==='visual_context'));
    assert.ok(!model.knowledgeManifest.some(entry=>entry.kind==='visual_context'||entry.entryId==='frames-entry'));
    assert.ok(!JSON.stringify(model).includes('MUST_NOT_BE_SENT'));
    assert.equal(model.posts[0].visualContextStatus,undefined);
    for(const id of ['speech-a','ocr-a','rule','fact'])assert.deepEqual(model.materials.find(material=>material.id===id),payload.materials.find(material=>material.id===id));
    assert.deepEqual(model.posts[0].attachments,payload.posts[0].attachments);
    assert.deepEqual(model.imageEvidence,payload.imageEvidence);
    assert.equal(model.videoEvidenceProjection.frameObservationsProvided,false);
    assert.match(model.videoEvidenceProjection.visualQuestionPolicy,/needs_attention with missing_context; do not guess/);
    assert.deepEqual(projectValidatedModelContext(model),model,'text-only projection is idempotent');
  }
  assert.deepEqual(payload,before,'canonical observations/proof/readiness remain unchanged');
});

test('missing OCR stays not provided, without manufactured captions or a no-text claim',()=>{
  const payload=prepareAssistantRequest(request()).payload;
  payload.materials=payload.materials.filter(material=>material.kind!=='ocr');
  payload.posts[0].attachments=[{type:'video',url:'https://cdn.example/video.mp4'}];
  const model=projectValidatedModelContext(payload);
  assert.ok(!model.materials.some(material=>material.kind==='ocr'));
  assert.equal(model.videoEvidenceProjection.missingScreenTextStatus,'not_provided');
  assert.match(model.videoEvidenceProjection.missingScreenTextMeaning,/does not establish whether extraction ran, found no text, or failed/);
  assert.equal(model.materials.find(material=>material.id==='speech-a').text,payload.materials.find(material=>material.id==='speech-a').text);
  const invalid=request();invalid.materials.push({id:'bad-proof',kind:'visual_context',postKey:'post:b',text:'Must still validate',visualEvidence:{schemaVersion:2}});
  assert.throws(()=>prepareAssistantRequest(invalid),{code:'ASSISTANT_INVALID_REQUEST'},'omitted video proof is validated before projection');
});

test('photo-only and ambiguous visual materials are retained even when another post contains video',()=>{
  const req=request();req.purpose='triage';delete req.firstPass;
  req.posts[0].attachments=[{type:'photo',url:'https://cdn.example/photo.jpg'}];
  req.posts[1].attachments=[{type:'video',url:'https://cdn.example/video.mp4'}];
  req.materials.push(
    {id:'photo-description',kind:'visual_context',postKey:'post:a',text:'Photo-only observed detail',knowledgeEntryId:'photo-entry',knowledgeVersionId:'photo-version'},
    {id:'ambiguous-description',kind:'visual_context',postKey:'unresolved-source',text:'Unknown source modality',knowledgeEntryId:'unknown-entry',knowledgeVersionId:'unknown-version'});
  req.knowledgeManifest.push(
    {entryId:'photo-entry',versionId:'photo-version',kind:'visual_context',trust:'source_only',hash:sha('photo')},
    {entryId:'unknown-entry',versionId:'unknown-version',kind:'visual_context',trust:'source_only',hash:sha('unknown')});
  const prepared=prepareAssistantRequest(req),model=JSON.parse(prepared.input);
  for(const id of ['photo-description','ambiguous-description'])assert.deepEqual(model.materials.find(material=>material.id===id),prepared.payload.materials.find(material=>material.id===id));
  for(const id of ['photo-entry','unknown-entry'])assert.deepEqual(model.knowledgeManifest.find(entry=>entry.entryId===id),prepared.payload.knowledgeManifest.find(entry=>entry.entryId===id));
  assert.deepEqual(model.posts[0].attachments,prepared.payload.posts[0].attachments);
});

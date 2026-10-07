import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {existsSync} from 'node:fs';

// Portable integration test: defaults to canonical source, and supports the
// owner's isolated adapter closure. No model, network, database or CLI dispatch.
const repo = fileURLToPath(new URL('../../../../../../../..', import.meta.url));
const adjacentDir = path.dirname(fileURLToPath(import.meta.url));
const adapterDir = process.env.CONTEXT_ADAPTER_DIR || (existsSync(path.join(adjacentDir,'assistant.mjs')) ? adjacentDir : path.join(repo, 'mvp/adapters'));
const {prepareAssistantRequest} = await import(pathToFileURL(path.join(adapterDir, 'assistant.mjs')));
const projectorFile = process.env.CONTEXT_PROJECTOR || path.join(adapterDir, 'assistant-model-context.mjs');
const {projectValidatedModelContext} = await import(pathToFileURL(projectorFile));
const sha = text => createHash('sha256').update(text).digest('hex');

// Semantic scenarios from the assignment, synthetic source words and IDs.
// These are transport regressions, not historical request replay or a verdict
// that a language model will reject a particular public response.
const scenarios = [
  {id:'parrot',comment:'С попугаем то что?😂',clarification:'В синтетической ветке автор уточняет, что речь о детали исходного ролика.',
    speech:'В синтетическом полном аудио упоминается выбранная деталь без объяснения её смысла.',
    rejected:'В ролике про него шутят как про обед из дома 😂'},
  {id:'haval',comment:'Честно говоря, до этого видео немного уважал их, но теперь мне стало смешно на них смотреть в Китае, половина народа нищие и им такие автомобили там, за счастье, а он тут разбрасывается, что там сено на них возят хавалы самые качественные.',clarification:'Это синтетическая критика подачи ролика, без запроса оценить автомобиль или владельцев.',
    speech:'Сегодня рассказываем про автомобиль из этого поста, без сравнения надёжности с чужим опытом.',
    rejected:'Про гусей и кукурузу сказали пренебрежительно, формулировка неудачная. Сельские поездки не повод обесценивать машину или её владельцев.'},
  {id:'performance',comment:'По моему, китаец уже с издевкой играет.. переиграли короче',clarification:'Я именно про подачу ведущих: диалог звучит неестественно.',
    speech:'В этом выпуске два ведущих обмениваются репликами про автомобиль.',
    rejected:'Что именно показалось перебором: реплики или манера подачи?'},
  {id:'duty-wish',comment:'И придет власть новая…\nИ отменит утильсбор на машины ввозные…\nИ уберут растаможку полностью…\nДа разрешат в круг тонировочку…аминь🙏🏻\n🤣🤣🤣',clarification:'Это синтетическое уточнение пожелания про пошлины, а не вопрос о скидке.',
    speech:'В видео обсуждается стоимость автомобиля; решения о пошлинах не обещаются.',
    rejected:'Трёх желаний джинна тут уже не хватит 😂'}
];

function request(scenario = scenarios[0], purpose = 'triage_review') {
  const speech = scenario.speech + '\n' + 'Дополнительная речь из полного аудио. '.repeat(350) + '\nASR_END_точная оговорка.';
  const ocr = 'Извлечённый экранный текст: ' + 'Часть строки не читается. '.repeat(250) + '\nOCR_END_источник не доказывает текущую цену.';
  return {account:'likeavto',purpose,
    ...(purpose === 'triage' ? {preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1'} : {}),
    items:[{id:scenario.id,branchId:'selected-branch',postId:'selected-post',postKey:'post:selected',text:scenario.comment,preview:scenario.comment}],
    posts:[{id:'selected-post',postKey:'post:selected',title:'Точный пост выбранного комментария',text:'Синтетическое описание поста.',body:'Синтетическое описание поста.',attachments:[{type:'video',url:'https://fixture.invalid/selected.mp4'}]},
      {id:'other-post',postKey:'post:other',title:'Другой пост',text:'НЕ СВЯЗАНО С ВЫБРАННЫМ КОММЕНТАРИЕМ'}],
    branches:[{id:'selected-branch',postId:'selected-post',contextComplete:false,contextTruncated:true,missingParentIds:['missing-parent'],messages:[
      {id:'root',parentId:'missing-parent',role:'participant',text:'Начало этой ветки отсутствует.'},
      {id:scenario.id,parentId:'root',role:'participant',text:scenario.comment},
      {id:'brand',parentId:scenario.id,role:'brand',text:'Это ранее опубликованная реплика бренда, не независимый факт.'},
      {id:'same-author-clarification',parentId:scenario.id,role:'participant',text:scenario.clarification},
      {id:'sibling',parentId:'root',role:'participant',text:'Соседняя реплика; сохранить порядок и точную принадлежность.'}
    ]},{id:'other-branch',postId:'other-post',contextComplete:true,messages:[{id:'foreign-sibling',role:'participant',text:'НЕ ПЕРЕНОСИТЬ В ВЫБРАННУЮ ВЕТКУ'}]}],
    materials:[{id:'rule',kind:'rule',text:'Отвечать на фактический вклад автора. Не выдумывать детали видео, оценку чужой марки или обязательную шутку.'},
      {id:'asr',kind:'transcript',postKey:'post:selected',text:speech,knowledgeEntryId:'asr-entry',knowledgeVersionId:'asr-version',transcription:{partial:false,coverage:'full_audio',sourcePostKey:'post:selected',fullSourceCoverage:true,videoFramesInspected:false}},
      {id:'ocr',kind:'ocr',postKey:'post:selected',text:ocr,knowledgeEntryId:'ocr-entry',knowledgeVersionId:'ocr-version'},
      {id:'other-asr',kind:'transcript',postKey:'post:other',text:'Только другой пост. Попугай летит, Haval сломан, джинн обещает скидку.'}],
    knowledgeManifest:[{entryId:'asr-entry',versionId:'asr-version',kind:'transcript',trust:'source_only',hash:sha(speech)},
      {entryId:'ocr-entry',versionId:'ocr-version',kind:'ocr',trust:'source_only',hash:sha(ocr)}],
    ...(purpose === 'triage_review' ? {firstPass:{text:scenario.rejected,sources:[],proposals:[],assessments:[{itemId:scenario.id,outcome:'needs_attention',reason:'Rejected response retained for review, not source evidence',tags:[]}]}} : {})};
}

for (const scenario of scenarios) for (const purpose of ['triage','triage_review']) {
  test(`${scenario.id}: ${purpose} passes exact recipient, author clarification, full ASR/OCR and uncertainty to the model`, () => {
    const req = request(scenario,purpose), before = structuredClone(req), prepared = prepareAssistantRequest(req);
    const model = JSON.parse(prepared.input), branch = prepared.payload.branches[0];
    assert.deepEqual(req,before,'source request remains unchanged');
    assert.deepEqual(model.branches[0],branch,'whole exact branch and siblings, not only selected comment');
    assert.equal(model.items[0].text,scenario.comment);
    assert.equal(model.branches[0].messages.find(row=>row.id==='same-author-clarification').text,scenario.clarification);
    for (const id of ['asr','ocr','rule']) assert.deepEqual(model.materials.find(row=>row.id===id),prepared.payload.materials.find(row=>row.id===id));
    assert.ok(model.materials.find(row=>row.id==='asr').text.endsWith('ASR_END_точная оговорка.'));
    assert.ok(model.materials.find(row=>row.id==='ocr').text.endsWith('OCR_END_источник не доказывает текущую цену.'));
    assert.deepEqual(model.knowledgeManifest,prepared.payload.knowledgeManifest);
    assert.equal(prepared.payload.posts.length,2,'full payload stays intact for source identity and admission');
    assert.equal(prepared.payload.branches.length,2);
    assert.deepEqual(projectValidatedModelContext(prepared.payload),model,'seam under test is the same projector used by preparation');
    if (purpose === 'triage_review') {
      assert.deepEqual(model.firstPass,prepared.payload.firstPass,'a rejected draft remains attributed to firstPass');
      assert.deepEqual(model.branches.map(row=>row.id),['selected-branch']);
      assert.deepEqual(model.posts.map(row=>row.id),['selected-post']);
      assert.ok(!model.materials.some(row=>row.id==='other-asr'));
    }
    assert.equal(model.videoEvidenceProjection.frameObservationsProvided,false);
    assert.match(model.videoEvidenceProjection.visualQuestionPolicy,/needs_attention with missing_context; do not guess/);
  });
}

test('changed same-author clarification and final audio sentence reach the actual preparation input', () => {
  const req=request(scenarios[2]),original=JSON.parse(prepareAssistantRequest(req).input);
  req.branches[0].messages[3].text='Я про конкретную фразу, а манера подачи понравилась.';
  req.materials[1].text+='\nНОВАЯ_АУДИО_ОГОВОРКА';
  const changed=JSON.parse(prepareAssistantRequest(req).input);
  assert.notDeepEqual(changed.branches[0],original.branches[0]);
  assert.equal(changed.branches[0].messages[3].text,req.branches[0].messages[3].text);
  assert.equal(changed.materials.find(row=>row.id==='asr').text,req.materials[1].text);
  assert.deepEqual(changed.firstPass,original.firstPass,'context changes cannot silently rewrite the candidate being reviewed');
});

test('partial speech and absent OCR remain explicit unknowns; no visual explanation or screen text is manufactured', () => {
  const req=request();req.materials=req.materials.filter(row=>row.kind!=='ocr');req.knowledgeManifest=req.knowledgeManifest.filter(row=>row.kind!=='ocr');
  Object.assign(req.materials[1].transcription,{partial:true,coverage:'initial_segment',fullSourceCoverage:false});
  const model=JSON.parse(prepareAssistantRequest(req).input);
  assert.deepEqual(model.materials.find(row=>row.id==='asr').transcription,req.materials[1].transcription);
  assert.ok(!model.materials.some(row=>row.kind==='ocr'));
  assert.equal(model.videoEvidenceProjection.missingScreenTextStatus,'not_provided');
  assert.match(model.videoEvidenceProjection.missingScreenTextMeaning,/does not establish whether extraction ran, found no text, or failed/);
  assert.ok(!model.materials.some(row=>row.kind==='visual_context'));
});

test('validated video frame text/proof is omitted model-only while complete source and exact audio/OCR stay intact', () => {
  const prepared=prepareAssistantRequest(request()),payload=prepared.payload;
  payload.materials.push({id:'frames',kind:'visual_context',postKey:'post:selected',knowledgeEntryId:'visual-entry',knowledgeVersionId:'visual-version',
    text:'SYNTHETIC_SCENE_MUST_NOT_BECOME_VIDEO_FACT',visualEvidence:{schemaVersion:2,modelProjectionVersion:1,
      source:{postKey:'post:selected',durationMs:2000},coverage:{kind:'all_frames_fast_selected_neural'},evidenceSha256:sha('proof'),
      aggregate:[{id:'group-1',observation:{scene:'SYNTHETIC_SCENE_MUST_NOT_BECOME_VIDEO_FACT',text:['INVENTED_CAPTION']},sourceTimestampsMs:[0]}]}});
  payload.knowledgeManifest.push({entryId:'visual-entry',versionId:'visual-version',kind:'visual_context',hash:sha('visual')});
  payload.posts[0].visualContextStatus='complete';
  const original=structuredClone(payload),model=projectValidatedModelContext(payload);
  assert.deepEqual(payload,original);
  assert.ok(!JSON.stringify(model).includes('SYNTHETIC_SCENE_MUST_NOT_BECOME_VIDEO_FACT'));
  assert.ok(!JSON.stringify(model).includes('INVENTED_CAPTION'));
  assert.ok(!model.knowledgeManifest.some(row=>row.entryId==='visual-entry'));
  assert.equal(model.posts[0].visualContextStatus,undefined);
  for(const id of ['asr','ocr']) assert.deepEqual(model.materials.find(row=>row.id===id),payload.materials.find(row=>row.id===id));
  assert.deepEqual(projectValidatedModelContext(model),model,'omission is idempotent');
});

for (const mixed of [false,true]) test(`unresolved direct postId must preserve supplied context (mixed recipients=${mixed})`, () => {
  const req=request();req.items=[{id:'unresolved',postId:'missing-post',text:'О чём эта реплика?'}];
  if(mixed)req.items.push({id:'parrot',branchId:'selected-branch',postId:'selected-post',text:'А попугай на плече зачем?'});
  req.firstPass.assessments=req.items.map(row=>({itemId:row.id,outcome:'needs_attention',reason:'Unresolved source pointers',tags:[]}));
  const prepared=prepareAssistantRequest(req),model=projectValidatedModelContext(prepared.payload);
  assert.deepEqual(model.branches,prepared.payload.branches,'unknown post identity cannot prove attached branches unrelated');
  assert.deepEqual(model.posts,prepared.payload.posts.map(({body,...row})=>row),'only exact duplicate body removal is allowed');
  assert.deepEqual(model.materials,prepared.payload.materials,'unknown post identity cannot prune supplied ASR/OCR');
  assert.deepEqual(model.knowledgeManifest,prepared.payload.knowledgeManifest);
});

test('resolved postKey or branch still scopes safely when the direct postId alias is missing', () => {
  for (const fallback of ['postKey','branchId']) {
    const req=request();req.items[0].postId='missing-post';delete req.items[0][fallback==='postKey'?'branchId':'postKey'];
    const prepared=prepareAssistantRequest(req),model=projectValidatedModelContext(prepared.payload);
    assert.deepEqual(model.posts.map(row=>row.id),['selected-post']);
    assert.deepEqual(model.branches.map(row=>row.id),['selected-branch']);
    assert.ok(!model.materials.some(row=>row.id==='other-asr'));
  }
});

test('full source validation runs before any pruning, including unrelated company evidence and overlong tail', () => {
  const wrongAccount=request();wrongAccount.materials[3].account='BAW Russia';
  assert.throws(()=>prepareAssistantRequest(wrongAccount),{code:'ASSISTANT_INVALID_REQUEST'});
  const tooLong=request();tooLong.materials[3].text='x'.repeat(24001);
  assert.throws(()=>prepareAssistantRequest(tooLong),{code:'ASSISTANT_CONTEXT_TOO_LARGE'});
  const wrongProof=request();wrongProof.materials.push({id:'bad-frame-proof',postKey:'post:other',kind:'visual_context',text:'not evidence',visualEvidence:{schemaVersion:2}});
  assert.throws(()=>prepareAssistantRequest(wrongProof),{code:'ASSISTANT_INVALID_REQUEST'});
});

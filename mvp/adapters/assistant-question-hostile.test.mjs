import test from 'node:test';
import assert from 'node:assert/strict';
import {validateAssistantResult,prepareAssistantRequest,expandCompactOutput,admitSinglePassResult} from './assistant.mjs';
import {preserveUnresolvedSubstantiveQuestions} from './assistant-question-preservation.mjs';
import {projectValidatedModelContext} from './assistant-model-context.mjs';

const blockedReply = () => ({text:'Оценка',sources:[],proposals:[{itemId:'q',kind:'reply_and_close',text:'Да, это всего лишь шутка в видео 🙂'}],
  assessments:[{itemId:'q',outcome:'reply',reason:'Необходимый референт попугая в видео не установлен; без этого содержательный ответ невозможен.',tags:['question','missing_context']}]});

test('an explicit final indispensable context blocker prevents reply_and_close',()=>{
  const value=blockedReply(),result=validateAssistantResult(value,new Set(['q']),true);
  assert.equal(result.assessments[0].outcome,'needs_attention');
  assert.deepEqual(result.proposals,[]);
});

test('single-pass reply_and_close context blocker holds its dependent decisions and proofs',()=>{
  const req={purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1',
    items:[{id:'q',text:'С попугаем то что?😂'},{id:'dependent',text:'Тогда это ответ и на мой вопрос?'}]};
  const prepared=prepareAssistantRequest(req);
  const row=(itemId,action,text,tags,dependsOnItemIds=[])=>({itemId,action,text,tags,dependsOnItemIds,
    reason:'Необходимое наблюдение видео отсутствует; без него содержательный ответ невозможен.',basis:'context',evidenceIndices:[],moderationRuleRefs:[],
    editorial:{decision:'accept',reason:'Исходная ошибочная оценка',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}}});
  const value=expandCompactOutput({text:'Оценка',evidence:[],decisions:[
    row('q','reply_and_close',blockedReply().proposals[0].text,['question','missing_context']),
    row('dependent','close','',['question'],['q'])]},prepared.ids);
  const result=admitSinglePassResult(value,prepared,{calls:0,openedUrls:[],completedActivity:[],webCallLimit:null});
  assert.deepEqual(result.admitted.proposals,[]);
  assert.deepEqual(result.editorialEvidence.entries,[]);
  assert.deepEqual(new Set(result.isolatedItemIds),new Set(['q','dependent']));
});

test('preservation retains a max-length exact blocker instead of truncating its identifying suffix',()=>{
  const suffix=' Не хватает реплики родителя parent-comment-314159.';
  const reason='x'.repeat(2000-suffix.length)+suffix;
  const value={text:'Оценка',sources:[],proposals:[{itemId:'q',kind:'close',text:''}],
    assessments:[{itemId:'q',outcome:'close',reason,tags:['question','missing_context']}]};
  const result=validateAssistantResult(value,new Set(['q']),true);
  assert.equal(result.assessments[0].outcome,'needs_attention');
  assert.ok(result.assessments[0].reason.endsWith(suffix));
  assert.ok(result.assessments[0].reason.length<=2000);
});

test('supported narrowing with a needs_fact topic tag alone stays eligible',()=>{
  const value=blockedReply();value.assessments[0].tags=['question','needs_fact'];
  value.assessments[0].reason='На основную часть вопроса есть точный привязанный ответ; неизвестный год выпуска не нужен для этого ответа.';
  value.proposals[0].text='В приложенной расшифровке говорится о подогреве сидений.';
  assert.deepEqual(validateAssistantResult(value,new Set(['q']),true),value);
});

test('normalization remains idempotent and cannot rescue malformed recipients',()=>{
  const value=blockedReply();value.assessments[0].outcome='close';value.proposals[0]={itemId:'q',kind:'close',text:''};
  const held=validateAssistantResult(value,new Set(['q']),true);
  assert.deepEqual(preserveUnresolvedSubstantiveQuestions(held),held);
  const foreign=structuredClone(value);foreign.proposals[0].itemId='other-company-recipient';
  assert.throws(()=>validateAssistantResult(foreign,new Set(['q']),true),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('an unresolved direct post pointer retains already-validated review context without rewriting binding',()=>{
  const payload={purpose:'triage_review',items:[{id:'q',postId:'missing-post'}],
    posts:[{id:'other',postKey:'post:other',text:'Existing admitted post'}],
    branches:[{id:'other-branch',postId:'other',messages:[{id:'context',text:'Existing admitted context'}]}],
    materials:[{id:'speech',kind:'transcript',postKey:'post:other',text:'Existing admitted transcript'}]};
  const before=structuredClone(payload),projected=projectValidatedModelContext(payload);
  assert.deepEqual(projected.posts,payload.posts);assert.deepEqual(projected.branches,payload.branches);
  assert.deepEqual(projected.materials,payload.materials);assert.deepEqual(projected.items,payload.items);
  assert.deepEqual(payload,before);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {preserveUnresolvedSubstantiveQuestions} from './assistant-question-preservation.mjs';
import {validateAssistantResult,prepareAssistantRequest,expandCompactOutput,admitSinglePassResult,
  retainGenerationEditorial,assistantInstructions,reviewInstructions,editorialInstructions,singlePassInstructions,
  generationMetadata} from './assistant.mjs';
import {REPLY_QUALITY_GUIDANCE} from './assistant-reply-quality-guidance.mjs';

const closed=(tags=[])=>({text:'Оценка',sources:[],proposals:[{itemId:'q',kind:'close',text:''}],
  assessments:[{itemId:'q',outcome:'close',reason:'Точный референт в доступном фрагменте не установлен.',tags}]});
test('final unresolved question and complaint declarations preserve the exact recipient and blocker',()=>{
  for(const tags of [['question','missing_context'],['question','needs_fact'],['complaint','missing_context']]){
    const original=closed(tags),snapshot=structuredClone(original);
    const result=validateAssistantResult(original,new Set(['q']),true);
    assert.deepEqual(result.proposals,[]);
    assert.equal(result.assessments[0].outcome,'needs_attention');
    assert.equal(result.assessments[0].itemId,'q');
    assert.deepEqual(result.assessments[0].tags,tags);
    assert.ok(result.assessments[0].reason.includes(original.assessments[0].reason));
    assert.deepEqual(original,snapshot,'guard does not rewrite the original paid result');
  }
});
test('answered questions, terminal thanks and feedback remain eligible for close',()=>{
  for(const tags of [[],['question'],['feedback'],['missing_context'],['needs_fact']]){
    const original=closed(tags);
    assert.deepEqual(preserveUnresolvedSubstantiveQuestions(original),original);
    assert.deepEqual(validateAssistantResult(original,new Set(['q']),true),original);
  }
});
test('partial context and historic item tags do not override an independently supported answer',()=>{
  const candidate=closed(['question','needs_fact']);
  candidate.proposals[0]={itemId:'q',kind:'reply_and_close',text:'Содержательный ответ из доступной привязанной информации.'};
  candidate.assessments[0].outcome='reply';
  assert.equal(validateAssistantResult(candidate,new Set(['q']),true).proposals.length,1);
  const req={purpose:'triage_review',items:[{id:'q',text:'Вопрос',tags:['question','missing_context']}],firstPass:closed(['question'])};
  assert.equal(prepareAssistantRequest(req).payload.firstPass.proposals[0].kind,'close');
});
test('first-pass review input carries a held question rather than falsely settled closure',()=>{
  const prepared=prepareAssistantRequest({purpose:'triage_review',items:[{id:'q',text:'С попугаем то что?😂'}],firstPass:closed(['question','missing_context'])});
  const model=JSON.parse(prepared.input);
  assert.deepEqual(model.firstPass.proposals,[]);
  assert.equal(model.firstPass.assessments[0].outcome,'needs_attention');
  assert.match(model.firstPass.assessments[0].reason,/Точный референт/);
});
const row=(itemId,tags=[],dependsOnItemIds=[])=>({itemId,action:'close',text:'',reason:'Контекст необходим для ответа.',tags,
  editorial:{decision:'accept',reason:'Модель ошибочно считает закрытие подходящим.',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}},
  basis:'context',evidenceIndices:[],dependsOnItemIds,moderationRuleRefs:[]});
test('compact admission removes contradictory close proof before dependent decision propagation',()=>{
  const req={purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1',
    items:[{id:'q',text:'Какой объект имелся в виду?'},{id:'independent',text:'Спасибо, ответ понятен.'},{id:'dependent',text:'И мой вопрос зависит от первого.'}]};
  const prepared=prepareAssistantRequest(req);
  const value=expandCompactOutput({text:'Проверка',evidence:[],decisions:[
    row('q',['question','missing_context']),row('independent'),row('dependent',['question'],['q'])
  ]},prepared.ids);
  const result=admitSinglePassResult(value,prepared,{calls:0,openedUrls:[],completedActivity:[],webCallLimit:null});
  assert.deepEqual(result.admitted.proposals.map(p=>p.itemId),['independent']);
  assert.deepEqual(result.editorialEvidence.entries.map(e=>e.itemId),['independent']);
  assert.deepEqual(new Set(result.isolatedItemIds),new Set(['q','dependent']));
  assert.equal(result.admitted.assessments.find(a=>a.itemId==='q').outcome,'needs_attention');
  assert.equal(result.admitted.assessments.find(a=>a.itemId==='dependent').outcome,'needs_attention');
});
test('old exact closure proof never survives a preservation hold',()=>{
  const original=closed(['question','needs_fact']);
  const held=validateAssistantResult(original,new Set(['q']),true);
  const evidence={version:1,contract:'communityhero-editorial-v1',entries:[{itemId:'q',kind:'close',textSha256:createHash('sha256').update('').digest('hex')}]};
  assert.deepEqual(retainGenerationEditorial(evidence,held).entries,[]);
});
test('all preparation, review, editorial and discussion seams consume the same quality contract',()=>{
  for(const company of ['likeavto','baw-russia'])for(const instructions of [
    assistantInstructions(true,company),assistantInstructions(false,company,true),reviewInstructions(company),
    editorialInstructions(company),singlePassInstructions(company,true,false,true)
  ])assert.ok(instructions.includes(REPLY_QUALITY_GUIDANCE));
  const instructions=assistantInstructions(true);
  assert.equal(generationMetadata('fixture',true).instructionSha256,createHash('sha256').update(instructions).digest('hex'));
});

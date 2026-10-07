import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,reviewInstructions,publicResearchInstructions,prepareAssistantRequest,
  admitReviewWithRepair,admitReviewEvidence,outputSchema,generationMetadata} from './assistant.mjs';
import {researchInstructions} from './assistant-research.mjs';
import {evidenceQualityFields,evidenceQualityHolds} from './assistant-evidence-quality.mjs';

const scope={model:'Q06',trim:'two-motor trim',market:'CN',modelYear:'2025'};
const source=(fields={})=>({itemId:'a',url:'https://maker.example/configuration',title:'Official table',claim:'This selected trim has two motors',...fields});
const prepared=(account='likeavto')=>prepareAssistantRequest({account,purpose:'triage_review',items:[{id:'a'},{id:'b'}],
  firstPass:{text:'Review',sources:[],proposals:[{itemId:'b',kind:'reply_and_close',text:'Спасибо за приглашение!'}],
    assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need trim specification',tags:['needs_fact']},
      {itemId:'b',outcome:'reply',reason:'Friendly invitation',tags:['feedback']}]}});
const candidate=(evidence)=>({text:'Reviewed',sources:[],evidence,
  proposals:[{itemId:'a',kind:'reply_and_close',text:'У этой комплектации два мотора.'},
    {itemId:'b',kind:'reply_and_close',text:'Спасибо за приглашение!'}],
  assessments:[{itemId:'a',outcome:'reply',reason:'Exact selected trim',tags:['question']},
    {itemId:'b',outcome:'reply',reason:'Friendly invitation',tags:['feedback']}]});
const trace={calls:1,openedUrls:['https://maker.example/configuration']};
const options={runAttempt:()=>{throw new Error('No model/browser call allowed in this fixture');}};

test('shared prompt policy distinguishes personal stories, wishes, sarcasm and invitations from objections',()=>{
  for(const account of ['likeavto','baw-russia'])for(const prompt of [assistantInstructions(true,account),reviewInstructions(account),researchInstructions(account),publicResearchInstructions()]){
    assert.match(prompt,/personal story is not automatically\s+an objection/);
    assert.match(prompt,/price wish, V8 wish, joke, price sarcasm or invitation to Voronezh/);
    assert.match(prompt,/concrete factual question/);
    assert.match(prompt,/Do not hold supported friendly engagement/);
    assert.match(prompt,/complete transcript cannot verify\s+a whole-range specification/);
    assert.match(prompt,/already supported narrow claim/);
    assert.match(prompt,/different years or markets/);
    assert.match(prompt,/unlabeled symbol/);
  }
  // Version change invalidates old prompt/profile bindings without editing company rules.
  assert.equal(generationMetadata('',true).promptVersion,'communityhero-drafting-v19-intent-scoped-evidence');
});

test('structured empty/missing-table/access-challenge sources hold only their recipient with company-scoped evidence',async()=>{
  for(const account of ['likeavto','baw-russia'])for(const status of ['empty','missing_table','access_challenge','rendered_unavailable']){
    const evidence=source({claimKind:'product_specification',scope,extraction:{status,observedAt:'2026-09-27T12:00:00Z',
      rowLabels:['Motor count'],columnLabels:['two-motor trim'],values:[]}});
    const before=candidate([evidence]);const unchanged=structuredClone(before);
    const result=await admitReviewWithRepair(before,prepared(account),trace,options);
    assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
    assert.deepEqual(result.admitted.assessments[1],before.assessments[1]);
    assert.deepEqual(result.admitted.proposals,[before.proposals[1]]);
    assert.deepEqual(result.evidence,[]);assert.deepEqual(before,unchanged);
    assert.equal(result.evidenceHolds[0].accountKey,account);
    assert.equal(result.evidenceHolds[0].url,evidence.url);
    assert.deepEqual(result.evidenceHolds[0].scope,scope);
    assert.deepEqual(result.evidenceHolds[0].extraction,evidence.extraction);
    assert.equal(result.evidenceHolds[0].renderedFallback.attempts,0);
    assert.equal(result.evidenceHolds[0].renderedFallback.status,status==='access_challenge'?'access_challenge':'unsupported');
    assert.throws(()=>admitReviewEvidence(before,prepared(account),trace),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'EVIDENCE_QUALITY'});
  }
});

test('an opened page cannot substantiate a declared specification with incomplete scope',async()=>{
  for(const missing of ['model','trim','market','modelYear']){
    const incomplete={...scope};delete incomplete[missing];
    const result=await admitReviewWithRepair(candidate([source({claimKind:'product_specification',scope:incomplete})]),prepared(),trace,options);
    assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
    assert.equal(result.evidenceHolds[0].reason,'specification_scope_incomplete');
  }
});

test('scoped narrow fact and correctly attributed speaker statement pass without mandatory extra research',async()=>{
  for(const fields of [{claimKind:'product_specification',scope},
    {claimKind:'product_specification',scope:{model:'Q06',trim:'two-motor trim',market:'CN',observedAt:'2026-09-27'}},
    {claimKind:'source_statement',claim:'The speaker said this shown version has two motors'}]){
    const before=candidate([source(fields)]);
    const result=await admitReviewWithRepair(before,prepared(),trace,options);
    assert.deepEqual(result.admitted.proposals,before.proposals);
    assert.equal(result.evidence[0].trust,'source_only');
    assert.equal(result.evidenceHolds,undefined);
  }
  const before=candidate([]);
  const result=await admitReviewWithRepair(before,prepared(),{calls:0,openedUrls:[]},options);
  assert.deepEqual(result.admitted.proposals,before.proposals);
});

test('wrong observed trim column, market or model year cannot silently support the requested specification',async()=>{
  for(const mismatch of [{trim:'one-motor trim'},{market:'RU'},{modelYear:'2026'},{model:'another model'}]){
    const evidence=source({claimKind:'product_specification',scope,sourceScope:{...scope,...mismatch},
      extraction:{status:'complete',rowLabels:['Motor count'],columnLabels:[mismatch.trim??scope.trim],values:['2']}});
    const result=await admitReviewWithRepair(candidate([evidence]),prepared(),trace,options);
    assert.equal(result.admitted.assessments[0].outcome,'needs_attention');
    assert.equal(result.evidenceHolds[0].reason,'specification_scope_mismatch');
    assert.deepEqual(result.evidenceHolds[0].sourceScope,{...scope,...mismatch});
    assert.equal(result.admitted.assessments[1].outcome,'reply');
  }
});

test('schema and admission allow only bounded extraction declarations, never fabricated rendered-browser capability',()=>{
  const schema=outputSchema(new Set(['a']),true,true).properties.evidence.items.properties;
  assert.ok(schema.scope);assert.ok(schema.extraction);
  for(const fields of [{claimKind:'verified_product'}, {scope:{...scope,account:'baw-russia'}},
    {extraction:{status:'rendered_present'}},{extraction:{status:'complete',browserVerified:true}},
    {extraction:{status:'empty',values:['']}},{extraction:{status:'empty',rowLabels:Array(21).fill('row')}}]){
    assert.throws(()=>evidenceQualityFields(source(fields)),{code:'ASSISTANT_INVALID_RESEARCH'});
  }
  assert.deepEqual(evidenceQualityHolds([source({claimKind:'source_statement'})],'likeavto'),[]);
});

test('quality hold does not hide malformed candidates or foreign evidence recipients',async()=>{
  const broken=candidate([source({extraction:{status:'empty'}})]);broken.proposals.push({itemId:'foreign',kind:'close',text:''});
  await assert.rejects(()=>admitReviewWithRepair(broken,prepared(),trace,options),{code:'ASSISTANT_INVALID_RESPONSE'});
  await assert.rejects(()=>admitReviewWithRepair(candidate([source({itemId:'foreign',extraction:{status:'empty'}})]),prepared(),trace,options),
    {code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'RECIPIENT'});
});

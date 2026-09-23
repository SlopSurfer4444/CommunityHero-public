import test from 'node:test';
import assert from 'node:assert/strict';
import {caseHash,outputHash} from './evaluate.mjs';
import {privateEvidenceDigest,validateModelReview} from './model-review.mjs';
function fixture(runtimeStatus='completed',candidateAction='reply'){
  const raw={caseId:'c',jobOrdinal:1,job:{prepareBundle:{request:{items:[{id:'i',text:'Private frozen evidence'}]}}},currentItem:{text:'Later text'}};
  raw.evidenceDigest=privateEvidenceDigest(raw);
  const c={id:'c',itemId:'i',postKey:'p',privateEvidence:{evidenceDigest:raw.evidenceDigest}};
  const output={caseId:'c',caseHash:caseHash(c),itemId:'i',runtimeStatus,candidateAction,action:runtimeStatus==='completed'?candidateAction:null,executed:false};
  const a={caseId:'c',caseHash:caseHash(c),outputHash:outputHash(output),evidenceDigest:raw.evidenceDigest,runtimeStatus,candidateAction,humanApproved:false,acceptedForPublication:false,
    scope:runtimeStatus==='completed'?'completed_output':candidateAction?'unadmitted_candidate':'no_candidate',
    dimensions:Object.fromEntries(['action','tone','facts','context','runtime'].map(d=>[d,{verdict:!candidateAction&&['action','tone','facts'].includes(d)?'not_evaluable':'supported',reason:'Frozen evidence assessment'}])),
    evidenceRefs:['/job/prepareBundle/request/items/0/text'],nextStep:'Review by operator'};
  return [{cases:[c]},{outputs:[output]},{cases:[raw]},{schemaVersion:1,artifactType:'model_quality_review',reviewer:{type:'model',id:'test-model'},humanAcceptance:false,qualityProven:false,assessments:[a]}];
}
test('model judgments remain separate from human acceptance for all runtime cohorts',()=>{
  for(const status of ['completed','source_changed','error']){
    const result=validateModelReview(...fixture(status));assert.equal(result[status],1);assert.equal(result.humanApproved,0);assert.equal(result.qualityProven,false);assert.equal(result.publicationAuthorized,false);
  }
});
test('changed output, case, private evidence and recipient invalidate stale review',()=>{
  for(const mutate of [f=>f[1].outputs[0].text='changed',f=>f[0].cases[0].postKey='other',f=>f[2].cases[0].job.prepareBundle.request.items[0].text='changed',f=>{f[1].outputs[0].itemId='other';f[3].assessments[0].outputHash=outputHash(f[1].outputs[0]);}]){
    const f=fixture();mutate(f);assert.throws(()=>validateModelReview(...f));
  }
});
test('reject human promotion, absent dimensions, duplicate cohort and live-context substitution',()=>{
  for(const mutate of [f=>f[3].humanAcceptance=true,f=>f[3].qualityProven=true,f=>f[3].reviewer.type='human',f=>f[3].labels=[],f=>f[3].assessments[0].humanApproved=true,f=>delete f[3].assessments[0].dimensions.facts,f=>f[3].assessments.push(f[3].assessments[0]),f=>f[3].assessments[0].evidenceRefs=['/currentItem/text'],f=>f[3].assessments[0].evidenceRefs=['/job/missing']]){
    const f=fixture();mutate(f);assert.throws(()=>validateModelReview(...f));
  }
});
test('unadmitted and missing candidates cannot become completed-action judgments',()=>{
  let f=fixture('error');f[3].assessments[0].scope='completed_output';assert.throws(()=>validateModelReview(...f));
  f=fixture('error');f[1].outputs[0].action='reply';f[3].assessments[0].outputHash=outputHash(f[1].outputs[0]);assert.throws(()=>validateModelReview(...f));
  f=fixture('source_changed',null);assert.equal(validateModelReview(...f).source_changed,1);
  f[3].assessments[0].dimensions.facts.verdict='supported';assert.throws(()=>validateModelReview(...f));
});

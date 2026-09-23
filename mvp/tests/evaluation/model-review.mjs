// Offline analyst evidence only. This format deliberately cannot act as human labels.
import {createHash} from 'node:crypto';
import {caseHash,outputHash,validateCases} from './evaluate.mjs';
const hash=value=>createHash('sha256').update(JSON.stringify(value)).digest('hex');
const requireValue=(ok,message)=>{if(!ok)throw new Error(message);};
const dimensions=['action','tone','facts','context','runtime'];
const verdicts=['supported','concern','insufficient_evidence','not_applicable','not_evaluable'];
export function privateEvidenceDigest(raw){const {caseId,evidenceDigest,...payload}=raw;return hash(payload);}
export function resolveEvidence(raw,pointer){
  requireValue(typeof pointer==='string'&&pointer.startsWith('/job/'),'Evidence must reference frozen job, not current state');
  return pointer.slice(1).split('/').map(x=>x.replace(/~1/g,'/').replace(/~0/g,'~')).reduce((v,key)=>v?.[key],raw);
}
function uniqueRows(rows,key,label){
  requireValue(Array.isArray(rows),`${label} array required`);
  const result=new Map();for(const row of rows){requireValue(row&&typeof row[key]==='string'&&!result.has(row[key]),`${label} duplicate or missing identity`);result.set(row[key],row);}return result;
}
export function validateModelReview(casesData,outputsData,privateData,review){
  const cases=validateCases(casesData),outputs=uniqueRows(outputsData?.outputs,'caseId','outputs'),raws=uniqueRows(privateData?.cases,'caseId','private evidence');
  requireValue(review?.schemaVersion===1&&review.artifactType==='model_quality_review','Model review format required');
  requireValue(review.reviewer?.type==='model'&&typeof review.reviewer.id==='string'&&review.reviewer.id,'Model reviewer identity required');
  requireValue(review.humanAcceptance===false&&review.qualityProven===false,'Model review cannot assert human acceptance or proven quality');
  requireValue(review.labels===undefined&&review.outputReviews===undefined,'Human label fields are forbidden in model review');
  const assessments=uniqueRows(review.assessments,'caseId','assessments');
  requireValue(assessments.size===cases.length&&outputs.size===cases.length&&raws.size===cases.length,'Exact cohort coverage required');
  const counts={reviewed:0,completed:0,source_changed:0,error:0,candidateConcerns:0};
  for(const c of cases){
    const a=assessments.get(c.id),o=outputs.get(c.id),raw=raws.get(c.id);
    requireValue(a&&o&&raw,'Missing case evidence');
    requireValue(a.caseHash===caseHash(c)&&o.caseHash===caseHash(c),'Case revision mismatch');
    requireValue(a.outputHash===outputHash(o),'Output revision mismatch');
    requireValue(a.evidenceDigest===raw.evidenceDigest&&a.evidenceDigest===c.privateEvidence?.evidenceDigest&&privateEvidenceDigest(raw)===raw.evidenceDigest,'Private evidence digest mismatch');
    requireValue(o.itemId===c.itemId&&raw.job.prepareBundle.request.items.some(i=>i.id===c.itemId),'Recipient mismatch');
    requireValue(a.humanApproved===false&&a.acceptedForPublication===false,'Model assessment is not publication or human approval');
    requireValue(a.runtimeStatus===o.runtimeStatus&&['completed','source_changed','error'].includes(a.runtimeStatus),'Runtime mismatch');
    const expectedScope=o.runtimeStatus==='completed'?'completed_output':o.candidateAction?'unadmitted_candidate':'no_candidate';
    requireValue(a.scope===expectedScope,'Candidate/admission scope mismatch');
    if(o.runtimeStatus!=='completed')requireValue(o.action===null&&o.executed===false,'Non-admitted output must not claim action or execution');
    requireValue(a.candidateAction===(o.candidateAction??null),'Candidate action mismatch');
    requireValue(dimensions.every(d=>verdicts.includes(a.dimensions?.[d]?.verdict)&&typeof a.dimensions[d].reason==='string'&&a.dimensions[d].reason.trim()),'Five reasoned dimensions required');
    if(expectedScope==='no_candidate')requireValue(['action','tone','facts'].every(d=>a.dimensions[d].verdict==='not_evaluable'),'Missing candidate cannot receive output-quality verdict');
    requireValue(Array.isArray(a.evidenceRefs)&&a.evidenceRefs.length>0,'Evidence references required');
    for(const ref of a.evidenceRefs)requireValue(resolveEvidence(raw,ref)!==undefined,`Missing frozen evidence: ${ref}`);
    requireValue(typeof a.nextStep==='string'&&a.nextStep.trim(),'Concrete next step required');
    counts.reviewed++;counts[o.runtimeStatus]++;
    if(['action','tone','facts','context'].some(d=>a.dimensions[d].verdict==='concern'))counts.candidateConcerns++;
  }
  return {...counts,reviewerType:'model',humanApproved:0,qualityProven:false,publicationAuthorized:false};
}

// Opt-in declarations from preparation, never inferred from prose or tags.
export const FACT_DEPENDENCY_CONTRACT = 'targeted_public_v1';
export const FACT_DEPENDENCY_INSTRUCTIONS = `For a held item whose indispensable fact remains missing after this pass, declare
factDependency with kind, claimScope and publicQuery. kind is missing_public_fact,
private_company_fact, missing_media or owner_decision. A missing_public_fact must
identify the actual indispensable public claim in claimScope and a focused public
subject/technical query in publicQuery. Do not copy a comment, customer identity,
private message, contract, contact details or company-case records into the query.
There is no model/trim/attribute whitelist. Search scope follows the actual claim.
Private stock, current company quote/order state, commitments and private decisions
require private_company_fact or owner_decision, never public research. Missing video
contents require missing_media. For these three kinds publicQuery must be null.
This declaration requests source-only evidence and a new preparation; it never
approves a candidate. No indispensable unresolved dependency means null. Only hold
decisions may declare a dependency. Unrelated supported decisions must continue.`;

export function factDependencySchema() {
  return {anyOf:[{type:'null'},{type:'object',additionalProperties:false,
    required:['kind','claimScope','publicQuery'],properties:{
      kind:{type:'string',enum:['missing_public_fact','private_company_fact','missing_media','owner_decision']},
      claimScope:{type:'string',minLength:2,maxLength:2000},
      publicQuery:{anyOf:[{type:'null'},{type:'string',minLength:2,maxLength:1000}]}}}]};
}

export function admitFactDependencies(value,ids,enabled) {
  const fail=message=>{throw Object.assign(new Error(message),{code:'ASSISTANT_INVALID_RESPONSE',responseCategory:'FACT_DEPENDENCY'});};
  if(!enabled){if(value.factDependencies!==undefined)fail('Fact dependencies require captured opt-in');return undefined;}
  const entries=value.factDependencies;
  if(!Array.isArray(entries)||entries.length>ids.size)fail('Invalid fact dependencies');
  const seen=new Set();
  return entries.map(entry=>{
    if(!entry||Object.keys(entry).length!==4||!['itemId','kind','claimScope','publicQuery'].every(key=>Object.hasOwn(entry,key))
      ||!ids.has(entry.itemId)||seen.has(entry.itemId)
      ||!['missing_public_fact','private_company_fact','missing_media','owner_decision'].includes(entry.kind)
      ||typeof entry.claimScope!=='string'||entry.claimScope.trim().length<2||entry.claimScope.length>2000||/[\u0000-\u001f\u007f]/u.test(entry.claimScope)
      ||!value.assessments.some(row=>row.itemId===entry.itemId&&row.outcome==='needs_attention')
      ||value.proposals.some(row=>row.itemId===entry.itemId))fail('Dependency must bind one held recipient');
    if(entry.kind==='missing_public_fact'){
      if(typeof entry.publicQuery!=='string'||entry.publicQuery.trim().length<2||entry.publicQuery.length>1000||/[\u0000-\u001f\u007f]/u.test(entry.publicQuery))fail('Public dependency requires a bounded public query');
    }else if(entry.publicQuery!==null)fail('Private/media/owner dependency cannot dispatch public research');
    seen.add(entry.itemId);
    return {...entry,claimScope:entry.claimScope.trim(),publicQuery:entry.publicQuery?.trim()??null};
  });
}

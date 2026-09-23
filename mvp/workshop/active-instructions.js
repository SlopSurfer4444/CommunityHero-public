// Read-only projection of rule/policy admission in server/src/knowledge.rs::select.
// Never infer active guidance from raw materials, old heads or browser drafts.
import {importedRuleSemantics} from './assistant-rule-semantics.mjs';

function displaySemantics(entry,version,account) {
  // The current workshop account is independently bound by the caller's account
  // filter. Unknown accounts and unrecognized provenance keep their raw title.
  if(account!=='LikeAvto')return undefined;
  const accountDefinition={accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto'};
  const manifest=[{entryId:entry.id,versionId:version.id,hash:version.hash,kind:version.kind,
    trust:version.trust,scope:version.scope}];
  try{return importedRuleSemantics({...version,knowledgeEntryId:entry.id,knowledgeVersionId:version.id},
    {account:accountDefinition,manifest});}
  catch{return undefined;}
}

export function activeInstructions(catalog, {account,postKeys=[],posts=[],connectorBinding,now=new Date()}={}) {
  if(!Array.isArray(catalog?.entries)||!Array.isArray(catalog?.versions)||!account)return null;
  const at=new Date(now).getTime(),keys=new Set(postKeys.filter(Boolean));
  if(!Number.isFinite(at))return null;
  const global=[],post=[];
  for(const entry of catalog.entries){
    const version=catalog.versions.find(row=>row.id===entry.currentVersionId&&row.entryId===entry.id);
    if(!version)return null;
    if(version.scope?.account!==account||!Array.isArray(version.scope?.postKeys))continue;
    let scope=version.scope.postKeys;
    const aliases=version.companyImport?.scope?.postAliases;
    if(Array.isArray(aliases)&&aliases.length){
      const company=version.companyImport.companyKey;
      const expected={likeavto:'LikeAvto','baw-russia':'BAW Russia'}[company];
      if(expected!==account||connectorBinding?.connector!=='angryspace'||connectorBinding.accountId!==account||connectorBinding.providerAccountId!==company)continue;
      scope=aliases.filter(a=>a.namespace==='commentops-fast.post-key'&&typeof a.value==='string'&&posts.some(p=>p.postKey===a.value&&[
        p.account,p.accountId,p.scope?.account,p.sourceMediaScope?.account,p.connectorBinding?.accountId
      ].filter(v=>v!==undefined).every(v=>v===account))).map(a=>a.value);
      if(!scope.length)continue;
    }
    if(scope.length&&!scope.some(key=>keys.has(key)))continue;
    if(!['rule','policy'].includes(version.kind)||version.status!=='active'||!['verified','imported_policy'].includes(version.trust))continue;
    const from=Date.parse(version.validFrom),until=version.validUntil==null?Infinity:Date.parse(version.validUntil);
    if(!Number.isFinite(from)||Number.isNaN(until)||from>at||until<=at)continue;
    const semantics=displaySemantics(entry,version,account);
    (scope.length?post:global).push({...version,...(semantics?{displaySemantics:semantics}:{})});
  }
  for(const rows of [global,post])rows.sort((a,b)=>String(a.sourceMaterialId||a.id).localeCompare(String(b.sourceMaterialId||b.id)));
  return {global,post};
}

// Presentation only. The admitted catalog and preparation order remain untouched.
const titleOrder=new Intl.Collator('ru',{numeric:true,sensitivity:'base'});
export function displayInstructions(rows) {
  const label=row=>String(row.displaySemantics?.label||row.title||'');
  return [...rows].sort((a,b)=>titleOrder.compare(label(a),label(b))||String(a.id).localeCompare(String(b.id)));
}

export function chronologicalHistory(rows,order='newest') {
  return [...rows].sort((a,b)=>{
    const left=Date.parse(a.createdAt),right=Date.parse(b.createdAt);
    if(!Number.isFinite(left))return Number.isFinite(right)?1:String(a.id).localeCompare(String(b.id));
    if(!Number.isFinite(right))return -1;
    return (order==='oldest'?left-right:right-left)||String(a.id).localeCompare(String(b.id));
  });
}

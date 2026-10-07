// These are evidence declarations, never independent verification. In particular,
// this adapter's isolated CLI offers web.run only, not a rendered browser path.
export const EVIDENCE_QUALITY_INSTRUCTIONS = `Communicative intent comes before reply wording. A personal story is not automatically
an objection. A price wish, V8 wish, joke, price sarcasm or invitation to Voronezh is
not automatically a technical question or price objection. Do not add a disclaimer,
lecture, invented emotion or forced empathy merely because such a topic is mentioned.
Retain grounded warmth, wit and company voice when useful. A concrete factual question,
actual misconception, essential limitation or complaint may need correction or escalation.
Review the draft's usefulness for the exact intent: accept an already useful draft,
revise an inappropriate one or hold an indispensable unsupported claim, with a concrete
reason in the assessment. Do not hold supported friendly engagement for missing facts
it does not need. Changed proposals require the normal new review and approval binding.
When asked to find a material, publication, source or link, first inspect the supplied
company database/materials and any available authorized account-scoped lookup. If no
verified match is there, continue through authorized company channels and the company
site, then external search when available. A missing database entry alone is not a
reason to stop. Use only tools actually available and permitted in this run; request
an available application research step when direct search is unavailable. This rule
does not grant new tool, account or publication permissions. Open candidate sources
with the available read tools and verify their content matches the requested material
before citing an exact observed URL. Never invent URLs, reconstruct a permalink from
memory, or claim an unavailable/unperformed search succeeded. Do not send private
company/customer data to public search or expose private source URLs to an audience
without access. If no verified match is found or a required tool/access is unavailable,
state the specific limitation and distinguish searched sources from unsearched ones.
source_only identifies where a statement came from, not whether it is true for a product.
Separate "the speaker said" from "the product has". A complete transcript cannot verify
a whole-range specification. Preserve accurate attribution without broadening a shown
version to all versions. An already supported narrow claim or an attributed statement
does not require extra web research merely because its source is a transcript.
For product specifications or concrete conflicts bind model, trim/version, market and
model year or observation date. Same-scope official contradictions require correction;
different years or markets may explain a difference and must not silently override a
source. Read table row/column labels, symbols and footnotes; an unlabeled symbol does
not establish equipment. Narrow a claim to its support, or hold the unresolved claim.
An empty text extraction or missing required table is missing extraction evidence, never
proof that the official page has no answer. This isolated runtime has no supported
rendered-browser tool. Do not invent a browser observation or repeatedly retry that
page. Keep the specific claim held and explain the rendered-path limitation; independent
supported decisions can continue. A later operator-supplied rendered observation needs
its exact URL, selected model/trim/market/year or date, row/column labels, observed values
and observation time, company binding and normal review before it can support a reply.`;

const object=value=>value&&typeof value==='object'&&!Array.isArray(value);
const fail=()=>Object.assign(new Error('Invalid structured evidence quality declaration'),
  {code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'EVIDENCE_QUALITY'});
const text=value=>typeof value==='string'&&value.trim()&&value.length<=500;
const scopeKeys=['model','trim','market','modelYear','observedAt'];

export const evidenceQualityProperties = {
  claimKind:{type:'string',enum:['source_statement','product_specification']},
  scope:{type:'object',additionalProperties:false,properties:Object.fromEntries(scopeKeys.map(key=>[key,{type:'string',maxLength:500}]))},
  sourceScope:{type:'object',additionalProperties:false,properties:Object.fromEntries(scopeKeys.map(key=>[key,{type:'string',maxLength:500}]))},
  extraction:{type:'object',additionalProperties:false,required:['status'],properties:{
    status:{type:'string',enum:['complete','empty','missing_table','access_challenge','rendered_unavailable']},
    observedAt:{type:'string',maxLength:500},
    rowLabels:{type:'array',maxItems:20,items:{type:'string',maxLength:500}},
    columnLabels:{type:'array',maxItems:20,items:{type:'string',maxLength:500}},
    values:{type:'array',maxItems:20,items:{type:'string',maxLength:500}}
  }}
};

export function evidenceQualityFields(source) {
  const result={};
  if(source.claimKind!==undefined){
    if(!evidenceQualityProperties.claimKind.enum.includes(source.claimKind))throw fail();
    result.claimKind=source.claimKind;
  }
  for(const field of ['scope','sourceScope'])if(source[field]!==undefined){
    if(!object(source[field])||Object.entries(source[field]).some(([key,value])=>!scopeKeys.includes(key)||!text(value)))throw fail();
    result[field]={...source[field]};
  }
  if(source.extraction!==undefined){
    const extraction=source.extraction;
    if(!object(extraction)||!evidenceQualityProperties.extraction.properties.status.enum.includes(extraction.status)
      ||Object.keys(extraction).some(key=>!Object.hasOwn(evidenceQualityProperties.extraction.properties,key)))throw fail();
    if(extraction.observedAt!==undefined&&!text(extraction.observedAt))throw fail();
    for(const key of ['rowLabels','columnLabels','values'])if(extraction[key]!==undefined
      &&(!Array.isArray(extraction[key])||extraction[key].length>20||extraction[key].some(value=>!text(value))))throw fail();
    result.extraction=structuredClone(extraction);
  }
  return result;
}

export function evidenceQualityHolds(evidence,accountKey) {
  return evidence.flatMap(source=>{
    const scope=source.scope;
    let reason;
    if(source.extraction&&source.extraction.status!=='complete')reason='incomplete_extraction';
    else if(source.claimKind==='product_specification'
      &&(!scope?.model||!scope.trim||!scope.market||!scope.modelYear&&!scope.observedAt))reason='specification_scope_incomplete';
    else if(source.claimKind==='product_specification'&&source.sourceScope
      &&scopeKeys.some(key=>scope?.[key]&&source.sourceScope[key]&&scope[key]!==source.sourceScope[key]))reason='specification_scope_mismatch';
    if(!reason)return [];
    return [{version:1,accountKey,itemId:source.itemId,url:source.url,reason,
      ...(scope?{scope}:{}),...(source.sourceScope?{sourceScope:source.sourceScope}:{}),...(source.extraction?{extraction:source.extraction}:{}),
      ...(reason==='incomplete_extraction'?{renderedFallback:{status:source.extraction.status==='access_challenge'
        ?'access_challenge':'unsupported',attempts:0,capability:'web.run_text_only'}}:{})}];
  });
}

export function holdIncompleteEvidence(candidate,holds) {
  if(!holds.length)return candidate;
  const blocked=new Set(holds.map(hold=>hold.itemId));
  return {...candidate,evidence:candidate.evidence.filter(source=>!blocked.has(source.itemId)),
    proposals:candidate.proposals.filter(proposal=>!blocked.has(proposal.itemId)),
    assessments:candidate.assessments.map(assessment=>blocked.has(assessment.itemId)
      ?{itemId:assessment.itemId,outcome:'needs_attention',tags:['needs_fact'],reason:holds.some(hold=>hold.itemId===assessment.itemId&&hold.reason==='incomplete_extraction')
        ?'Нужная таблица или значения не извлечены; поддерживаемый rendered-browser путь отсутствует. Требуется привязанное к версии наблюдение и новая проверка.'
        :holds.some(hold=>hold.itemId===assessment.itemId&&hold.reason==='specification_scope_mismatch')
          ?'Область утверждения отличается от модели, комплектации, рынка, года или даты источника; перенос характеристики требует уточнения и новой проверки.'
        :'Характеристика не привязана к модели, комплектации, рынку и году или дате наблюдения; требуется уточнить либо сузить утверждение.'}:assessment)};
}

const ARRAY_FIELDS={
  forbidden_substrings:{effect:'deny',subject:'reply_text',operation:'literal_substring',
    historicalMatching:{caseNormalization:'python_str_lower',replyTrim:'none',regex:false}},
  allowed_reply_urls:{effect:'allow_only',subject:'reply_urls',operation:'exact_url',
    historicalMatching:{caseNormalization:'none',extractPattern:'https?://[^\\s<>]+',trimTrailingCharacters:'.,!?;:)]}',matchOrigin:false}},
  forbidden_reply_prefixes:{effect:'deny',subject:'reply_start',operation:'literal_prefix',
    historicalMatching:{caseNormalization:'python_str_casefold',replyTrim:'python_str_strip',regex:false}},
};
const EDITORIAL_SYMBOLS=new Set(['REPLY_EDITING','FACT_CHECKING']);
const LABELS={forbidden_substrings:'Запрещённые упоминания',allowed_reply_urls:'Разрешённые ссылки',
  forbidden_reply_prefixes:'Запрещённые начала ответов',REPLY_EDITING:'Редактура ответов — смешанное руководство',
  FACT_CHECKING:'Проверка фактов — смешанное руководство'};
const record=value=>!!value&&typeof value==='object'&&!Array.isArray(value);
const bounded=(value,max)=>typeof value==='string'&&value.length>0&&value.length<=max&&!/[\u0000-\u001f\u007f]/u.test(value);
const digest=value=>typeof value==='string'&&/^[a-f0-9]{64}$/i.test(value);
const canonical=value=>Array.isArray(value)?value.map(canonical):record(value)
  ?Object.fromEntries(Object.keys(value).sort().map(key=>[key,canonical(value[key])])):value;
const equal=(a,b)=>JSON.stringify(canonical(a))===JSON.stringify(canonical(b));
function invalid(){throw Object.assign(new Error('Invalid imported rule semantic provenance'),{code:'ASSISTANT_INVALID_REQUEST'});}

// This is a lossless description of recognized importer fields, not a new rule
// evaluator, policy approval, or instruction extracted from arbitrary text.
export function importedRuleSemantics(material,{account,manifest=[]}={}) {
  if(material?.kind!=='rule'||material.trust!=='imported_policy')return undefined;
  const imported=material.companyImport;
  if(!record(imported)||!record(imported.source)||!record(imported.source.originalIds))return undefined;
  const {source}=imported;const marker=source.originalIds;
  const field=source.origin==='commentops-fast.account-card'&&Object.hasOwn(ARRAY_FIELDS,marker.field)?marker.field:undefined;
  const symbol=source.origin==='commentops-fast.editorial-guidance'&&EDITORIAL_SYMBOLS.has(marker.symbol)?marker.symbol:undefined;
  if(!field&&!symbol)return undefined;

  if(!account||imported.companyKey!==account.accountKey||!record(imported.scope)||imported.scope.companyKey!==account.accountKey
    ||Object.keys(imported.scope).some(key=>key!=='companyKey')
    ||!bounded(material.knowledgeEntryId,512)||!bounded(material.knowledgeVersionId,512)
    ||!bounded(imported.importKey,512)||!digest(imported.recordSha256)||!digest(source.sha256)
    ||!record(imported.metadata)||imported.metadata.grantsExecutionAuthority!==false)invalid();
  const accountMatches=value=>value===account.accountKey||value===account.providerAccountId||value===account.displayName;
  if(material.account!==undefined&&!accountMatches(material.account))invalid();
  const candidates=Array.isArray(manifest)?manifest.filter(entry=>entry?.entryId===material.knowledgeEntryId&&entry.versionId===material.knowledgeVersionId):[];
  if(candidates.length!==1)invalid();
  const selected=candidates[0];
  if(selected.kind!==material.kind||selected.trust!==material.trust||!digest(selected.hash)
    ||!record(selected.scope)||!accountMatches(selected.scope.account)||!Array.isArray(selected.scope.postKeys)
    ||selected.scope.postKeys.length!==0||Object.keys(selected.scope).some(key=>!['account','postKeys'].includes(key)))invalid();
  if(material.scope!==undefined&&(!record(material.scope)||!accountMatches(material.scope.account)
    ||!equal(material.scope.postKeys,selected.scope.postKeys)||Object.keys(material.scope).some(key=>!['account','postKeys'].includes(key))))invalid();
  if(material.grantsExecutionAuthority!==undefined&&material.grantsExecutionAuthority!==false)invalid();
  const scope={companyKey:account.accountKey,account:account.displayName,postKeys:[]};
  const provenance={importKey:imported.importKey,recordSha256:imported.recordSha256,
    sourceSha256:source.sha256,origin:source.origin};
  const common={version:1,trust:'imported_policy',grantsExecutionAuthority:false,
    knowledgeEntryId:material.knowledgeEntryId,knowledgeVersionId:material.knowledgeVersionId,
    knowledgeVersionHash:selected.hash,scope};
  if(typeof material.text!=='string')invalid();

  if(field) {
    const expectedSource=field==='forbidden_reply_prefixes'?'configs/commentops-fast/common.json':`configs/commentops-fast/${account.accountKey}.json`;
    if(marker.source!==expectedSource||Object.keys(marker).some(key=>!['field','source'].includes(key))
      ||imported.metadata.category!=='brand_policy'
      ||material.category!==undefined&&material.category!=='brand_policy'
      ||!equal(imported.metadata.legacyProvenance,marker)
      ||!equal(imported.metadata.legacyScope,imported.scope))invalid();
    let values;try{values=JSON.parse(material.text);}catch{invalid();}
    if(!Array.isArray(values)||values.length>100||values.some(value=>!bounded(value,2048))
      ||!equal(values,imported.metadata.value))invalid();
    return {...common,label:LABELS[field],policyType:'reply_constraint',constraint:{...ARRAY_FIELDS[field],
      historicalMatching:{...ARRAY_FIELDS[field].historicalMatching},values:[...values]},
      provenance:{...provenance,field,document:marker.source}};
  }

  if(marker.path!=='commentops_fast/decision.py'||Object.keys(marker).some(key=>!['path','symbol'].includes(key))
    ||imported.metadata.category!=='existing_editorial_guidance'
    ||material.category!==undefined&&material.category!=='existing_editorial_guidance'
    ||imported.metadata.examplesAreVerifiedFacts!==false)invalid();
  return {...common,label:LABELS[symbol],policyType:'mixed_guidance',examplesAreVerifiedFacts:false,
    provenance:{...provenance,symbol,document:marker.path}};
}

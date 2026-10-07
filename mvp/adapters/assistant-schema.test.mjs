import test from 'node:test';
import assert from 'node:assert/strict';
import {outputSchema,editorialOutputSchema,reviewEvidenceQualityFields,prepareAssistantRequest,admitReviewEvidence} from './assistant.mjs';
import {evidenceQualityProperties,evidenceQualityFields,evidenceQualityHolds} from './assistant-evidence-quality.mjs';

// Validate the documented strict subset recursively, independently of the
// schema builder. This negative control detects the original sparse-required
// subtree even when it is hidden inside arrays and anyOf branches.
function strictContract(schema,path='$') {
  assert(schema&&typeof schema==='object'&&!Array.isArray(schema),path);
  if(schema.enum)assert(schema.enum.length>0,path+' empty enum');
  const types=Array.isArray(schema.type)?schema.type:[schema.type];
  if(types.includes('object')) {
    assert.equal(schema.additionalProperties,false,path+' additionalProperties');
    assert.deepEqual([...schema.required??[]].sort(),Object.keys(schema.properties??{}).sort(),path+' required');
    assert.equal(new Set(schema.required).size,schema.required.length,path+' duplicate required');
    for(const [key,value]of Object.entries(schema.properties))strictContract(value,path+'.'+key);
  }
  if(types.includes('array'))strictContract(schema.items,path+'[]');
  for(const [i,branch]of (schema.anyOf??[]).entries())strictContract(branch,path+'.anyOf['+i+']');
  for(const forbidden of ['allOf','not','dependentRequired','dependentSchemas','if','then','else'])assert.equal(schema[forbidden],undefined,path+' unsupported '+forbidden);
}

test('all output modes retain recursive strict required/additionalProperties and nonempty enums',()=>{
  for(const count of [0,1,20,100]) {
    const ids=new Set(Array.from({length:count},(_,i)=>'synthetic-'+i));
    for(const [triage,review,lookup]of [[true,false,false],[true,true,false],[false,false,false],[false,false,true]])strictContract(outputSchema(ids,triage,review,lookup));
    const tools={callsRemaining:4,definitions:[{name:'synthetic_tool',parameters:{type:'object',required:['name'],properties:{name:{type:'string'},optional:{type:'object',properties:{value:{type:'string'}}}}}}]};
    strictContract(outputSchema(ids,false,false,false,tools));
  }
  strictContract(editorialOutputSchema({payload:{editorialCandidates:[{proposalId:'proposal',proposalRevision:1,itemId:'synthetic',textSha256:'a'.repeat(64),contextDigest:'b'.repeat(64),rulesDigest:'c'.repeat(64)}]}}));
});

test('strict contract rejects historical evidence schema while optional wire fields remain nullable',()=>{
  const legacy={type:'object',additionalProperties:false,required:['itemId','url','title','claim'],properties:{itemId:{type:'string'},url:{type:'string'},title:{type:'string'},claim:{type:'string'},...structuredClone(evidenceQualityProperties)}};
  assert.throws(()=>strictContract(legacy),/required/);
  const schema=outputSchema(new Set(['synthetic']),true,true);
  const evidence=schema.properties.evidence.items;
  for(const key of ['itemId','url','title','claim'])assert.equal(evidence.properties[key].type,'string');
  for(const key of Object.keys(evidenceQualityProperties))assert(evidence.properties[key].anyOf.some(branch=>branch.type==='null'));
  const extraction=evidence.properties.extraction.anyOf.find(branch=>branch.type==='object');
  assert.equal(extraction.properties.status.type,'string');
  for(const key of ['observedAt','rowLabels','columnLabels','values'])assert(extraction.properties[key].anyOf.some(branch=>branch.type==='null'));
  assert.equal(schema.properties.assessments.minItems,1);assert.equal(schema.properties.assessments.maxItems,1);
  assert.deepEqual(schema.properties.generationEditorial.items.required,['itemId','kind','text','decision','reason','checks']);
  assert.equal(outputSchema(new Set(),true,true).properties.evidence.maxItems,0);
});

test('nullable wire declarations normalize only optional known fields and preserve quality holds',()=>{
  assert.deepEqual(reviewEvidenceQualityFields({claimKind:null,scope:null,sourceScope:null,extraction:null}),evidenceQualityFields({}));
  const sparse={claimKind:'product_specification',scope:{model:'Q06',trim:'two',market:'CN',modelYear:'2025'},extraction:{status:'complete'}};
  const wire={...sparse,scope:{...sparse.scope,observedAt:null},sourceScope:null,extraction:{status:'complete',observedAt:null,rowLabels:null,columnLabels:null,values:null}};
  assert.deepEqual(reviewEvidenceQualityFields(wire),evidenceQualityFields(sparse));
  for(const invalid of [{extraction:{status:null}},{extraction:{status:'complete',unexpected:null}},{scope:{model:'Q06',account:null}},{scope:[]},{claimKind:'verified_product'},{extraction:{status:'empty',values:['']}}])assert.throws(()=>reviewEvidenceQualityFields(invalid),{code:'ASSISTANT_INVALID_RESEARCH'});
  const incomplete=reviewEvidenceQualityFields({claimKind:'product_specification',scope:{model:'Q06',trim:null,market:'CN',modelYear:null,observedAt:null},sourceScope:null,extraction:null});
  assert.equal(evidenceQualityHolds([{itemId:'synthetic',...incomplete}],'likeavto')[0].reason,'specification_scope_incomplete');
  const mismatch=reviewEvidenceQualityFields({...wire,sourceScope:{model:'Q06',trim:'other',market:'CN',modelYear:'2025',observedAt:null}});
  assert.equal(evidenceQualityHolds([{itemId:'synthetic',...mismatch}],'likeavto')[0].reason,'specification_scope_mismatch');
  const inaccessible=reviewEvidenceQualityFields({...wire,extraction:{status:'access_challenge',observedAt:null,rowLabels:null,columnLabels:null,values:null}});
  assert.equal(evidenceQualityHolds([{itemId:'synthetic',...inaccessible}],'likeavto')[0].reason,'incomplete_extraction');
});

test('wire-null evidence still passes exact recipient/opened URL admission and never masks holds',()=>{
  const firstPass={text:'Synthetic',sources:[],proposals:[{itemId:'synthetic',kind:'reply_and_close',text:'Спасибо!'}],assessments:[{itemId:'synthetic',outcome:'reply',reason:'Friendly',tags:[]}]};
  const prepared=prepareAssistantRequest({purpose:'triage_review',account:'likeavto',items:[{id:'synthetic'}],firstPass});
  const source={itemId:'synthetic',url:'https://fixture.example/source',title:'Synthetic',claim:'A source statement',claimKind:'source_statement',scope:null,sourceScope:null,extraction:null};
  const value={...firstPass,evidence:[source]},trace={calls:1,openedUrls:[source.url]};
  const admitted=admitReviewEvidence(value,prepared,trace);
  assert.equal(admitted[0].claimKind,'source_statement');assert.equal(admitted[0].scope,undefined);
  assert.throws(()=>admitReviewEvidence({...value,evidence:[{...source,itemId:'foreign'}]},prepared,trace),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'RECIPIENT'});
  assert.throws(()=>admitReviewEvidence(value,prepared,{calls:1,openedUrls:[]}),{code:'ASSISTANT_INVALID_RESEARCH'});
  assert.throws(()=>admitReviewEvidence({...value,evidence:[{...source,claimKind:'product_specification',scope:{model:'Q06',trim:null,market:null,modelYear:null,observedAt:null}}]},prepared,trace),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'EVIDENCE_QUALITY'});
});

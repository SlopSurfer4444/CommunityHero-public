import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,compactOutputSchema,expandCompactOutput,singlePassInstructions,
  assistantInstructions,reviewInstructions,limitedReviewInstructions,admitSinglePassResult,singlePassMetadata,outputSchema,UNCAPPED_EVIDENCE_CONTRACT} from './assistant.mjs';
const sha=x=>createHash('sha256').update(x).digest('hex');
const pass={intent:'pass',companyRules:'pass',factualScope:'pass'};
const req=(ids=['a','b'])=>({purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',items:ids.map(id=>({id,text:'Supplied context'}))});
const row=(itemId,action='reply_and_close')=>({itemId,action,text:action==='reply_and_close'?`Exact final ${itemId}`:'',reason:'Supported decision',tags:[],editorial:action==='hold'?null:{decision:'accept',reason:'Exact final checked',checks:{...pass}},basis:action==='hold'?'unresolved':'context',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[]});
const candidate=(ids=['a','b'])=>({text:'Prepared',evidence:[],decisions:ids.map(id=>row(id))});
const trace={calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
const source=itemId=>({itemId,url:'https://example.com/exact',title:'Exact source',claim:'Narrow attributed statement',claimKind:'source_statement',scope:null,sourceScope:null,extraction:null});
const admit=(value,request=req(),observed=trace,images=undefined)=>admitSinglePassResult(expandCompactOutput(value,new Set(request.items.map(i=>i.id))),prepareAssistantRequest(request),observed,images);

test('compact selector is explicit, digest-bound and unavailable to old paid or other purposes',()=>{
 const request=req(),prepared=prepareAssistantRequest(request);assert.equal(prepared.compactOutput,true);
 assert.equal(JSON.parse(prepared.input).responseContract,'compact_decisions_v1');
 const old=structuredClone(request);delete old.responseContract;const legacy=prepareAssistantRequest(old);
 assert.equal(legacy.compactOutput,false);assert.notEqual(sha(prepared.input),sha(legacy.input));
 for(const mutate of [r=>r.responseContract='unknown',r=>r.responseContract=null,r=>delete r.preparationMode,r=>r.purpose='triage_review',r=>r.purpose='discussion']){
  const changed=req();mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
 }
 assert.deepEqual(outputSchema(legacy.ids,true,false,false,undefined,true).required,['text','sources','proposals','assessments','generationEditorial','evidence','moderationEvidence','decisionEvidence']);
});

test('one strict row expands exact editorial binding, holds and dependency proof without changing input',()=>{
 const value=candidate();value.decisions[1]=row('b','hold');value.decisions[1].dependsOnItemIds=['a'];const before=structuredClone(value);
 const expanded=expandCompactOutput(value,['a','b']);assert.deepEqual(value,before);
 assert.deepEqual(expanded.proposals,[{itemId:'a',kind:'reply_and_close',text:'Exact final a'}]);
 assert.equal(expanded.generationEditorial[0].text,expanded.proposals[0].text);assert.equal(expanded.assessments[1].outcome,'needs_attention');
 assert.deepEqual(expanded.decisionEvidence[1].dependsOnItemIds,['a']);assert.equal(expanded.sources.length,0);
 const result=admit(value);assert.equal(result.editorialEvidence.entries[0].textSha256,sha('Exact final a'));
 assert.deepEqual(result.decisionDependencies.entries[1],{itemId:'b',dependsOnItemIds:['a']});
 const schema=compactOutputSchema(new Set(['a','b']));assert.deepEqual(Object.keys(schema.properties),['text','evidence','decisions']);
 assert.equal(schema.properties.decisions.items.properties.editorial.anyOf[1].properties.text,undefined);
});

test('unknown nested fields, missing, foreign, duplicate recipients and invalid kinds fail closed',()=>{
 for(const mutate of [v=>v.secret='x',v=>v.decisions[0].extra=true,v=>v.decisions[0].editorial.text='Override',
  v=>v.decisions[0].editorial.checks.extra='pass',v=>v.decisions[0].itemId='foreign',v=>v.decisions[1].itemId='a',
  v=>v.decisions.pop(),v=>v.decisions[0].action='send',v=>v.decisions[0].tags=['invented'],
  v=>v.decisions[0].dependsOnItemIds=['foreign'],v=>v.decisions[0].dependsOnItemIds=['a'],v=>v.decisions[0].editorial=null,
  v=>{v.decisions[0].action='close';},v=>{v.decisions[0]=row('a','hold');v.decisions[0].editorial={decision:'accept',reason:'x',checks:pass};}]){
   const value=candidate();mutate(value);assert.throws(()=>expandCompactOutput(value,['a','b']),{code:'ASSISTANT_INVALID_RESPONSE'});
 }
 const value=candidate();value.evidence=[{...source('a'),privateExtra:'x'}];assert.throws(()=>expandCompactOutput(value,['a','b']),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('full source admission still holds unobserved web rows and dependent decisions, not independent rows',()=>{
 const request=req(['a','b','c']),value=candidate(['a','b','c']);value.evidence=[source('a')];
 Object.assign(value.decisions[0],{basis:'web',evidenceIndices:[0]});value.decisions[1].dependsOnItemIds=['a'];
 const held=admit(value,request);assert.deepEqual(held.admitted.proposals.map(p=>p.itemId),['c']);
 const observed=admit(value,request,{...trace,calls:1,openedUrls:['https://example.com/exact']});assert.equal(observed.admitted.proposals.length,3);
 const wrong=structuredClone(value);wrong.decisions[1].evidenceIndices=[0];assert.throws(()=>admit(wrong,request),{code:'ASSISTANT_INVALID_RESPONSE'});
 const incomplete=structuredClone(value);incomplete.evidence[0].extraction={status:'missing_table',observedAt:null,rowLabels:null,columnLabels:null,values:null};
 assert.deepEqual(admit(incomplete,request,{...trace,calls:1,openedUrls:['https://example.com/exact']}).admitted.proposals.map(p=>p.itemId),['c']);
});

test('editorial and missing-image holds remain per row after normalization',()=>{
 const value=candidate();value.decisions[0].editorial.checks.factualScope='uncertain';value.decisions[0].editorial.decision='hold';
 assert.deepEqual(admit(value).admitted.proposals.map(p=>p.itemId),['b']);
 assert.deepEqual(admit(candidate(),req(),trace,{blockedItemIds:['a'],failureEvidence:[]}).admitted.proposals.map(p=>p.itemId),['b']);
});

const binding={id:'angryspace-likeavto-v1',workspaceId:'local-pilot',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'likeavto'};
const ref={entryId:'rule-entry',versionId:'rule-version',hash:'a'.repeat(64)};
function moderationRequest(){const request=req();request.connectorBinding=binding;request.items.forEach(i=>Object.assign(i,{postKey:'post-a',moderationCapabilities:{delete:'supported',hide:'unsupported'}}));request.moderationContext={version:1,account:'LikeAvto',connectorBinding:binding,ruleRefs:[ref]};request.knowledgeManifest=[{...ref,kind:'rule',scope:{account:'LikeAvto',postKeys:[]}}];request.materials=[{id:'rule',kind:'rule',text:'Delete insults; preserve substantive criticism.',knowledgeEntryId:ref.entryId,knowledgeVersionId:ref.versionId}];return request;}
test('moderation remains bound to exact capability, active rule, company and editorial checks',()=>{
 const value=candidate(),request=moderationRequest();value.decisions[0]=row('a','delete');value.decisions[0].moderationRuleRefs=[ref];
 assert.deepEqual(admit(value,request).admitted.moderationEvidence.entries,[{itemId:'a',kind:'delete',ruleRefs:[ref]}]);
 for(const mutate of [r=>r.items[0].moderationCapabilities.delete='unknown',r=>r.moderationContext.ruleRefs=[],r=>r.knowledgeManifest[0].scope.postKeys=['other']]){
  const changed=structuredClone(request);mutate(changed);assert.deepEqual(admit(value,changed).admitted.proposals.map(p=>p.itemId),['b']);
 }
 const foreign=structuredClone(request);foreign.moderationContext.account='BAW Russia';assert.throws(()=>admit(value,foreign),{code:'ASSISTANT_INVALID_REQUEST'});
 const stale=structuredClone(value);stale.decisions[0].moderationRuleRefs[0].hash='b'.repeat(64);assert.deepEqual(admit(stale,request).admitted.proposals.map(p=>p.itemId),['b']);
 const missing=structuredClone(value);missing.decisions[0].moderationRuleRefs=[];assert.throws(()=>admit(missing,request),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('compact policy and current material lookup instructions bind exact prompts',()=>{
 const compact=singlePassInstructions('likeavto',true);assert.match(compact,/Apply ALL categories and conditions/);assert.match(compact,/Absence of a personal target does not alone exempt abuse/);assert.match(compact,/grants no action without an applicable current company rule/);
 assert.doesNotMatch(compact,/Repeat exact final text byte-for-byte|Return generationEditorial covering|Return decisionEvidence exactly/);
 assert.match(compact,/first inspect the supplied\ncompany database\/materials/);assert.match(compact,/does not grant new tool, account or publication permissions/);
 assert.equal(sha(singlePassInstructions()),'de674f4a4981abca1bc8df2dbde65caa0d12fc0a5d210ee736a084506837368a');
 assert.equal(sha(assistantInstructions()),'f5bdf937f768804cc4e1ce30f2389a87992a71881d00554da44e5971bc77a83b');
 assert.equal(sha(limitedReviewInstructions('likeavto',8)),'b2097db6ad5d4cc4a936d1f9f569a4a18ba9d791d6f9b12e4b393a80300a0858');
 assert.equal(sha(reviewInstructions()),'6ba228e71d2ba058ad2140a3f5d7d6ac2a2023675980385a1300088847dfa58b');
 const p=prepareAssistantRequest(req()),reviewed=admit(candidate());assert.equal(singlePassMetadata(p,reviewed).instructionSha256,sha(compact));
});

test('empty and maximum recipient coverage are bounded without truncating proof',()=>{
 assert.deepEqual(expandCompactOutput(candidate([]),[]).assessments,[]);
 const ids=Array.from({length:100},(_,n)=>`i-${n}`);assert.equal(expandCompactOutput(candidate(ids),ids).generationEditorial.length,100);
 const value=candidate();value.evidence=Array.from({length:301},()=>source('a'));assert.throws(()=>expandCompactOutput(value,['a','b']),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('new captured uncapped contract binds prompt and provenance with current quality guidance bound for both captured and absent variants',()=>{
 const request={...req(),researchLimitContract:UNCAPPED_EVIDENCE_CONTRACT},p=prepareAssistantRequest(request);
 assert.equal(p.uncappedEvidence,true);assert.equal(JSON.parse(p.input).researchLimitContract,UNCAPPED_EVIDENCE_CONTRACT);
 const old=prepareAssistantRequest(req());assert.equal(old.uncappedEvidence,false);assert.notEqual(sha(p.input),sha(old.input));
 const prompt=singlePassInstructions('likeavto',true,false,false,true);
 assert.match(prompt,/the relevant supporting sources in evidence/);assert.doesNotMatch(prompt,/1-3 sources/);
 assert.equal(sha(prompt),'23f3c94ef280a0b256d2f648cf3dd51f6c4fb0f76e0e028c43fd54055e77c111');
 const admitted=admitSinglePassResult(expandCompactOutput(candidate(),p.ids,true),p,trace);
 const metadata=singlePassMetadata(p,admitted);assert.equal(metadata.researchLimitContract,UNCAPPED_EVIDENCE_CONTRACT);
 assert.equal(metadata.instructionSha256,sha(prompt));assert.equal(metadata.inputSha256,sha(p.input));
 assert.equal(singlePassMetadata(old,admit(candidate())).researchLimitContract,undefined);
 for(const mutate of [r=>r.researchLimitContract=null,r=>r.researchLimitContract='unknown',r=>delete r.preparationMode,r=>r.purpose='triage_review']){
  const changed=structuredClone(request);mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
 }
});

test('tagged evidence has no source-count cap while byte and exact-index guards remain',()=>{
 const value=candidate();value.evidence=Array.from({length:301},()=>source('a'));
 assert.equal(expandCompactOutput(value,['a','b'],true).evidence.length,301);
 assert.equal(compactOutputSchema(new Set(['a','b']),true).properties.evidence.maxItems,undefined);
 value.evidence=Array.from({length:3000},()=>({...source('a'),claim:'x'.repeat(1000)}));
 assert.throws(()=>expandCompactOutput(value,['a','b'],true),{code:'ASSISTANT_INVALID_RESPONSE'});
 const bad=candidate();bad.evidence=[source('a')];bad.decisions[0].evidenceIndices=[1];
 assert.throws(()=>expandCompactOutput(bad,['a','b'],true),{code:'ASSISTANT_INVALID_RESPONSE'});
});

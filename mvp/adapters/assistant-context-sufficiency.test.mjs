import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,singlePassInstructions,singlePassMetadata,assistantCliArgs,
  expandCompactOutput,admitSinglePassResult} from './assistant.mjs';
const sha=value=>createHash('sha256').update(value).digest('hex');
const request=()=>({purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1',items:[{id:'a',text:'Technical question'},{id:'b',text:'Grounded friendly observation'}]});
const trace={calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
const candidate=()=>({text:'Prepared',evidence:[],decisions:['a','b'].map(itemId=>({itemId,action:'reply_and_close',text:'Bounded supported response',reason:'Supplied scope supports response',tags:[],editorial:{decision:'accept',reason:'Exact final claims supported',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}},basis:'context',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[]}))});
const source=()=>({itemId:'a',url:'https://example.com/technical',title:'Exact technical source',claim:'Narrow supported specification',claimKind:'source_statement',scope:null,sourceScope:null,extraction:null});
const admission=(value,observed=trace,images=undefined)=>admitSinglePassResult(expandCompactOutput(value,['a','b']),prepareAssistantRequest(request()),observed,images);

test('captured policy binds input and current quality guidance across instruction variants',()=>{
 const req=request(),p=prepareAssistantRequest(req);assert.equal(p.contextSufficient,true);assert.equal(JSON.parse(p.input).researchPolicy,'context_sufficient_v1');
 const old={...req};delete old.researchPolicy;const prior=prepareAssistantRequest(old);assert.equal(prior.contextSufficient,false);assert.notEqual(sha(p.input),sha(prior.input));
 assert.equal(sha(singlePassInstructions('likeavto',true,false)),'0be54358812bcd1b525a7b83eca50078794a4e2913b44c20bc94fabafcac6b37');
 assert.equal(sha(singlePassInstructions('likeavto',true,true)),'94de348997441f29fa9e05b02e7592b19b6fbd85baa8c5163f99cb017cf0f7ed');
 assert.equal(sha(singlePassInstructions()),'de674f4a4981abca1bc8df2dbde65caa0d12fc0a5d210ee736a084506837368a');
 const reviewed=admission(candidate());assert.equal(singlePassMetadata(p,reviewed).instructionSha256,sha(singlePassInstructions('likeavto',true,false,true)));
 for(const mutate of [r=>r.researchPolicy=null,r=>r.researchPolicy='unknown',r=>delete r.preparationMode,r=>delete r.responseContract,r=>r.purpose='triage_review']){const bad=request();mutate(bad);assert.throws(()=>prepareAssistantRequest(bad),{code:'ASSISTANT_INVALID_REQUEST'});}
});

test('policy retains enabled high web route and explicitly distinguishes necessary research from repetition',()=>{
 const instruction=singlePassInstructions('likeavto',true,false,true);
 assert.match(instruction,/Do not search solely to re-prove/);assert.match(instruction,/You MUST research an indispensable\nmissing public or technical fact/);
 assert.match(instruction,/present price, legal, availability/);assert.match(instruction,/Existing TTL alone does not establish freshness/);
 assert.match(instruction,/Never answer a technical\nquestion from model memory/);assert.match(instruction,/Do not evade\nneeded research with banter/);
 assert.match(instruction,/cannot establish this company's current stock/);assert.match(instruction,/Do not label a new web-derived assertion basis=context/);
 assert.match(instruction,/An attributed historical statement can remain attributed/);assert.match(instruction,/Never send customer identities/);
 const args=assistantCliArgs('synthetic',false,[],true);assert.ok(args.includes('web_search="live"'));assert.ok(args.includes('standalone_web_search'));assert.ok(args.includes('model_reasoning_effort="high"'));
});

test('exact supplied context can finish with no fresh evidence and no tool trace without dropping proofs',()=>{
 const result=admission(candidate());assert.equal(result.admitted.proposals.length,2);assert.equal(result.evidence.length,0);assert.equal(result.editorialEvidence.entries.length,2);
 assert.equal(result.decisionDependencies.entries.length,2);assert.equal(result.trace.calls,0);
});

test('new technical facts still require exact observed source and scope; unrelated context decision survives',()=>{
 const value=candidate();value.evidence=[source()];Object.assign(value.decisions[0],{basis:'web',evidenceIndices:[0]});
 assert.deepEqual(admission(value).admitted.proposals.map(p=>p.itemId),['b']);
 const observed={...trace,calls:1,openedUrls:[value.evidence[0].url]};assert.equal(admission(value,observed).admitted.proposals.length,2);
 assert.deepEqual(admission(value,{...observed,openedUrls:['https://example.com/other']}).admitted.proposals.map(p=>p.itemId),['b']);
 const mismatch=structuredClone(value);Object.assign(mismatch.evidence[0],{claimKind:'product_specification',scope:{model:'Exact',trim:'A',market:'China',modelYear:'2026',observedAt:null},sourceScope:{model:'Exact',trim:'A',market:'Japan',modelYear:'2021',observedAt:null}});
 assert.deepEqual(admission(mismatch,observed).admitted.proposals.map(p=>p.itemId),['b']);
 const dependency=structuredClone(value);dependency.decisions[1].dependsOnItemIds=['a'];assert.equal(admission(dependency).admitted.proposals.length,0);
});

test('incomplete evidence, private-data holds and image uncertainty cannot become actionable from policy flag',()=>{
 const value=candidate();value.evidence=[source()];Object.assign(value.decisions[0],{basis:'web',evidenceIndices:[0]});value.evidence[0].extraction={status:'access_challenge',observedAt:null,rowLabels:null,columnLabels:null,values:null};
 assert.deepEqual(admission(value,{...trace,calls:1,openedUrls:[value.evidence[0].url]}).admitted.proposals.map(p=>p.itemId),['b']);
 const held=candidate();Object.assign(held.decisions[0],{action:'hold',text:'',reason:'Current private stock requires company confirmation',editorial:null,basis:'unresolved'});
 assert.deepEqual(admission(held).admitted.proposals.map(p=>p.itemId),['b']);
 assert.deepEqual(admission(candidate(),trace,{blockedItemIds:['a'],failureEvidence:[]}).admitted.proposals.map(p=>p.itemId),['b']);
});

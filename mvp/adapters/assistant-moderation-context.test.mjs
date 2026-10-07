import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {projectModerationRuleSets,expandModerationRuleSets,SHARED_MODERATION_CONTEXT} from './assistant-moderation-context.mjs';
import {projectValidatedModelContext,serializeAssistantModelInput} from './assistant-model-context.mjs';
import {prepareAssistantRequest,admitSinglePassResult,singlePassInstructions,singlePassMetadata} from './assistant.mjs';

const sha = value => createHash('sha256').update(value).digest('hex');
const binding = {id:'test-likeavto',workspaceId:'local-pilot',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'likeavto'};
const ref = n => ({entryId:`knowledge-${sha('entry'+n)}`,versionId:`knowledge-version-${sha('version'+n)}`,hash:sha('version'+n)});
function fixture(count=5,ruleCount=3) {
  const refs=Array.from({length:ruleCount},(_,i)=>ref(i));
  return {account:'likeavto',purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',
    modelContextContract:SHARED_MODERATION_CONTEXT,connectorBinding:binding,
    items:Array.from({length:count},(_,n)=>({id:`item-${n}`,text:'Supplied comment',postKey:n<2?'p':n<4?'q':'z',moderationCapabilities:{delete:'supported',hide:'unsupported'}})),
    moderationContext:{version:1,account:'LikeAvto',connectorBinding:binding,ruleRefs:refs},
    materials:refs.map((r,n)=>({id:`rule-${n}`,kind:'rule',text:`Full rule ${n}: apply only on its actual scope.`,knowledgeEntryId:r.entryId,knowledgeVersionId:r.versionId})),
    knowledgeManifest:refs.map((r,n)=>({...r,kind:'rule',scope:{account:'LikeAvto',postKeys:n===1?['p']:n===2?['q']:[]}}))};
}
const trace = {calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
function result(req) {
  const proposals=req.items.map((i,n)=>({itemId:i.id,kind:n===0?'delete':'close',text:''}));
  return {text:'Prepared',sources:[],evidence:[],proposals,
    assessments:proposals.map(p=>({itemId:p.itemId,outcome:p.kind,reason:'Exact applicable rule and context',tags:[]})),
    generationEditorial:proposals.map(p=>({...p,decision:'accept',reason:'Final action checked',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}})),
    decisionEvidence:proposals.map(p=>({itemId:p.itemId,basis:'context',evidenceIndices:[],dependsOnItemIds:[]})),
    moderationEvidence:[{itemId:req.items[0].id,kind:'delete',ruleRefs:[req.moderationContext.ruleRefs[1]]}]};
}

test('exact shared sets round-trip distinct post scopes without mutating canonical evidence',()=>{
  const req=fixture(),original=structuredClone(req),prepared=prepareAssistantRequest(req),full=structuredClone(prepared.payload);
  const projected=JSON.parse(prepared.input),restored=expandModerationRuleSets(projected);
  assert.deepEqual(req,original);assert.deepEqual(prepared.payload,full);
  assert.equal(projected.moderationRuleSets.length,2);
  assert.equal(projected.items[0].moderationRuleSetId,projected.items[1].moderationRuleSetId);
  assert.notEqual(projected.items[0].moderationRuleSetId,projected.items[2].moderationRuleSetId);
  for(let n=0;n<req.items.length;n++)assert.deepEqual(restored.items[n].moderationRuleEntryIds,full.items[n].moderationRuleEntryIds);
  assert.deepEqual(projected.materials,full.materials);assert.deepEqual(projected.knowledgeManifest,full.knowledgeManifest);
  assert.deepEqual(projected.moderationContext,full.moderationContext);
  assert.deepEqual(projected.items.map(i=>i.moderationCapabilities),full.items.map(i=>i.moderationCapabilities));
  assert.deepEqual(projectModerationRuleSets(full),projectModerationRuleSets(full),'deterministic references');
});

test('full admission stays identical, including unsupported capability and wrong post rule holds',()=>{
  for(const mutate of [()=>{},r=>r.items[0].moderationCapabilities.delete='unknown',r=>r.items[0].postKey='q']){
    const req=fixture();mutate(req);const old=structuredClone(req);delete old.modelContextContract;
    const current=prepareAssistantRequest(req),previous=prepareAssistantRequest(old),wire=result(req);
    const actual=admitSinglePassResult(wire,current,trace),expected=admitSinglePassResult(wire,previous,trace);
    assert.deepEqual(actual,expected);
    assert.equal(actual.admitted.proposals.some(p=>p.itemId==='item-0'),req.items[0].postKey==='p'&&req.items[0].moderationCapabilities.delete==='supported');
    assert.deepEqual(current.payload.items,previous.payload.items,'admission sees complete original lists');
  }
});

test('only newly captured supported contract changes model input and prompt identity',()=>{
  const req=fixture(),old=structuredClone(req);delete old.modelContextContract;
  const a=prepareAssistantRequest(req),b=prepareAssistantRequest(old);
  assert.equal(b.sharedModeration,false);assert.equal(JSON.parse(b.input).moderationRuleSets,undefined);
  assert.notEqual(sha(a.input),sha(b.input));assert.equal(a.payload.modelContextContract,SHARED_MODERATION_CONTEXT);
  for(const mutate of [r=>r.modelContextContract=null,r=>r.modelContextContract='unknown',r=>delete r.responseContract,r=>delete r.preparationMode,r=>r.purpose='triage_review']){
    const bad=fixture();mutate(bad);assert.throws(()=>prepareAssistantRequest(bad),{code:'ASSISTANT_INVALID_REQUEST'});
  }
  const prompt=singlePassInstructions('likeavto',true,true);
  assert.match(prompt,/Never union another/);assert.match(prompt,/cite the short set ID/);
  const reviewed=admitSinglePassResult(result(req),a,trace);
  assert.equal(singlePassMetadata(a,reviewed).instructionSha256,sha(prompt));
  assert.equal(singlePassInstructions('likeavto',true),singlePassInstructions('likeavto',true,false));
  assert.equal(sha(singlePassInstructions('likeavto',true)),'0be54358812bcd1b525a7b83eca50078794a4e2913b44c20bc94fabafcac6b37');
  assert.equal(sha(singlePassInstructions('baw-russia',true)),'d14aa1721e7690c28d5dceef9e5d83abd9210a630d3d8de5eacad7ff8c7b1236');
});

test('repeat serialization after image staging preserves shared sets and exact photo evidence',()=>{
  const prepared=prepareAssistantRequest(fixture());
  prepared.payload.items[0].attachments=[{type:'photo',url:'https://example.com/exact-photo.png'}];
  prepared.payload.imageEvidence=[{itemId:'item-0',attachmentIndex:0,sha256:sha('image')}];
  const first=serializeAssistantModelInput(prepared.payload),second=serializeAssistantModelInput(prepared.payload);
  assert.equal(first,second);const model=JSON.parse(first);
  assert.deepEqual(model.items[0].attachments,prepared.payload.items[0].attachments);
  assert.deepEqual(model.imageEvidence,prepared.payload.imageEvidence);
  assert.deepEqual(expandModerationRuleSets(model).items[0].moderationRuleEntryIds,prepared.payload.items[0].moderationRuleEntryIds);
});

test('100 recipients and 47 full rules fit without deleting authority or recipient context',t=>{
  const req=fixture(100,47);req.knowledgeManifest.forEach(m=>m.scope.postKeys=[]);
  const p=prepareAssistantRequest(req),full=projectValidatedModelContext({...p.payload,modelContextContract:undefined});
  const originalBytes=Buffer.byteLength(JSON.stringify(full)),modelBytes=Buffer.byteLength(p.input);
  assert.ok(originalBytes-modelBytes>300000);assert.equal(JSON.parse(p.input).items.length,100);
  assert.equal(JSON.parse(p.input).materials.length,47);assert.equal(JSON.parse(p.input).moderationRuleSets.length,1);
  assert.deepEqual(expandModerationRuleSets(JSON.parse(p.input)).items,p.payload.items);
  t.diagnostic(`synthetic 100/47: ${originalBytes} -> ${modelBytes} bytes; no latency claim`);
});

test('small or unknown shapes remain intact and malformed reference round-trips fail',()=>{
  const small={modelContextContract:SHARED_MODERATION_CONTEXT,items:[{id:'a',moderationRuleEntryIds:[]},{id:'b',moderationRuleEntryIds:[]}]};
  assert.equal(projectModerationRuleSets(small),small);
  const malformed={...small,items:[{id:'a',moderationRuleEntryIds:['x','x']}]};assert.equal(projectModerationRuleSets(malformed),malformed);
  const projected=JSON.parse(prepareAssistantRequest(fixture()).input);
  for(const mutate of [v=>v.items[0].moderationRuleSetId='foreign',v=>v.items[0].moderationRuleEntryIds=['x'],
    v=>v.moderationRuleSets.push(v.moderationRuleSets[0]),v=>delete v.moderationRuleSets]){
    const bad=structuredClone(projected);mutate(bad);assert.throws(()=>expandModerationRuleSets(bad),/Invalid shared/);
  }
});

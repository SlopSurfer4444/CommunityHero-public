import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,compactOutputSchema,expandCompactOutput,admitSinglePassResult,singlePassMetadata,
  visualFollowupRequest,runPreparationVisualFollowup,VISUAL_NEED_CONTRACT} from './assistant.mjs';
const sha=value=>createHash('sha256').update(value).digest('hex');
const request=()=>({account:'likeavto',purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',visualNeedContract:VISUAL_NEED_CONTRACT,
 items:[{id:'a',postId:'p',text:'Which visible detail?'},{id:'b',postId:'p',text:'Thanks!'}],
 posts:[{id:'p',postKey:'post-p',title:'Exact title',text:'Exact caption',body:'Exact body',attachmentsState:'present',attachments:[0,1,2].map(i=>({type:'photo',url:`https://example.com/${i}.jpg`}))}],
 branches:[],materials:[{id:'speech',kind:'transcript',text:'Captured speech remains supplied'},{id:'screen',kind:'ocr',text:'Actually extracted screen text'}],knowledgeManifest:[]});
const row=(itemId,hold=false,need=null)=>({itemId,action:hold?'hold':'reply_and_close',text:hold?'':`Exact useful ${itemId}`,reason:hold?'Unseen indispensable photo detail':'Supplied context supports it',tags:hold?['missing_context']:[],
 editorial:hold?null:{decision:'accept',reason:'Exact wording checked',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}},basis:hold?'unresolved':'context',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[],visualNeed:need});
const need={postId:'p',attachmentIndices:[1],reason:'Identify the exact visible detail that changes this answer'};
const trace={calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
function admitted(req,rows,images={}){
 const prepared=prepareAssistantRequest(req),wire={text:'Prepared',evidence:[],decisions:rows};
 const expanded=expandCompactOutput(wire,prepared.ids,false,false,true);
 const result=admitSinglePassResult(expanded,prepared,trace,images);
 return {...result.admitted,runMetadata:{...singlePassMetadata(prepared,result,5),imageEvidence:images.manifest||[]}};
}
function runner(results,seen){return async(req,options)=>{
 seen.push(structuredClone(req));options.captureReceipt({traceSha256:sha(`synthetic-offline-trace-${seen.length}`)});
 const result=results[seen.length-1];if(result instanceof Error)throw result;
 return typeof result==='function'?result(req):structuredClone(result);
};}
test('captured selector is explicit while legacy requests retain absent semantics',()=>{
 const req=request(),p=prepareAssistantRequest(req);assert.deepEqual(p.payload.visualSelection,{version:1,postImages:[]});
 assert.equal(p.visualNeeds,true);const legacy=structuredClone(req);delete legacy.visualNeedContract;
 assert.equal(prepareAssistantRequest(legacy).payload.visualSelection,undefined);
 assert.notEqual(prepareAssistantRequest(legacy).input,p.input);
 assert.ok(compactOutputSchema(p.ids,false,false,true).properties.decisions.items.required.includes('visualNeed'));
 for(const change of [r=>r.visualNeedContract='unknown',r=>r.purpose='discussion',r=>delete r.responseContract]){
  const bad=request();change(bad);assert.throws(()=>prepareAssistantRequest(bad),{code:'ASSISTANT_INVALID_REQUEST'});
 }
});
test('hold-only visual declaration binds exact source and cannot replace supported siblings',()=>{
 const req=request(),first=admitted(req,[row('a',true,need),row('b')]),next=visualFollowupRequest(req,first);
 assert.deepEqual(next.items.map(i=>i.id),['a']);assert.deepEqual(next.visualSelection.postImages,[{itemId:'a',...need}]);
 assert.deepEqual(next.posts,req.posts);assert.deepEqual(next.materials,req.materials);
 assert.throws(()=>admitted(req,[row('a',false,need),row('b')]),{code:'ASSISTANT_INVALID_RESPONSE'});
 for(const change of [n=>n.postId='foreign',n=>n.attachmentIndices=[3],n=>n.attachmentIndices=[1,1]]){
  const bad=structuredClone(need);change(bad);assert.throws(()=>admitted(req,[row('a',true,bad),row('b')]),{code:'ASSISTANT_INVALID_RESPONSE'});
 }
});
test('one targeted pass recovers only declared held item and preserves original sibling proof',async()=>{
 const req=request(),first=admitted(req,[row('a',true,need),row('b')]),before=structuredClone(first),seen=[];
 const result=await runPreparationVisualFollowup(req,{runPass:runner([first,r=>admitted(r,[row('a')])],seen)});
 assert.equal(seen.length,2);assert.deepEqual(result.proposals.find(p=>p.itemId==='b'),first.proposals[0]);
  assert.deepEqual(result.editorialEvidence.entries.find(p=>p.itemId==='b'),first.editorialEvidence.entries[0]);
 assert.deepEqual(Object.keys(result.editorialEvidence).sort(),['contract','entries','version']);
 assert.equal(result.editorialEvidence.contract,'communityhero-editorial-v1');assert.deepEqual(result.runMetadata.editorialEvidence,result.editorialEvidence);
 assert.deepEqual(Object.keys(result.moderationEvidence).sort(),['entries','version']);
 assert.deepEqual(result.assessments.find(p=>p.itemId==='b'),first.assessments[1]);assert.equal(result.proposals.length,2);
 assert.equal(result.visualNeeds,undefined);assert.equal(result.runMetadata.visualFollowup.status,'completed');
 assert.equal(result.runMetadata.inputSha256,first.runMetadata.inputSha256);
 assert.equal(result.runMetadata.visualFollowup.firstPass.resultSha256,sha(JSON.stringify(first,(_,v)=>v&&typeof v==='object'&&!Array.isArray(v)?Object.fromEntries(Object.entries(v).sort(([a],[b])=>a<b?-1:a>b?1:0)):v)));
 assert.deepEqual(first,before,'Admission inputs never mutated');
});
test('failed targeted read preserves initial hold and supported sibling, without retry evidence',async()=>{
 const req=request(),first=admitted(req,[row('a',true,need),row('b')]),seen=[];
 const result=await runPreparationVisualFollowup(req,{runPass:runner([first,new Error('synthetic image/model failure')],seen)});
 assert.equal(seen.length,2);assert.deepEqual(result.proposals,first.proposals);assert.deepEqual(result.assessments,first.assessments);
 assert.equal(result.runMetadata.visualFollowup.status,'held');assert.equal(result.runMetadata.visualFollowup.retry,null);
 assert.deepEqual(result.runMetadata.imageEvidence,[]);
});
test('image failure and editorial failure cannot promote a visual candidate',async()=>{
 for(const kind of ['image','editorial']){
  const req=request(),first=admitted(req,[row('a',true,need),row('b')]),seen=[];
  const result=await runPreparationVisualFollowup(req,{runPass:runner([first,r=>{
   const candidate=row('a');if(kind==='editorial'){candidate.editorial.decision='hold';candidate.editorial.checks.factualScope='uncertain';}
   return admitted(r,[candidate],kind==='image'?{blockedItemIds:['a'],manifest:[]}:{manifest:[]});
  }],seen)});
  assert.equal(result.assessments.find(p=>p.itemId==='a').outcome,'needs_attention');assert.deepEqual(result.proposals,first.proposals);
 }
});
test('unresolved visual pass declares no third pass and no unsupported acceptance',async()=>{
 const req=request(),seen=[],first=admitted(req,[row('a',true,need),row('b')]);
 const result=await runPreparationVisualFollowup(req,{runPass:runner([first,r=>admitted(r,[row('a',true,need)])],seen)});
 assert.equal(seen.length,2);assert.deepEqual(result.proposals,first.proposals);assert.equal(result.visualNeeds,undefined);
});
test('no visual declaration or already selected input does not launch another pass',async()=>{
 for(const selected of [false,true]){
  const req=request();if(selected)req.visualSelection={version:1,postImages:[{itemId:'a',...need}]};
  const first=admitted(req,[row('a',selected,selected?need:null),row('b')]),seen=[];
  const result=await runPreparationVisualFollowup(req,{runPass:runner([first],seen)});
  assert.equal(seen.length,1);assert.equal(result.runMetadata.visualFollowup,undefined);
 }
});
test('malformed or foreign follow-up cannot grant acceptance; exhausted shared budget keeps hold',async()=>{
 const req=request(),first=admitted(req,[row('a',true,need),row('b')]);
 for(const failure of ['foreign','deadline']){
  const seen=[];let calls=0;
  const result=await runPreparationVisualFollowup(req,{now:()=>failure==='deadline'&&++calls>1?3_000_000:0,
   runPass:runner([first,r=>({...admitted(r,[row('a')]),proposals:[{itemId:'foreign',kind:'reply_and_close',text:'Not admitted'}]})],seen)});
  assert.deepEqual(result.proposals,first.proposals);assert.equal(result.runMetadata.visualFollowup.status,'held');
  assert.equal(seen.length,failure==='deadline'?1:2);
 }
});

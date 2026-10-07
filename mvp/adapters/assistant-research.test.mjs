import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,admitAssistantEvents,admitReviewEvidence,admitPublicResearchResult,reviewInstructions,publicResearchInstructions} from './assistant.mjs';
import {publicUrl} from './assistant-research.mjs';
const firstPass={text:'Разбор',sources:[],proposals:[],assessments:[{itemId:'a',outcome:'needs_attention',reason:'Нужна мощность',tags:['needs_fact']}]};
const prepared=()=>prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'}],firstPass});

test('both research prompts require literal absolute URL opens with uncapped activity and unchanged exact source proof',()=>{
 for(const instructions of [reviewInstructions(),publicResearchInstructions()]) {
   assert.match(instructions,/explicitly open its literal absolute\nhttp:\/\/ or https:\/\/ URL/);
   assert.match(instructions,/reference ID \(ref_id such as turn0search0\) are\ninsufficient/);
   assert.match(instructions,/Cite the exact URL you explicitly opened/);
   assert.match(instructions,/no numerical/);
   assert.doesNotMatch(instructions,/eight-call total|Never exceed the budget/);
   assert.match(instructions,/never invent support/);
 }
 const source={itemId:'a',url:'https://manufacturer.example/spec',title:'Spec',claim:'Engine power'};
 const value={evidence:[source],assessments:[{itemId:'a',outcome:'reply'}]};
 const trace=query=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{id:'w',type:'web_search',action:{type:'other'},query}}),true);
 for(const admit of [activity=>admitReviewEvidence(value,prepared(),activity),
   activity=>admitPublicResearchResult({text:'A sourced answer',sources:[source]},activity)]) {
   assert.throws(()=>admit(trace('turn0search0')),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'UNOBSERVED_URL'});
   assert.doesNotThrow(()=>admit(trace(source.url)));
 }
 const overBudget=Array.from({length:9},(_,i)=>JSON.stringify({type:'item.completed',item:{id:`w${i}`,type:'web_search',action:{type:'other'},query:source.url}})).join('\n');
 assert.throws(()=>admitAssistantEvents(overBudget,true),{code:'ASSISTANT_RESEARCH_LIMIT'});
});
test('review first pass is validated against exact IDs and projected, not trusted',()=>{
 assert.equal(prepared().review,true);
 assert.throws(()=>prepareAssistantRequest({purpose:'triage_review',items:[{id:'b'}],firstPass}),{code:'ASSISTANT_INVALID_RESPONSE'});
 assert.equal(prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'}],firstPass:{...firstPass,secret:'hidden'}}).input.includes('hidden'),false);
});
test('only review admits web activity; arbitrary commands, files and tools fail closed',()=>{
 const line=item=>JSON.stringify({type:'item.completed',item});
 assert.throws(()=>admitAssistantEvents(line({id:'w',type:'web_search'})),{code:'ASSISTANT_ISOLATION_FAILED'});
 for(const type of ['command_execution','file_change','mcp_tool_call','collab_tool_call'])
  assert.throws(()=>admitAssistantEvents(line({id:'x',type}),true),{code:'ASSISTANT_ISOLATION_FAILED'});
 const events=Array.from({length:9},(_,i)=>line({id:String(i),type:'web_search'})).join('\n');
 assert.throws(()=>admitAssistantEvents(events,true),{code:'ASSISTANT_RESEARCH_LIMIT'});
});
test('source provenance requires opened public URL and exact target; URL alone is not evidence',()=>{
 const source={itemId:'a',url:'https://manufacturer.example/spec',title:'Spec',claim:'Engine power'};
 const value={evidence:[source],assessments:[{itemId:'a',outcome:'reply'}]};
 const trace={calls:1,openedUrls:[source.url]};
 assert.equal(admitReviewEvidence(value,prepared(),trace)[0].trust,'source_only');
 assert.throws(()=>admitReviewEvidence(value,prepared(),{...trace,openedUrls:[]}),{code:'ASSISTANT_INVALID_RESEARCH'});
 assert.throws(()=>admitReviewEvidence({...value,evidence:[{...source,itemId:'b'}]},prepared(),trace),{code:'ASSISTANT_INVALID_RESEARCH'});
 assert.throws(()=>admitReviewEvidence({...value,evidence:[]},prepared(),trace),{code:'ASSISTANT_INVALID_RESEARCH'});
 for(const url of ['http://127.0.0.1/a','http://10.0.0.1/a','https://user:pass@example.com','https://example.com/\nx','file:///C:/private'])assert.equal(publicUrl(url),null);
});
test('cached timestamps and narrow case history survive allowlists without arbitrary fields',()=>{
 const p=prepareAssistantRequest({items:[{id:'a'}],materials:[{id:'r',fetchedAt:'today',expiresAt:'tomorrow',itemIds:['a','other'],secret:'hidden'}],
  customerCases:[{itemId:'a',scope:'account_platform_author',historyComplete:false,secret:'hidden',messages:[{itemId:'m',text:'Claim',claimType:'customer_statement',password:'hidden'}],priorContractRequests:[{replyId:'r',text:'Номер договора?',createdAt:'today'}]}]}).payload;
 assert.equal(p.materials[0].expiresAt,'tomorrow');assert.deepEqual(p.materials[0].itemIds,['a']);
 assert.equal(p.customerCases[0].messages[0].claimType,'customer_statement');assert.ok(!JSON.stringify(p).includes('hidden'));
 assert.throws(()=>prepareAssistantRequest({items:[{id:'a'}],customerCases:[{itemId:'b'}]}),{code:'ASSISTANT_INVALID_REQUEST'});
 assert.match(reviewInstructions(),/time-sensitive claims always require a fresh source/);
 assert.match(reviewInstructions(),/MUST search with web\.run before retaining that hold/);
 assert.doesNotMatch(reviewInstructions(),/hosted web_search/);
});

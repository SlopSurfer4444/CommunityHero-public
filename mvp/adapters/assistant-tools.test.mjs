import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,generationMetadata,prepareAssistantRequest,validateAssistantResult,
  preparePublicResearchRequest,admitPublicResearchResult,assistantCliArgs} from './assistant.mjs';
import {outputSchema} from './assistant.mjs';

const names=['search_comments','workspace_stats','read_comments','set_workflow','navigate','research_public',
  'prepare_action_review','execute_action_review'];
const assistantTools={version:1,callsRemaining:4,roundsRemaining:3,
  definitions:names.map(name=>({name,description:`Use ${name}`,parameters:{type:'object',properties:{}}}))};
const base={text:'Проверяю локальные данные.',sources:[],proposals:[],lookup:null};
const prepared=()=>prepareAssistantRequest({purpose:'discussion',items:[{id:'a',revision:4}],assistantTools,
  toolResults:[{id:'lookup-1',name:'search_comments',ok:true,result:{total:1,items:[{id:'a',revision:4}]}}]});

test('discussion receives bounded Rust tool catalog and cumulative results without extra fields',()=>{
  const request=prepareAssistantRequest({purpose:'discussion',assistantTools:{...assistantTools,secret:'hidden',definitions:assistantTools.definitions.map(d=>({...d,secret:'hidden'}))},
    toolResults:[{id:'one',name:'workspace_stats',ok:true,result:{total:12},secret:'hidden'},
      {id:'two',name:'read_comments',ok:false,error:{code:'not_found',message:'Unknown item',private:'hidden'}}]});
  assert.equal(request.assistantTools.callsRemaining,4);
  assert.equal(request.lookupAllowed,false);
  assert.deepEqual(request.payload.toolResults,[{id:'one',name:'workspace_stats',ok:true,result:{total:12}},
    {id:'two',name:'read_comments',ok:false,error:{code:'not_found',message:'Unknown item'}}]);
  assert.equal(request.input.includes('hidden'),false);
  assert.throws(()=>prepareAssistantRequest({purpose:'triage',assistantTools}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({toolResults:[{id:'one'}]}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('server time and workspace zone reach discussion without arbitrary context fields',()=>{
  const request=prepareAssistantRequest({purpose:'discussion',currentTime:'2026-09-23T04:11:44Z',timezoneHint:'Europe/Moscow',
    assistantTools,secret:'hidden'});
  assert.equal(request.payload.currentTime,'2026-09-23T04:11:44Z');
  assert.equal(request.payload.timezoneHint,'Europe/Moscow');
  assert.equal(request.input.includes('hidden'),false);
  assert.match(assistantInstructions(false,'likeavto',true),/local\s+calendar-day boundaries/);
  assert.match(assistantInstructions(false,'likeavto',true),/syncFreshness and coverage/);
  for(const patch of [{currentTime:'tomorrow'},{timezoneHint:'Bad/../../Zone'},{timezoneHint:'Unknown/Zone'}])
    assert.throws(()=>prepareAssistantRequest({purpose:'discussion',assistantTools,...patch}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({purpose:'triage',currentTime:'2026-09-23T04:11:44Z'}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('structured output has closed typed argument objects and omits unused null fields',()=>{
  const tools={version:1,callsRemaining:1,roundsRemaining:1,definitions:[{name:'workspace_stats',description:'Exact local totals',
    parameters:{type:'object',properties:{query:{type:'string'},limit:{type:'integer',minimum:1,maximum:20}},required:[]}}]};
  const prepared=prepareAssistantRequest({purpose:'discussion',assistantTools:tools});
  const schema=outputSchema(prepared.ids,false,false,false,prepared.assistantTools);
  const variant=schema.properties.toolCalls.anyOf[1].items.anyOf[0];
  assert.equal(variant.additionalProperties,false);
  assert.equal(variant.properties.arguments.additionalProperties,false);
  assert.deepEqual(variant.properties.arguments.required,['query','limit']);
  assert.deepEqual(variant.properties.arguments.properties.query.anyOf,[{type:'string'},{type:'null'}]);
  const result=validateAssistantResult({...base,toolCalls:[{id:'stats',name:'workspace_stats',arguments:{query:null,limit:null}}]},
    prepared.ids,false,false,prepared.assistantTools);
  assert.deepEqual(result.toolCalls,[{id:'stats',name:'workspace_stats',arguments:{}}]);
  assert.equal(validateAssistantResult({...base,text:'',toolCalls:[{id:'stats',name:'workspace_stats',arguments:{}}]},
    prepared.ids,false,false,prepared.assistantTools).text,'Проверяю данные в рабочей области.');
  assert.throws(()=>validateAssistantResult({...base,text:'',toolCalls:[]},prepared.ids,false,false,prepared.assistantTools),
    {code:'ASSISTANT_INVALID_RESPONSE'});
  try {validateAssistantResult({...base,toolCalls:[{id:'bad',name:'workspace_stats',arguments:{limit:-1}}]},
    prepared.ids,false,false,prepared.assistantTools);assert.fail('Expected rejection');}
  catch(failure){assert.equal(failure.code,'ASSISTANT_INVALID_RESPONSE');assert.equal(failure.validationCategory,'TOOL_ARGUMENTS');}
});

test('several typed read requests are accepted but cannot carry a proposal',()=>{
  const p=prepared();
  const calls=[
    {id:'search',name:'search_comments',arguments:{query:'',workflow:'attention',limit:20,offset:0}},
    {id:'stats',name:'workspace_stats',arguments:{query:'',topic:'question'}},
    {id:'read',name:'read_comments',arguments:{itemIds:['a']}},
    {id:'open',name:'navigate',arguments:{kind:'comment',itemId:'a'}}
  ];
  assert.deepEqual(validateAssistantResult({...base,toolCalls:calls},p.ids,false,false,p.assistantTools).toolCalls,calls);
  assert.throws(()=>validateAssistantResult({...base,toolCalls:calls,proposals:[{itemId:'a',kind:'close',text:''}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.throws(()=>validateAssistantResult({...base,toolCalls:[...calls,calls[0]]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.throws(()=>validateAssistantResult({...base,toolCalls:[calls[0],{...calls[1],id:'search'}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('local workflow request requires exact revision and forbids external actions',()=>{
  const p=prepared();
  const valid={id:'move',name:'set_workflow',arguments:{items:[{itemId:'a',expectedRevision:4}],workflow:'waiting',waitingReason:'Ждём уточнение'}};
  assert.deepEqual(validateAssistantResult({...base,toolCalls:[valid]},p.ids,false,false,p.assistantTools).toolCalls,[valid]);
  for(const argumentsPatch of [
    {items:[{itemId:'a'}],workflow:'waiting'},
    {items:[{itemId:'a',expectedRevision:4}],workflow:'closed'},
    {items:[{itemId:'a',expectedRevision:4}],workflow:'waiting',publish:true},
    {items:[{itemId:'a',expectedRevision:-1}],workflow:'attention'}
  ])assert.throws(()=>validateAssistantResult({...base,toolCalls:[{...valid,arguments:argumentsPatch}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
  for(const name of ['shell','publish','approve','delete','web.run'])
    assert.throws(()=>validateAssistantResult({...base,toolCalls:[{id:'x',name,arguments:{}}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('tool budget closes further requests while final answer and proposals remain available',()=>{
  const zero={...assistantTools,callsRemaining:0,roundsRemaining:0};
  const p=prepareAssistantRequest({purpose:'discussion',items:[{id:'a'}],assistantTools:zero});
  assert.deepEqual(validateAssistantResult({...base,toolCalls:[]},p.ids,false,false,p.assistantTools).toolCalls,[]);
  assert.deepEqual(validateAssistantResult({...base,toolCalls:null},p.ids,false,false,p.assistantTools).toolCalls,[]);
  assert.throws(()=>validateAssistantResult({...base,toolCalls:[{id:'s',name:'workspace_stats',arguments:{}}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.deepEqual(validateAssistantResult({...base,toolCalls:[],proposals:[{itemId:'a',kind:'reply_and_close',text:'Спасибо!'}]},p.ids,false,false,p.assistantTools).proposals,
    [{itemId:'a',kind:'reply_and_close',text:'Спасибо!'}]);
  assert.throws(()=>validateAssistantResult({...base,toolCalls:[{id:'x',name:'navigate',arguments:{kind:'queue'}}]},new Set(),false,false),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('action review calls carry exact references but cannot carry text or approval claims',()=>{
  const p=prepared();
  const prepare={id:'prepare',name:'prepare_action_review',arguments:{mode:'execute_prepared',
    items:[{id:'a',revision:4,proposalId:'proposal-a',proposalRevision:2}]}};
  const execute={id:'execute',name:'execute_action_review',arguments:{reviewId:'review-1'}};
  for(const call of [prepare,execute])
    assert.deepEqual(validateAssistantResult({...base,toolCalls:[call]},p.ids,false,false,p.assistantTools).toolCalls,[call]);
  for(const argumentsPatch of [
    {mode:'execute_prepared',items:[{id:'a',revision:4,proposalId:'p'}]},
    {mode:'execute_prepared',items:[{id:'a',revision:4}],approved:true},
    {mode:'publish',items:[{id:'a',revision:4}]},
    {mode:'close_without_reply',items:[{id:'a',revision:4,text:'delete'}]}
  ])assert.throws(()=>validateAssistantResult({...base,toolCalls:[{...prepare,arguments:argumentsPatch}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.throws(()=>validateAssistantResult({...base,toolCalls:[{...execute,arguments:{reviewId:'review-1',confirmed:true}}]},p.ids,false,false,p.assistantTools),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('discussion instructions and provenance identify application tools without altering preparation',()=>{
  const prompt=assistantInstructions(false,'likeavto',true);
  assert.match(prompt,/assistantTools lists the only application tools/);
  assert.match(prompt,/Rust application/);
  assert.match(prompt,/exact local totals/);
  assert.match(prompt,/operator explicitly asks/);
  assert.doesNotMatch(prompt,/one application search/);
  assert.doesNotMatch(assistantInstructions(true),/toolCalls as an array/);
  assert.notEqual(generationMetadata('input',false,0,'likeavto',true).instructionSha256,
    generationMetadata('input',false).instructionSha256);
});

test('public research receives only a bounded public query and admits only opened sources',()=>{
  const prepared=preparePublicResearchRequest({account:'likeavto',query:'  Model Q dimensions 2026  ',messages:[{text:'private message'}],items:[{id:'secret'}]});
  assert.deepEqual(JSON.parse(prepared.input),{query:'Model Q dimensions 2026'});
  assert.equal(prepared.input.includes('private'),false);
  for(const query of ['', 'x', 'x'.repeat(1001), 'public\nprivate'])
    assert.throws(()=>preparePublicResearchRequest({query}),{code:'ASSISTANT_INVALID_REQUEST'});
  const source={title:'Manufacturer',url:'https://example.com/spec#section',claim:'Dimensions listed'};
  assert.deepEqual(admitPublicResearchResult({text:'Проверены размеры.',sources:[source],secret:'hidden'},
    {openedUrls:['https://example.com/spec']}),{text:'Проверены размеры.',sources:[{...source,url:'https://example.com/spec',trust:'source_only'}]});
  assert.throws(()=>admitPublicResearchResult({text:'Claim',sources:[source]},{openedUrls:[]}),{code:'ASSISTANT_INVALID_RESEARCH'});
  assert.throws(()=>admitPublicResearchResult({text:'Claim',sources:[{...source,url:'http://127.0.0.1/private'}]},
    {openedUrls:['http://127.0.0.1/private']}),{code:'ASSISTANT_INVALID_RESEARCH'});
  assert.ok(assistantCliArgs('C:/private/research',true).includes('standalone_web_search'));
});

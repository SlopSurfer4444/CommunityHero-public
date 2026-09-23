import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,admitAssistantEvents,admitReviewEvidence,admitPublicResearchResult,researchAdmissionDiagnostic,persistResearchDiagnostic,withAssistantLane} from './assistant.mjs';
import {safeError} from './bridge.mjs';

const source={itemId:'a',url:'https://manufacturer.example/spec',title:'Specification',claim:'Engine power'};
const prepared=prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'}],firstPass:{text:'Review',sources:[],proposals:[],assessments:[{itemId:'a',outcome:'needs_attention',reason:'Need power',tags:['needs_fact']}]}});
const value={evidence:[source],assessments:[{itemId:'a',outcome:'reply'}]};
const trace={calls:1,openedUrls:[source.url]};
const admit=(evidence,activity=trace)=>admitReviewEvidence({...value,evidence},prepared,activity);

test('research validators retain fail-closed base code and emit distinct safe categories',()=>{
  const cases=[
    ['MISSING_EVIDENCE',()=>admit(undefined)],
    ['FIELDS',()=>admit(Array(31).fill(source))],
    ['FIELDS',()=>admit([{...source,title:''}])],
    ['FIELDS',()=>admit([{...source,url:'http://127.0.0.1/private'}])],
    ['RECIPIENT',()=>admit([{...source,itemId:'foreign'}])],
    ['UNOBSERVED_URL',()=>admit([source],{calls:1,openedUrls:[]})],
    ['UNATTRIBUTED_REPLY',()=>admit([])],
    ['ACTIVITY_ID',()=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{type:'web_search'}}),true)],
  ];
  for(const [category,run] of cases)assert.throws(run,e=>{
    assert.equal(e.code,'ASSISTANT_INVALID_RESEARCH');
    assert.equal(e.researchCategory,category);
    const code=`ASSISTANT_INVALID_RESEARCH_${category}`;
    assert.deepEqual(safeError(e),{ok:false,error:{code,message:code}});
    return true;
  });
  assert.deepEqual(admit([source]),[{...source,trust:'source_only'}]);
});

test('safeError exports only an allowed research category and never raw error properties',()=>{
  const sensitive={message:'Bearer private-token',url:'https://private.example/secret',request:{text:'private comment'},stack:'private stack'};
  for(const researchCategory of ['UNOBSERVED_URL','SECRET_TOKEN','https://private.example/secret',null,{toString:()=> 'FIELDS'}]){
    const code=researchCategory==='UNOBSERVED_URL'?'ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL':'ASSISTANT_INVALID_RESEARCH';
    assert.deepEqual(safeError({...sensitive,code:'ASSISTANT_INVALID_RESEARCH',researchCategory}),{ok:false,error:{code,message:code}});
  }
  assert.equal(safeError({...sensitive,code:'ASSISTANT_INVALID_RESEARCH_SECRET_TOKEN'}).error.code,'ASSISTANT_INVALID_RESEARCH');
  assert.equal(safeError({...sensitive,code:'ASSISTANT_INVALID_RESEARCH_FIELDS'}).error.code,'ASSISTANT_INVALID_RESEARCH_FIELDS');
  assert.equal(safeError({...sensitive,code:'contains private text'}).error.code,'ADAPTER_UNAVAILABLE');
  assert.equal(safeError({...sensitive,code:'ASSISTANT_FAILED',researchCategory:'FIELDS'}).error.code,'ASSISTANT_FAILED');
});

test('safeError carries only fixed response-validation categories to Rust',()=>{
  for(const category of ['TOOL_ARGUMENTS','CORE_FIELDS','OUTPUT_JSON']) {
    const code=`ASSISTANT_INVALID_RESPONSE_${category}`;
    assert.deepEqual(safeError({code:'ASSISTANT_INVALID_RESPONSE',validationCategory:category,message:'private output'}),{ok:false,error:{code,message:code}});
  }
  for(const category of ['SECRET_TOKEN','https://private.example',null,{toString:()=> 'CORE_FIELDS'}]) {
    assert.deepEqual(safeError({code:'ASSISTANT_INVALID_RESPONSE',validationCategory:category,message:'private output'}),{ok:false,error:{code:'ASSISTANT_INVALID_RESPONSE',message:'ASSISTANT_INVALID_RESPONSE'}});
  }
  assert.equal(safeError({code:'ASSISTANT_INVALID_RESPONSE_SECRET_TOKEN'}).error.code,'ASSISTANT_INVALID_RESPONSE');
});

test('research admission normalizes only URL syntax and never substitutes another observed page',()=>{
  const event=(type,action,query)=>JSON.stringify({type,item:{id:'web-1',type:'web_search',action,query}});
  const activity=admitAssistantEvents(event('item.completed',{type:'open_page',url:'https://MANUFACTURER.example:443/spec#engine'}),true);
  assert.deepEqual(admit([source],activity),[{...source,trust:'source_only'}]);
  for(const url of ['https://manufacturer.example/spec/','http://manufacturer.example/spec',
    'https://manufacturer.example/spec?version=other','https://manufacturer.example/another']) {
    assert.throws(()=>admit([{...source,url}],activity),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'UNOBSERVED_URL'});
  }
  for(const stream of [
    event('item.started',{type:'open_page',url:source.url}),
    event('item.completed',{type:'search',query:source.url},source.url),
    event('item.completed',{type:'other'},'turn0search0'),
  ]) {
    assert.throws(()=>admit([source],admitAssistantEvents(stream,true)),{code:'ASSISTANT_INVALID_RESEARCH',researchCategory:'UNOBSERVED_URL'});
  }
});

test('failed URL proof reports fixed comparison/action categories and hashes without admitting variants',()=>{
  const events=[
    {id:'search',action:{type:'search'},query:'private query text'},
    {id:'reference',action:{type:'other'},query:'turn0search0'},
    {id:'structured',action:{type:'other'},query:'{"open":[{"ref_id":"private"}]}'},
    {id:'opened',action:{type:'open_page',url:source.url}},
  ].map(item=>JSON.stringify({type:'item.completed',item:{type:'web_search',...item}})).join('\n');
  const activity=admitAssistantEvents(events,true);
  const hash=url=>createHash('sha256').update(url).digest('hex');
  for(const [url,comparison] of [[source.url+'/','path_variant'],[source.url+'?a=1','query_variant'],
    [source.url.replace('https:','http:'),'scheme_variant'],['https://another.example/page','not_observed']]) {
    for(const run of [()=>admit([{...source,url}],activity),
      ()=>admitPublicResearchResult({text:'Reply',sources:[{...source,url}]},activity)]) {
      assert.throws(run,error=>{
        assert.equal(error.researchCategory,'UNOBSERVED_URL');
        const d=error.researchDiagnostic;
        assert.equal(d.openedUrlCount,1);assert.equal(d.unobservedUrlCount,1);
        assert.deepEqual(d.openedUrlSha256,[hash(source.url)]);
        assert.deepEqual(d.unobserved,[{urlSha256:hash(url),comparison}]);
        assert.deepEqual(d.completedActivity.map(v=>v.locatorKind),['other','reference_id','structured_locator','absolute_url']);
        assert.doesNotMatch(JSON.stringify(d),/https?:|private|manufacturer|turn0search0|Engine/);
        return true;
      });
    }
  }
  assert.deepEqual(admit([source],activity),[{...source,trust:'source_only'}]);
  const d=researchAdmissionDiagnostic([source],{calls:1,openedUrls:[],completedActivity:[{action:'private',locatorKind:'private',urlSha256:{toString:()=> 'a'.repeat(64)},secret:'private'}]});
  assert.equal(d.unobserved[0].comparison,'no_completed_literal_open');
  assert.deepEqual(d.completedActivity,[{action:'unknown',locatorKind:'other'}]);
});

test('sanitized lane receipts survive run cleanup, retain at most 32 own files and never mask failure',async()=>{
  const lane=await fs.mkdtemp(path.join(os.tmpdir(),'ch-research-diagnostics-'));
  try {
    const directory=path.join(lane,'research-diagnostics');await fs.mkdir(directory);
    const foreign=path.join(directory,'operator-note.json');await fs.writeFile(foreign,'keep');
    for(let i=0;i<34;i++)await fs.writeFile(path.join(directory,`research-failure-${1000000000000+i}-00000000-0000-0000-0000-000000000000.json`),'{}');
    let failure;
    try{admit([source],{calls:1,openedUrls:[]});}catch(error){failure=error;}
    failure.researchDiagnostic.secret='PRIVATE_TEXT';
    failure.researchDiagnostic.completedActivity=Array(100).fill({action:'PRIVATE_TEXT',locatorKind:'PRIVATE_TEXT',urlSha256:'PRIVATE_TEXT',query:'PRIVATE_TEXT'});
    const input='PRIVATE_TEXT';const prompt='communityhero-drafting-v15-review-exact-url-verification';
    assert.equal(await persistResearchDiagnostic(lane,failure,input,prompt),true);
    const files=(await fs.readdir(directory)).filter(name=>name.startsWith('research-failure-'));
    assert.equal(files.length,32);assert.equal(await fs.readFile(foreign,'utf8'),'keep');
    const newest=files.sort().at(-1);const text=await fs.readFile(path.join(directory,newest),'utf8');const receipt=JSON.parse(text);
    assert.equal(receipt.inputSha256,createHash('sha256').update(input).digest('hex'));
    assert.equal(receipt.reason,'UNOBSERVED_URL');assert.equal(receipt.promptVersion,prompt);
    assert.equal(receipt.completedActivity.length,8);assert.ok(Buffer.byteLength(text)<=16000);
    assert.doesNotMatch(text,/PRIVATE_TEXT|https?:|manufacturer|Specification|Engine power/);
    assert.equal(await persistResearchDiagnostic(path.join(lane,'missing'),failure,input,prompt),false);
    assert.equal(failure.code,'ASSISTANT_INVALID_RESEARCH');assert.equal(failure.researchCategory,'UNOBSERVED_URL');
    assert.equal(await persistResearchDiagnostic(lane,failure,input,'PRIVATE_TEXT'),false);
    assert.equal(await persistResearchDiagnostic(lane,{...failure,researchCategory:'FIELDS'},input,prompt),false);
    assert.equal((await fs.readdir(directory)).length,33);
    await assert.rejects(withAssistantLane(lane,'preparation',async home=>{
      await fs.writeFile(path.join(home,'response.json'),'PRIVATE_TEXT');
      assert.equal(await persistResearchDiagnostic(path.dirname(home),failure,input,prompt),true);
      throw failure;
    }),error=>error===failure);
    assert.deepEqual(await fs.readdir(path.join(lane,'preparation')),['research-diagnostics']);
    assert.equal((await fs.readdir(path.join(lane,'preparation','research-diagnostics'))).length,1);
  } finally {
    assert.equal(path.dirname(lane),os.tmpdir());assert.ok(path.basename(lane).startsWith('ch-research-diagnostics-'));
    await fs.rm(lane,{recursive:true,force:true});
  }
});

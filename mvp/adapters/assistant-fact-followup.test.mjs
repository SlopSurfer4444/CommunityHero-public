import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,compactOutputSchema,expandCompactOutput,admitSinglePassResult,
  preparePublicResearchRequest,admitPublicResearchResult,singlePassMetadata,deterministicMediaHold,assistantLaneForRequest} from './assistant.mjs';
import {FACT_DEPENDENCY_INSTRUCTIONS} from './assistant-fact-followup.mjs';
import {createHash} from 'node:crypto';
const request=()=>({purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',factDependencyContract:'targeted_public_v1',items:[{id:'public',text:'Exact technical question'},{id:'other',text:'Grounded comment'}]});
const candidate=()=>({text:'Prepared',evidence:[],decisions:[
  {itemId:'public',action:'hold',text:'',reason:'Indispensable exact fact still missing',tags:['needs_fact'],editorial:null,basis:'unresolved',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[],factDependency:{kind:'missing_public_fact',claimScope:'Manufacturer thermal operating specification for the supplied variant',publicQuery:'manufacturer thermal operating specification exact variant'}},
  {itemId:'other',action:'reply_and_close',text:'Grounded reply',reason:'Supplied context supports engagement',tags:[],editorial:{decision:'accept',reason:'Exact final claim supported',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}},basis:'context',evidenceIndices:[],dependsOnItemIds:[],moderationRuleRefs:[],factDependency:null}]});
const trace={calls:0,openedUrls:[],completedActivity:[],webCallLimit:null};
const admission=value=>admitSinglePassResult(expandCompactOutput(value,['public','other'],false,true),prepareAssistantRequest(request()),trace);
const source=()=>({title:'Manufacturer source',url:'https://manufacturer.example/specification',claim:'Exact supported thermal operating range',claimKind:'source_statement',scope:null,sourceScope:null,extraction:null});

test('opt-in typed public declaration requests only held recipient and preserves independent proposal',()=>{
  const out=admission(candidate());assert.deepEqual(out.admitted.proposals.map(p=>p.itemId),['other']);
  assert.deepEqual(out.admitted.factDependencies,[{itemId:'public',...candidate().decisions[0].factDependency}]);
  assert.equal(prepareAssistantRequest(request()).factDependencies,true);
  const legacy=request();delete legacy.factDependencyContract;
  assert.equal(prepareAssistantRequest(legacy).factDependencies,false);
  assert.ok(!Object.hasOwn(compactOutputSchema(new Set(['public'])).properties.decisions.items.properties,'factDependency'));
  assert.match(FACT_DEPENDENCY_INSTRUCTIONS,/customer identity/);
  assert.match(FACT_DEPENDENCY_INSTRUCTIONS,/No indispensable unresolved dependency means null/);
  assert.match(FACT_DEPENDENCY_INSTRUCTIONS,/There is no model\/trim\/attribute whitelist/);
  const metadata=singlePassMetadata(prepareAssistantRequest(request()),out);
  assert.match(metadata.instructionSha256,/^[a-f0-9]{64}$/);
});

test('private, media and owner facts stay typed holds with no public query',()=>{
  for(const kind of ['private_company_fact','missing_media','owner_decision']){
    const value=candidate();value.decisions[0].factDependency={kind,claimScope:'Missing exact company/context evidence',publicQuery:null};
    const out=admission(value);assert.equal(out.admitted.factDependencies[0].kind,kind);assert.equal(out.admitted.factDependencies[0].publicQuery,null);
    value.decisions[0].factDependency.publicQuery='invent private lookup';assert.throws(()=>admission(value),{code:'ASSISTANT_INVALID_RESPONSE'});
  }
});

test('no prose/tag guessing, malformed and executable declarations fail instead of becoming work',()=>{
  const noTyped=candidate();noTyped.decisions[0].factDependency=null;
  assert.deepEqual(admission(noTyped).admitted.factDependencies,[]);
  for(const mutate of [v=>v.decisions[0].factDependency.publicQuery=null,v=>v.decisions[0].factDependency.kind='needs_fact',
    v=>v.decisions[0].factDependency.publicQuery='x\nprivate',v=>v.decisions[0].factDependency.extra='bad',
    v=>v.decisions[1].factDependency=v.decisions[0].factDependency]){
    const value=candidate();mutate(value);assert.throws(()=>admission(value),{code:'ASSISTANT_INVALID_RESPONSE'});
  }
  const req=request();req.factDependencyContract='unknown';assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('targeted research remains query-only and pins quality contract to actual isolated input',()=>{
  const req={account:'BAW Russia',query:'public subject technical specification',factResearchContract:'scoped_source_v1',
    privateCase:{phone:'never passed'},comment:'never passed',claimScope:'never passed',itemIds:['never passed']};
  const p=preparePublicResearchRequest(req);
  assert.equal(assistantLaneForRequest(p),'interactive');
  assert.equal(assistantLaneForRequest(prepareAssistantRequest(request())),'preparation');
  assert.deepEqual(JSON.parse(p.input),{query:req.query,factResearchContract:'scoped_source_v1'});
  assert.equal(p.factResearch,true);
  assert.equal(createHash('sha256').update(p.input).digest('hex'),createHash('sha256').update(JSON.stringify({query:req.query,factResearchContract:'scoped_source_v1'})).digest('hex'));
  assert.throws(()=>preparePublicResearchRequest({...req,factResearchContract:'unknown'}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('quality scoped sources require literal opened URL and preserve extraction limits',()=>{
  const raw={text:'Scoped finding',sources:[source()]},observed={calls:1,openedUrls:[source().url]};
  assert.throws(()=>admitPublicResearchResult(raw,{calls:0,openedUrls:[]},true),{code:'ASSISTANT_INVALID_RESEARCH'});
  const admitted=admitPublicResearchResult(raw,observed,true);
  assert.equal(admitted.sources[0].claimKind,'source_statement');assert.equal(admitted.sources[0].trust,'source_only');
  for(const quality of [{extraction:{status:'access_challenge'}},{claimKind:'product_specification',scope:{model:'Exact',trim:'A',market:'CN',modelYear:'2026'},sourceScope:{model:'Exact',trim:'A',market:'JP',modelYear:'2026'}}]){
    const value={text:'Limit retained',sources:[{...source(),...quality}]};const held=admitPublicResearchResult(value,observed,true,'baw-russia');
    assert.deepEqual(held.sources,[]);assert.equal(held.evidenceHolds[0].accountKey,'baw-russia');
    if(quality.extraction)assert.deepEqual(held.evidenceHolds[0].renderedFallback,{status:'access_challenge',attempts:0,capability:'web.run_text_only'});
  }
  assert.ok(!Object.hasOwn(admitPublicResearchResult(raw,observed).sources[0],'claimKind'),'existing public chat projection remains unchanged');
});

test('deterministic missing media includes an empty opt-in dependency envelope without inventing a query',()=>{
  const req={...request(),items:[{id:'public',attachmentsState:'present',attachments:[{type:'unsupported'}]}]};
  const held=deterministicMediaHold(prepareAssistantRequest(req));assert.deepEqual(held.factDependencies,[]);assert.equal(held.runMetadata,undefined);
  const legacy={...req};delete legacy.factDependencyContract;
  assert.ok(!Object.hasOwn(deterministicMediaHold(prepareAssistantRequest(legacy)),'factDependencies'));
});

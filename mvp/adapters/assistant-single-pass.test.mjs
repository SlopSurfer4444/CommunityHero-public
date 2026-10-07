import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,outputSchema,assistantCliArgs,admitAssistantEvents,
  admitSinglePassResult,singlePassMetadata,singlePassInstructions,generationMetadata,limitedReviewInstructions as reviewInstructions,editorialInstructions,UNCAPPED_EVIDENCE_CONTRACT} from './assistant.mjs';

const sha=value=>createHash('sha256').update(value).digest('hex');
const request=(ids=['a','b','c'])=>({purpose:'triage',preparationMode:'single_pass_v1',items:ids.map(id=>({id,text:'Supplied context'}))});
const prepared=()=>prepareAssistantRequest(request());
const pass={companyRules:'pass',intent:'pass',factualScope:'pass'};
const trace={calls:0,openedUrls:[],completedActivity:[]};
function candidate(ids=['a','b','c']){
  const proposals=ids.map(itemId=>({itemId,kind:'reply_and_close',text:`Supported reply ${itemId}`}));
  return {text:'Prepared',sources:[],proposals,assessments:ids.map(itemId=>({itemId,outcome:'reply',reason:'Context supports the reply',tags:[]})),
    evidence:[],generationEditorial:proposals.map(row=>({...row,decision:'accept',reason:'Exact final reply checked',checks:{...pass}})),
    decisionEvidence:ids.map(itemId=>({itemId,basis:'context',evidenceIndices:[],dependsOnItemIds:[]}))};
}
function web(value,itemId='a',url='https://example.com/exact'){
  const index=value.evidence.push({itemId,url,title:'Source',claim:'Narrow attributed claim'})-1;
  Object.assign(value.decisionEvidence.find(row=>row.itemId===itemId),{basis:'web',evidenceIndices:[index]});
}

test('single-pass mode is explicit, digest-bound and cannot fabricate an earlier pass',()=>{
  const req=request(),result=prepareAssistantRequest(req);
  assert.equal(result.singlePass,true);assert.equal(result.review,false);assert.equal(result.triage,true);
  assert.equal(JSON.parse(result.input).preparationMode,'single_pass_v1');assert.equal(result.payload.firstPass,undefined);
  const legacy=structuredClone(req);delete legacy.preparationMode;
  assert.notEqual(sha(result.input),sha(prepareAssistantRequest(legacy).input));
  for(const mutation of [r=>r.purpose='discussion',r=>r.purpose='triage_review',r=>r.purpose='editorial_review',
    r=>r.preparationMode='single_pass_v2',r=>r.firstPass=candidate()]){
    const invalid=request();mutation(invalid);assert.throws(()=>prepareAssistantRequest(invalid),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

test('single-pass wire is high Sol 6.1 with only review public tools and strict editorial/recipient schemas',()=>{
  const p=prepared(),args=assistantCliArgs('synthetic-home',false,[],true);
  assert.ok(args.includes('gpt-6.1-sol'));assert.ok(args.includes('model_reasoning_effort="high"'));
  assert.ok(args.includes('web_search="live"'));assert.ok(args.includes('standalone_web_search'));
  for(const name of ['shell_tool','unified_exec','apps','plugins','multi_agent','computer_use','browser_use','code_mode'])
    assert.ok(args.some((entry,index)=>entry==='--disable'&&args[index+1]===name));
  const schema=outputSchema(p.ids,true,false,false,undefined,true);
  for(const field of ['assessments','generationEditorial','evidence','decisionEvidence'])assert.ok(schema.required.includes(field));
  assert.equal(schema.properties.decisionEvidence.items.additionalProperties,false);
  assert.deepEqual(schema.properties.decisionEvidence.items.properties.itemId.enum,['a','b','c']);
  assert.throws(()=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{type:'command_execution',id:'x'}}),true),{code:'ASSISTANT_ISOLATION_FAILED'});
  const opened=admitAssistantEvents(JSON.stringify({type:'item.completed',item:{type:'web_search',id:'x',action:{type:'open_page',url:'https://example.com/exact'}}}),true);
  assert.deepEqual(opened.openedUrls,['https://example.com/exact']);
});

test('single-pass has no numerical web-call cap while legacy review remains capped and tools stay isolated',()=>{
  const events=Array.from({length:40},(_,index)=>JSON.stringify({type:'item.completed',item:{type:'web_search',id:`web-${index}`,
    action:{type:'open_page',url:`https://example.com/source-${index}`}}})).join('\n');
  const observed=admitAssistantEvents(events,true,8,true);
  assert.equal(observed.calls,40);assert.equal(observed.openedUrls.length,40);assert.equal(observed.webCallLimit,null);
  assert.throws(()=>admitAssistantEvents(events,true),{code:'ASSISTANT_RESEARCH_LIMIT'});
  assert.throws(()=>admitAssistantEvents(events+'\n'+JSON.stringify({type:'item.completed',item:{type:'command_execution',id:'foreign'}}),true,8,true),
    {code:'ASSISTANT_ISOLATION_FAILED'});
  const value=candidate();web(value,'a','https://example.com/source-39');
  const admission=admitSinglePassResult(value,prepared(),observed);
  assert.equal(admission.admitted.proposals.length,3,'An exact URL opened beyond the old cap remains observed');
  const metadata=singlePassMetadata(prepared(),admission);
  assert.equal(metadata.research.webCalls,40);assert.equal(metadata.research.webCallLimit,null);
  assert.doesNotMatch(singlePassInstructions(),/eight-call|eight web tool|at most 3 focused queries/);
  assert.match(singlePassInstructions(),/no numerical web-call or search-query cap/);
  assert.match(reviewInstructions(),/At most eight web tool calls/);
});

test('uncapped research keeps bounded rejected source diagnostics without losing unrelated recipients',()=>{
  const value=candidate();web(value,'a');
  value.evidence.push({itemId:'a',url:'https://example.com/unopened',title:'Unopened',claim:'Unsupported claim'});
  value.decisionEvidence[0].evidenceIndices.push(1);
  const result=admitSinglePassResult(value,prepared(),{...trace,calls:40,openedUrls:['https://example.com/exact']});
  assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['b','c']);
  assert.equal(result.rejectedSources.webCallsUsed,40);assert.equal(result.rejectedSources.webCallsLimit,null);
  assert.equal(result.rejectedSources.sources.length,1,'Already observed source is not mislabeled as rejected');
  assert.equal(result.rejectedSources.sources[0].candidateUrlSha256,sha('https://example.com/unopened'));
  assert.equal(result.rejectedSources.sourcesTruncated,false);assert.equal(result.rejectedSources.openedUrlsTruncated,false);
});

test('dense hundred-item tagged single-pass preserves all sources and absent contracts retain V75 bounds',()=>{
  const ids=Array.from({length:100},(_,n)=>`item-${n}`),legacy=prepareAssistantRequest(request(ids)),
    p=prepareAssistantRequest({...request(ids),researchLimitContract:UNCAPPED_EVIDENCE_CONTRACT}),value=candidate(ids);
  for(const [index,row] of value.decisionEvidence.entries()){
    row.basis='web';
    for(let n=0;n<3;n++){row.evidenceIndices.push(value.evidence.length);value.evidence.push({itemId:row.itemId,
      url:`https://example.com/source-${index}-${n}`,title:'Exact source',claim:'Attributed narrow claim'});}
  }
  const observed={calls:300,openedUrls:value.evidence.map(row=>row.url),completedActivity:[],webCallLimit:null};
  const result=admitSinglePassResult(value,p,observed);
  assert.equal(result.admitted.proposals.length,100);assert.equal(singlePassMetadata(p,result).research.sources.length,300);
  assert.equal(outputSchema(p.ids,true,false,false,undefined,true,p.uncappedEvidence).properties.evidence.maxItems,undefined);
  assert.equal(outputSchema(p.ids,true,true).properties.evidence.maxItems,undefined);
  assert.equal(outputSchema(p.ids,true,false,false,undefined,true,p.uncappedEvidence).properties.decisionEvidence.items.properties.evidenceIndices.items.maximum,undefined);
  assert.equal(outputSchema(legacy.ids,true,false,false,undefined,true).properties.evidence.maxItems,300);
  assert.equal(outputSchema(legacy.ids,true,false,false,undefined,true).properties.decisionEvidence.items.properties.evidenceIndices.items.maximum,299);
  assert.equal(outputSchema(legacy.ids,true,true,false,undefined,false,false).properties.evidence.maxItems,30);
  assert.equal(admitSinglePassResult(value,legacy,observed).admitted.proposals.length,100);
  assert.equal(singlePassMetadata(legacy,admitSinglePassResult(value,legacy,observed)).researchLimitContract,undefined);
  const excess=structuredClone(value);excess.evidence.push({...excess.evidence[0]});
  assert.throws(()=>admitSinglePassResult(excess,p,observed),{code:'ASSISTANT_INVALID_RESEARCH'});
  const held=admitSinglePassResult(value,p,{...observed,openedUrls:Array.from({length:40},(_,n)=>`https://other.example/open-${n}`)});
  assert.equal(held.admitted.proposals.length,0);assert.equal(held.rejectedSources.sources.length,30);
  assert.equal(held.rejectedSources.sourcesTruncated,true);assert.equal(held.rejectedSources.openedUrlsTruncated,true);
  assert.equal(held.rejectedSources.openedUrlSha256.length,8);
});

test('context-only results retain exact editorial hashes and actual high research metadata',()=>{
  const p=prepared(),value=candidate(),admission=admitSinglePassResult(value,p,trace),metadata=singlePassMetadata(p,admission,14.2);
  assert.equal(admission.admitted.proposals.length,3);assert.equal(admission.admitted.decisionEvidence,undefined);
  assert.equal(metadata.model,'gpt-6.1-sol');assert.equal(metadata.reasoningEffort,'high');assert.equal(metadata.research.reasoningEffort,'high');
  assert.equal(metadata.promptVersion,'communityhero-preparation-v1-single-pass');
  assert.equal(metadata.instructionSha256,sha(singlePassInstructions()));assert.equal(metadata.inputSha256,sha(p.input));
  assert.equal(metadata.research.status,'no_sources');assert.equal(metadata.research.webCalls,0);
  assert.equal(metadata.editorialEvidence.contract,'communityhero-editorial-v1');
  assert.equal(metadata.editorialEvidence.entries[0].textSha256,sha(value.proposals[0].text));
  assert.doesNotMatch(singlePassInstructions(),/firstPass|second-pass|full_audio_only with ownerAuthorizedAudioOnly=true/);
  assert.match(singlePassInstructions(),/default_full_audio_text/);
});

test('known supplied narrow facts need no new web call and keep cached provenance in bound input',()=>{
  const req={...request(['a']),materials:[{id:'fact',account:'LikeAvto',kind:'research',trust:'source_only',
    text:'Manufacturer states this for the named trim.',sourceUrl:'https://example.com/exact',sourceItemId:'a',
    claimKind:'product_specification',scope:{model:'Model Q',trim:'Named trim',market:'CN',modelYear:'2026'},
    fetchedAt:'2026-09-28T12:00:00Z',extraction:{status:'complete'}}]};
  const p=prepareAssistantRequest(req),result=admitSinglePassResult(candidate(['a']),p,trace);
  assert.equal(result.admitted.proposals.length,1);assert.equal(singlePassMetadata(p,result).research.webCalls,0);
  assert.equal(p.payload.materials[0].sourceItemId,'a');assert.equal(p.payload.materials[0].scope.trim,'Named trim');
  assert.equal(p.payload.materials[0].extraction.status,'complete');
});

test('editorial prompts check relevance without leaking internal caution or forcing unsupported answers',()=>{
  for(const account of ['likeavto','baw-russia']){
    for(const prompt of [singlePassInstructions(account),reviewInstructions(account),editorialInstructions(account)]){
      assert.match(prompt,/Tie each public sentence to the recipient's actual question or contribution/);
      assert.match(prompt,/do not add a comparison or recap merely to demonstrate context use/);
      assert.match(prompt,/operator reason, not the public reply/);
      assert.match(prompt,/Preserve a factual limitation or uncertainty/);
      assert.match(prompt,/Judge the whole exact reply, including its qualifiers and ending/);
      assert.match(prompt,/Keep a correction or limitation when/);
      assert.match(prompt,/distracting clause while preserving useful supported content/);
      assert.match(prompt,/a targeted clarification with a brief practical reason is valid/);
      assert.match(prompt,/retain the unresolved hold/);
    }
    const prompt=singlePassInstructions(account);
    assert.match(prompt,/Revise irrelevant detours and internal-process commentary within this same generation/);
    assert.ok(prompt.indexOf('Revise irrelevant detours')<prompt.indexOf('Return generationEditorial'));
    assert.match(prompt,/Repeat exact final text byte-for-byte/);
    assert.match(prompt,/If any required check is fail or uncertain, hold/);
    assert.doesNotMatch(prompt,/firstPass|second-pass/);
  }
});

test('single-pass strict schema remains valid for an empty selected set',()=>{
  const schema=outputSchema(new Set(),true,false,false,undefined,true);
  assert.equal(schema.properties.decisionEvidence.maxItems,0);
  const visit=value=>{if(!value||typeof value!=='object')return;if(value.enum)assert.ok(value.enum.length);for(const child of Object.values(value))visit(child);};
  visit(schema);
  const p=prepareAssistantRequest(request([])),result=admitSinglePassResult(candidate([]),p,trace);
  assert.deepEqual(result.editorialEvidence.entries,[]);
});

test('unobserved web source holds only affected recipient and transitive dependencies without a repair call',()=>{
  const value=candidate();web(value);
  value.decisionEvidence[1].dependsOnItemIds=['a'];
  const result=admitSinglePassResult(value,prepared(),{...trace,calls:1,openedUrls:['https://example.com/exact/']});
  assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['c']);
  assert.deepEqual(result.editorialEvidence.entries.map(row=>row.itemId),['c']);
  assert.equal(result.admitted.assessments[0].outcome,'needs_attention');assert.equal(result.admitted.assessments[1].outcome,'needs_attention');
  assert.equal(result.rejectedSources.reason,'still_unobserved');assert.equal(result.evidence.length,0);
  assert.equal(value.proposals.length,3);
});

test('exact observed sources preserve item-scoped provenance without treating URL activity as truth',()=>{
  const value=candidate();web(value);
  const result=admitSinglePassResult(value,prepared(),{...trace,calls:1,openedUrls:['https://example.com/exact']});
  assert.equal(result.admitted.proposals.length,3);assert.equal(result.evidence[0].itemId,'a');assert.equal(result.evidence[0].trust,'source_only');
  assert.equal(singlePassMetadata(prepared(),result).research.sources[0].claim,'Narrow attributed claim');
  value.evidence[0].extraction={status:'missing_table',observedAt:'2026-09-28T12:00:00Z'};
  const held=admitSinglePassResult(value,prepared(),{...trace,calls:1,openedUrls:['https://example.com/exact']});
  assert.deepEqual(held.admitted.proposals.map(row=>row.itemId),['b','c']);assert.equal(held.evidenceHolds[0].reason,'incomplete_extraction');
});

test('editorial revise or hold removes only its proposal, never silently accepting final failed checks',()=>{
  const value=candidate();value.generationEditorial[0].decision='revise';value.generationEditorial[0].checks.intent='fail';
  value.generationEditorial[1].decision='hold';value.generationEditorial[1].checks.factualScope='uncertain';
  const result=admitSinglePassResult(value,prepared(),trace);
  assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['c']);
  assert.ok(result.editorialEvidence.entries.every(row=>row.decision==='accept'));
});

test('invalid exact editorial proof rejects rather than attesting a different proposal',()=>{
  for(const mutate of [v=>v.generationEditorial.pop(),v=>v.generationEditorial[0].text+='changed',
    v=>v.generationEditorial[0].itemId='foreign',v=>v.generationEditorial[0].checks.intent='uncertain']){
    const value=candidate();mutate(value);assert.throws(()=>admitSinglePassResult(value,prepared(),trace),{code:'ASSISTANT_INVALID_RESPONSE'});
  }
});

test('foreign, duplicate, missing or cross-recipient dependency/source proof cannot be admitted',()=>{
  for(const mutate of [v=>v.decisionEvidence.pop(),v=>v.decisionEvidence[1].itemId='a',v=>v.decisionEvidence[0].itemId='foreign',
    v=>v.decisionEvidence[0].dependsOnItemIds=['foreign'],v=>v.decisionEvidence[0].dependsOnItemIds=['a'],
    v=>{web(v,'b');v.decisionEvidence[0].basis='web';v.decisionEvidence[0].evidenceIndices=[0];},
    v=>{web(v);v.decisionEvidence[0].basis='context';},v=>{web(v);v.evidence[0].itemId='foreign';}]){
    const value=candidate();mutate(value);assert.throws(()=>admitSinglePassResult(value,prepared(),trace),{code:'ASSISTANT_INVALID_RESEARCH'});
  }
});

test('missing web evidence and circular dependencies hold without discarding unrelated decisions',()=>{
  const value=candidate();value.decisionEvidence[0].basis='web';
  let result=admitSinglePassResult(value,prepared(),trace);assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['b','c']);
  value.decisionEvidence[0].basis='context';value.decisionEvidence[0].dependsOnItemIds=['b'];value.decisionEvidence[1].dependsOnItemIds=['a'];
  result=admitSinglePassResult(value,prepared(),trace);assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['c']);
});

test('image holds preserve original evidence indices and propagate to dependent decisions',()=>{
  const value=candidate();web(value,'a');web(value,'b','https://example.com/second');
  value.decisionEvidence[2].dependsOnItemIds=['a'];
  const result=admitSinglePassResult(value,prepared(),{...trace,calls:2,openedUrls:value.evidence.map(row=>row.url)}, {blockedItemIds:['a']});
  assert.deepEqual(result.admitted.proposals.map(row=>row.itemId),['b']);
  assert.equal(result.evidence[0].itemId,'b');assert.equal(result.admitted.assessments[0].tags[0],'missing_context');
});

test('validated dependency graph survives metadata for durable cross-group currentness checks',()=>{
  const p=prepared(),value=candidate();value.decisionEvidence[0].dependsOnItemIds=['b'];value.decisionEvidence[1].dependsOnItemIds=['c'];
  const admission=admitSinglePassResult(value,p,trace),metadata=singlePassMetadata(p,admission);
  assert.equal(admission.admitted.proposals.length,3,'Dependencies alone do not hold supported decisions');
  assert.deepEqual(metadata.decisionDependencies,{version:1,entries:[
    {itemId:'a',dependsOnItemIds:['b']},{itemId:'b',dependsOnItemIds:['c']},{itemId:'c',dependsOnItemIds:[]}]});
  value.decisionEvidence[0].dependsOnItemIds.push('c');
  assert.deepEqual(metadata.decisionDependencies.entries[0].dependsOnItemIds,['b'],'Validated map is detached from raw candidate');
  const held=admitSinglePassResult(value,p,trace,{blockedItemIds:['c']});
  assert.equal(held.admitted.proposals.length,0);
  assert.equal(singlePassMetadata(p,held).decisionDependencies.entries.length,3,'Held dependency rows remain available for closure validation');
  assert.equal(generationMetadata(p.input,true).decisionDependencies,undefined,'Legacy metadata contract unchanged');
});

test('all held decisions still return an empty exact editorial envelope',()=>{
  const value=candidate();for(const row of value.decisionEvidence)row.basis='unresolved';
  const result=admitSinglePassResult(value,prepared(),trace);
  assert.deepEqual(result.admitted.proposals,[]);assert.deepEqual(result.editorialEvidence,{version:1,contract:'communityhero-editorial-v1',entries:[]});
});

test('legacy triage, discussion and paid review keep their low/medium routes and output contracts',()=>{
  const p=prepareAssistantRequest({purpose:'triage',items:[{id:'a'}]});assert.equal(p.singlePass,false);
  assert.ok(assistantCliArgs('home').includes('model_reasoning_effort="low"'));
  assert.ok(assistantCliArgs('home',true).includes('model_reasoning_effort="medium"'));
  assert.ok(assistantCliArgs('home').includes('web_search="disabled"'));
  assert.equal(generationMetadata(p.input,true).reasoningEffort,'low');
  assert.equal(outputSchema(p.ids,true).properties.generationEditorial,undefined);
  assert.equal(outputSchema(p.ids,true,true).properties.decisionEvidence,undefined);
  assert.match(reviewInstructions(),/second-pass review of firstPass/);
  assert.throws(()=>prepareAssistantRequest({purpose:'triage_review',items:[{id:'a'}]}),{code:'ASSISTANT_INVALID_RESPONSE'});
});

test('preparation media policy remains exact company/source-bound while acquisition policy is preserved',()=>{
  const binding={id:'angryspace-likeavto-v1',workspaceId:'local-pilot',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'LikeAvto'};
  // Use the provider binding advertised by the account configuration.
  binding.providerAccountId=prepareAssistantRequest(request()).account.providerAccountId;
  const acquisition={version:1,mode:'full_audio_visual',fullAudioRequired:true,visualRequired:true,ownerAuthorizedAudioOnly:false,
    account:'LikeAvto',connectorBinding:binding,sourceVersion:'a'.repeat(64),policySha256:'b'.repeat(64)};
  const policy={...acquisition,purpose:'preparation',mode:'full_audio_only',visualRequired:false,decisionBasis:{kind:'default_full_audio_text'}};
  const req={...request(),connectorBinding:binding,posts:[{id:'post',mediaPolicy:acquisition,preparationMediaPolicy:policy,visualContextStatus:'missing'}]};
  const p=prepareAssistantRequest(req);assert.equal(p.payload.posts[0].mediaPolicy.visualRequired,true);
  assert.deepEqual(p.payload.posts[0].preparationMediaPolicy,policy);assert.match(p.input,/default_full_audio_text/);
  for(const mutate of [r=>r.posts[0].preparationMediaPolicy.account='BAW Russia',r=>r.posts[0].preparationMediaPolicy.sourceVersion='c'.repeat(64),
    r=>r.posts[0].preparationMediaPolicy.ownerAuthorizedAudioOnly=true,r=>r.posts[0].preparationMediaPolicy.decisionBasis.kind='unknown',
    r=>r.posts[0].preparationMediaPolicy.connectorBinding={...binding,id:'foreign'}]){
    const invalid=structuredClone(req);mutate(invalid);assert.throws(()=>prepareAssistantRequest(invalid),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_POLICY'});
  }
});

test('default preparation audio policy admits only exact owner-confirmed full-audio equivalence',()=>{
  const digest=letter=>letter.repeat(64),binding={id:'angryspace-baw-russia-v1',workspaceId:'local-pilot',
    accountId:'BAW Russia',connector:'angryspace',revision:1,providerAccountId:'baw-russia'};
  const acquisition={version:1,mode:'full_audio_visual',fullAudioRequired:true,visualRequired:true,ownerAuthorizedAudioOnly:false,
    account:'BAW Russia',connectorBinding:binding,sourceVersion:digest('a'),policySha256:digest('b')};
  const policy={...acquisition,purpose:'preparation',mode:'full_audio_only',visualRequired:false,decisionBasis:{kind:'default_full_audio_text'}};
  const edge={match:'owner_confirmed_audio_equivalence',authorization:'owner_confirmed_same_video',
    equivalenceSha256:digest('e'),equivalenceRevision:4,targetPostId:'target',postKey:'12182:target',
    targetSourceVersion:digest('a'),sourcePostId:'source',sourcePostKey:'12185:source',sourceVersion:digest('c'),
    account:'BAW Russia',connectorBinding:binding,transcript:{entryId:'entry',versionId:'version',hash:digest('d')},
    identities:[],byteEqualityClaimed:false};
  const req={...request(['a']),account:'baw-russia',connectorBinding:binding,
    items:[{id:'a',postId:'target',postKey:'12182:target'}],
    posts:[{id:'target',postKey:'12182:target',mediaPolicy:acquisition,preparationMediaPolicy:policy,visualContextStatus:'missing'}],
    materials:[{id:'audio',account:'BAW Russia',postKey:'12185:source',kind:'transcript',trust:'source_only',
      text:'Complete source transcript',knowledgeEntryId:'entry',knowledgeVersionId:'version',
      transcription:{sourceVersion:digest('c'),partial:false,coverage:'full_audio',mediaDurationSeconds:1200,audioDurationSeconds:1200},
      audioEquivalence:[structuredClone(edge)]}],
    knowledgeManifest:[{entryId:'entry',versionId:'version',hash:digest('d'),kind:'transcript',trust:'source_only',mediaBinding:[structuredClone(edge)]}]};
  const p=prepareAssistantRequest(req);
  assert.deepEqual(p.payload.materials[0].audioEquivalence,[edge]);
  assert.deepEqual(p.payload.knowledgeManifest[0].mediaBinding,[edge]);
  assert.equal(p.payload.posts[0].mediaPolicy.mode,'full_audio_visual');
  const legacy=structuredClone(req);delete legacy.preparationMode;
  assert.throws(()=>prepareAssistantRequest(legacy),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'AUDIO_EQUIVALENCE'});
  for(const mutate of [r=>r.materials[0].transcription.partial=true,r=>r.materials[0].transcription.sourceVersion=digest('f'),
    r=>r.materials[0].transcription.audioDurationSeconds=1190,
    r=>{delete r.posts[0].preparationMediaPolicy;},
    r=>{r.posts[0].preparationMediaPolicy={...policy,mode:'full_audio_visual',visualRequired:true,decisionBasis:{kind:'exact_owner_override'}};},
    ...['authorization','targetSourceVersion','sourceVersion','account','equivalenceSha256'].map(key=>r=>{
      for(const e of [r.materials[0].audioEquivalence[0],r.knowledgeManifest[0].mediaBinding[0]])
        e[key]=key==='authorization'?'model_inferred_same_video':key==='account'?'LikeAvto':key==='equivalenceSha256'?'not-a-hash':digest('f');
    }),
    r=>{r.knowledgeManifest[0].mediaBinding[0].equivalenceRevision=5;}]){
    const invalid=structuredClone(req);mutate(invalid);
    assert.throws(()=>prepareAssistantRequest(invalid),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'AUDIO_EQUIVALENCE'});
  }
});

const moderationBinding={id:'angryspace-likeavto-v1',workspaceId:'local-pilot',accountId:'LikeAvto',connector:'angryspace',revision:1,providerAccountId:'likeavto'};
const ruleRef={entryId:'rule-entry',versionId:'rule-version',hash:'a'.repeat(64)};
function moderationRequest(){const req=request(['a','b']);req.connectorBinding=moderationBinding;
  req.items.forEach(item=>Object.assign(item,{postKey:'post-a',moderationCapabilities:{delete:'supported',hide:'unsupported'}}));
  req.moderationContext={version:1,account:'LikeAvto',connectorBinding:moderationBinding,ruleRefs:[ruleRef]};
  req.knowledgeManifest=[{...ruleRef,kind:'rule',scope:{account:'LikeAvto',postKeys:[]}}];
  req.materials=[{id:'rule',kind:'rule',text:'Delete targeted insults; preserve substantive criticism.',knowledgeEntryId:ruleRef.entryId,knowledgeVersionId:ruleRef.versionId}];return req;}
function moderationCandidate(){const value=candidate(['a','b']);Object.assign(value.proposals[0],{kind:'delete',text:''});
  value.assessments[0].outcome='delete';Object.assign(value.generationEditorial[0],{kind:'delete',text:''});
  value.moderationEvidence=[{itemId:'a',kind:'delete',ruleRefs:[ruleRef]}];return value;}
test('moderation only accepts exact supported action, current company rule and final editorial proof',()=>{
  const req=moderationRequest(),p=prepareAssistantRequest(req),result=admitSinglePassResult(moderationCandidate(),p,trace);
  assert.equal(result.admitted.proposals[0].kind,'delete');assert.equal(result.admitted.proposals[0].text,'');
  assert.deepEqual(result.admitted.moderationEvidence.entries,[{itemId:'a',kind:'delete',ruleRefs:[ruleRef]}]);
  assert.equal(result.editorialEvidence.entries[0].textSha256,sha(''));
  const legacy={...req};delete legacy.preparationMode;
  assert.equal(prepareAssistantRequest(legacy).payload.moderationContext,undefined);
  assert.ok(!outputSchema(p.ids,true).properties.proposals.items.properties.kind.enum.includes('delete'));
  for(const mutate of [r=>r.items[0].moderationCapabilities.delete='unknown',r=>r.items[0].moderationCapabilities.delete='unsupported',
    r=>r.knowledgeManifest[0].scope.postKeys=['other-post'],r=>r.moderationContext.ruleRefs=[]]){
    const changed=structuredClone(req);mutate(changed);const held=admitSinglePassResult(moderationCandidate(),prepareAssistantRequest(changed),trace);
    assert.deepEqual(held.admitted.proposals.map(row=>row.itemId),['b']);assert.equal(held.admitted.assessments[0].outcome,'needs_attention');
    assert.deepEqual(held.admitted.moderationEvidence.entries,[]);
  }
});
test('moderation foreign/company/stale proof fails closed without widening legacy output',()=>{
  const req=moderationRequest();
  for(const mutate of [r=>r.moderationContext.account='BAW Russia',r=>r.knowledgeManifest[0].scope.account='BAW Russia',
    r=>r.moderationContext.connectorBinding={...moderationBinding,revision:2},r=>r.materials[0].knowledgeVersionId='stale']){
    const changed=structuredClone(req);mutate(changed);assert.throws(()=>prepareAssistantRequest(changed),{code:'ASSISTANT_INVALID_REQUEST'});
  }
  const p=prepareAssistantRequest(req);
  const foreign=moderationCandidate();foreign.moderationEvidence[0].itemId='foreign';assert.throws(()=>admitSinglePassResult(foreign,p,trace));
  const stale=moderationCandidate();stale.moderationEvidence[0].ruleRefs=[{...ruleRef,hash:'b'.repeat(64)}];
  assert.deepEqual(admitSinglePassResult(stale,p,trace).admitted.proposals.map(row=>row.itemId),['b']);
  const editorial=moderationCandidate();editorial.generationEditorial[0].decision='hold';editorial.generationEditorial[0].checks.companyRules='uncertain';
  assert.deepEqual(admitSinglePassResult(editorial,p,trace).admitted.proposals.map(row=>row.itemId),['b']);
});

test('single-pass revalidation preserves moderation history without authorizing reuse',()=>{
  for(const outcome of ['hide','delete']){
    const req=request(['a']);req.previousDecision={itemId:'a',outcome,text:'',reason:'Historic proposal'};
    assert.equal(prepareAssistantRequest(req).payload.previousDecision.outcome,outcome);
    const legacy=structuredClone(req);delete legacy.preparationMode;assert.throws(()=>prepareAssistantRequest(legacy),{code:'ASSISTANT_INVALID_REQUEST'});
    req.previousDecision.text='must never become public text';assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});
  }
});

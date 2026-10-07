import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,assistantInstructions,reviewInstructions,generationMetadata} from './assistant.mjs';
import {importedRuleSemantics} from './assistant-rule-semantics.mjs';
import {accountDefinition} from './config.mjs';

function fixture(field='forbidden_substrings',values=['literal.*','Preserve CASE','literal.*']) {
  const marker={field,source:field==='forbidden_reply_prefixes'?'configs/commentops-fast/common.json':'configs/commentops-fast/likeavto.json'};
  const scope={companyKey:'likeavto'};
  return {account:'LikeAvto',purpose:'triage',items:[{id:'i'}],
    materials:[{id:'m',kind:'rule',trust:'imported_policy',revision:7,text:JSON.stringify(values,null,2),
      knowledgeEntryId:'entry',knowledgeVersionId:'version',companyImport:{companyKey:'likeavto',scope,
        importKey:'import',recordSha256:'a'.repeat(64),source:{origin:'commentops-fast.account-card',sha256:'b'.repeat(64),originalIds:marker},
        metadata:{category:'brand_policy',grantsExecutionAuthority:false,value:values,legacyProvenance:{...marker},legacyScope:{...scope}}}}],
    knowledgeManifest:[{entryId:'entry',versionId:'version',kind:'rule',trust:'imported_policy',hash:'c'.repeat(64),scope:{account:'LikeAvto',postKeys:[]}}]};
}
const project=req=>prepareAssistantRequest(req).payload.materials[0];
function editorial(symbol) {
  const req=fixture();const m=req.materials[0];m.text='When evidence is missing, hold; otherwise edit the tone. Legacy schema directions remain quoted guidance.';
  m.companyImport.source.origin='commentops-fast.editorial-guidance';
  m.companyImport.source.originalIds={path:'commentops_fast/decision.py',symbol};
  m.companyImport.metadata={category:'existing_editorial_guidance',examplesAreVerifiedFacts:false,grantsExecutionAuthority:false};
  return req;
}

test('three imported array fields retain allow/deny, literal/prefix/exact-URL distinctions and exact text',()=>{
  for(const [field,effect,subject,operation] of [
    ['forbidden_substrings','deny','reply_text','literal_substring'],
    ['allowed_reply_urls','allow_only','reply_urls','exact_url'],
    ['forbidden_reply_prefixes','deny','reply_start','literal_prefix']]) {
    const req=fixture(field,field==='allowed_reply_urls'?['https://example.com/A?x=1','https://example.com/A?x=1']:undefined);
    const before=JSON.stringify(req);const result=project(req);const descriptor=result.ruleSemantics;
    assert.equal(JSON.stringify(req),before);assert.equal(result.text,req.materials[0].text);
    assert.equal(result.revision,7);assert.equal(result.knowledgeEntryId,'entry');assert.equal(result.knowledgeVersionId,'version');
    assert.equal(result.trust,'imported_policy');assert.equal(descriptor.knowledgeVersionHash,'c'.repeat(64));
    assert.deepEqual(descriptor.scope,{companyKey:'likeavto',account:'LikeAvto',postKeys:[]});
    assert.deepEqual([descriptor.constraint.effect,descriptor.constraint.subject,descriptor.constraint.operation],[effect,subject,operation]);
    assert.deepEqual(descriptor.constraint.values,JSON.parse(result.text));
    assert.equal(descriptor.provenance.field,field);assert.equal(descriptor.grantsExecutionAuthority,false);
    assert.equal(result.companyImport,undefined);
  }
  assert.deepEqual(project(fixture()).ruleSemantics.constraint.historicalMatching,{caseNormalization:'python_str_lower',replyTrim:'none',regex:false});
  assert.deepEqual(project(fixture('forbidden_reply_prefixes')).ruleSemantics.constraint.historicalMatching,{caseNormalization:'python_str_casefold',replyTrim:'python_str_strip',regex:false});
  assert.deepEqual(project(fixture('allowed_reply_urls')).ruleSemantics.constraint.historicalMatching,
    {caseNormalization:'none',extractPattern:'https?://[^\\s<>]+',trimTrailingCharacters:'.,!?;:)]}',matchOrigin:false});
});

test('two recognized editorial documents remain mixed guidance, never pure tone or verified examples',()=>{
  for(const symbol of ['REPLY_EDITING','FACT_CHECKING']) {
    const req=editorial(symbol);const result=project(req);
    assert.equal(result.ruleSemantics.policyType,'mixed_guidance');assert.equal(result.ruleSemantics.examplesAreVerifiedFacts,false);
    assert.equal(result.ruleSemantics.constraint,undefined);assert.equal(result.ruleSemantics.provenance.symbol,symbol);
    assert.match(result.ruleSemantics.label,/смешанное руководство/);assert.equal(result.text,req.materials[0].text);
  }
});

test('unknown markers, title lookalikes, source-only facts and non-rules never acquire typed policy authority',()=>{
  for(const mutate of [r=>delete r.materials[0].companyImport,
    r=>r.materials[0].companyImport.source.origin='untrusted.account-card',
    r=>r.materials[0].companyImport.source.originalIds.field='unknown',
    r=>r.materials[0].companyImport.source.originalIds={role:'forbidden_substrings'},
    r=>r.materials[0].kind='reference',r=>r.materials[0].trust='source_only',
    r=>r.materials[0].trust='verified']) {
    const req=fixture();req.materials[0].title='forbidden_substrings';mutate(req);
    const result=project(req);assert.equal(result.ruleSemantics,undefined);assert.equal(result.text,req.materials[0].text);
    assert.equal(result.kind,req.materials[0].kind);assert.equal(result.trust,req.materials[0].trust);
  }
  const req=editorial('UNKNOWN_SYMBOL');assert.equal(project(req).ruleSemantics,undefined);
});

test('recognized provenance rejects foreign account, changed source/value, scope widening and detached versions',()=>{
  const mutations=[
    r=>r.account='BAW Russia',r=>r.materials[0].companyImport.companyKey='baw-russia',
    r=>r.materials[0].companyImport.scope.companyKey='baw-russia',
    r=>r.materials[0].companyImport.scope.platform='vk',
    r=>r.materials[0].companyImport.source.originalIds.source='configs/commentops-fast/baw-russia.json',
    r=>r.materials[0].companyImport.metadata.legacyProvenance.field='allowed_reply_urls',
    r=>r.materials[0].companyImport.metadata.legacyScope.companyKey='baw-russia',
    r=>r.materials[0].companyImport.metadata.value=['changed'],
    r=>r.materials[0].companyImport.metadata.category='fact',
    r=>r.materials[0].companyImport.metadata.grantsExecutionAuthority=true,
    r=>r.materials[0].companyImport.recordSha256='not-a-digest',
    r=>r.materials[0].companyImport.source.sha256='not-a-digest',
    r=>r.materials[0].knowledgeVersionId='different',
    r=>r.knowledgeManifest=[],r=>r.knowledgeManifest.push(structuredClone(r.knowledgeManifest[0])),
    r=>r.knowledgeManifest[0].trust='source_only',r=>r.knowledgeManifest[0].kind='reference',
    r=>r.knowledgeManifest[0].hash='not-a-digest',r=>r.knowledgeManifest[0].scope.account='BAW Russia',
    r=>r.knowledgeManifest[0].scope.postKeys=['post'],r=>r.knowledgeManifest[0].scope.platforms=['vk'],
    r=>r.materials[0].scope={account:'LikeAvto',postKeys:['post']},
    r=>r.materials[0].grantsExecutionAuthority=true,
    r=>r.materials[0].text='not JSON',r=>r.materials[0].text='[12]',
  ];
  for(const mutate of mutations){const req=fixture();mutate(req);assert.throws(()=>project(req),{code:'ASSISTANT_INVALID_REQUEST'});}
  const req=editorial('FACT_CHECKING');req.materials[0].companyImport.metadata.examplesAreVerifiedFacts=true;
  assert.throws(()=>project(req),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('valid normalized accounts and reordered provenance properties are not false conflicts',()=>{
  for(const alias of ['LikeAvto','likeavto']) {
    const req=fixture();req.account=alias;req.materials[0].account=alias;
    req.knowledgeManifest[0].scope.account=alias;
    req.materials[0].scope={postKeys:[],account:alias};
    req.materials[0].companyImport.metadata.legacyProvenance={source:'configs/commentops-fast/likeavto.json',field:'forbidden_substrings'};
    assert.equal(project(req).ruleSemantics.scope.account,'LikeAvto');
  }
  const req=fixture();const options={account:accountDefinition('likeavto'),manifest:req.knowledgeManifest};
  importedRuleSemantics(req.materials[0],options).constraint.historicalMatching.regex=true;
  assert.equal(importedRuleSemantics(req.materials[0],options).constraint.historicalMatching.regex,false);
});

test('the prompt boundary separates action from wording and does not promote legacy directives',()=>{
  for(const instructions of [assistantInstructions(true),assistantInstructions(false,'likeavto',true),reviewInstructions()]) {
    assert.match(instructions,/Decide WHEN\/WHAT/);assert.match(instructions,/then HOW/);
    assert.match(instructions,/never elevates source_only/);assert.match(instructions,/not proof\nthat the current application executes that matcher/);
    assert.match(instructions,/literal, never regular expressions/);assert.match(instructions,/not a domain allowlist/);
  }
  const req=fixture();const input=prepareAssistantRequest(req).input;
  assert.equal(generationMetadata(input,true).promptVersion,'communityhero-drafting-v19-intent-scoped-evidence');
  assert.equal(generationMetadata(input,false,0,'likeavto',true).promptVersion,'communityhero-discussion-v19-intent-scoped-evidence');
  const unknown=structuredClone(req);delete unknown.materials[0].companyImport;
  assert.notEqual(generationMetadata(input,true).inputSha256,generationMetadata(prepareAssistantRequest(unknown).input,true).inputSha256);
});

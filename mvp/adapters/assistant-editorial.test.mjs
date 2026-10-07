import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,validateEditorialResult,editorialOutputSchema,editorialInstructions,
  admitGenerationEditorial,retainGenerationEditorial,reviewInstructions,outputSchema,
  validateAssistantResult,assistantCliArgs,assistantLaneForRequest,admitAssistantEvents,
  admitReviewWithRepair} from './assistant.mjs';

const hash=text=>createHash('sha256').update(text).digest('hex');
const pass={companyRules:'pass',intent:'pass',factualScope:'pass'};
const candidate=(id='proposal-1',itemId='item-1',text='Спасибо за историю!')=>({proposalId:id,
  proposalRevision:3,itemId,kind:'reply_and_close',text,textSha256:hash(text),
  contextDigest:hash('exact branch'),rulesDigest:hash('company rule versions')});
const request=(candidates=[candidate()])=>({purpose:'editorial_review',account:'baw-russia',
  items:[...new Set(candidates.map(entry=>entry.itemId))].map(id=>({id,text:'История владельца'})),
  materials:[{id:'rule-1',account:'baw-russia',kind:'rule',trust:'verified',text:'Без выдуманных обещаний.'}],
  editorialCandidates:candidates});
const decision=(c,patch={})=>({proposalId:c.proposalId,proposalRevision:c.proposalRevision,itemId:c.itemId,
  textSha256:c.textSha256,contextDigest:c.contextDigest,rulesDigest:c.rulesDigest,decision:'accept',
  reason:'Благодарность отвечает личной истории и не добавляет фактов или обещаний.',proposedText:null,checks:{...pass},...patch});
const result=(candidates=[candidate()])=>({text:'Редакторская проверка завершена.',sources:[],proposals:[],editorial:candidates.map(c=>decision(c))});
const invalidRequest={code:'ASSISTANT_INVALID_REQUEST'};
const invalidResponse={code:'ASSISTANT_INVALID_RESPONSE',validationCategory:'EDITORIAL'};

test('dedicated request preserves exact text and bindings from multiple origins in one company',()=>{
  const list=[candidate(),candidate('manual-edit','item-2','Хочу V8 — понятное желание 🙂')];
  const prepared=prepareAssistantRequest(request(list));
  assert.equal(prepared.editorial,true);assert.equal(prepared.review,false);assert.equal(prepared.triage,false);
  assert.equal(prepared.payload.account.accountKey,'baw-russia');
  assert.deepEqual(prepared.payload.editorialCandidates,list);
  assert.equal(assistantLaneForRequest(prepared),'preparation');
  assert.deepEqual(validateEditorialResult(result(list),prepared).editorial,list.map(c=>decision(c)));
  assert.throws(()=>prepareAssistantRequest({...request(),materials:[{id:'foreign',account:'likeavto',text:'Foreign rules'}]}),invalidRequest);
});

test('candidate exact byte binding rejects stale hashes, detached targets and invalid batches',()=>{
  const c=candidate();
  for(const patch of [{text:c.text+' '},{textSha256:hash('another draft')},{itemId:'foreign'},
    {proposalRevision:0},{proposalRevision:1.5},{proposalId:''},{kind:'publish'},
    {contextDigest:''},{rulesDigest:'not-a-hash'},{text:' '.repeat(3)}]){
    const req=request();req.editorialCandidates=[{...c,...patch}];
    assert.throws(()=>prepareAssistantRequest(req),invalidRequest);
  }
  for(const editorialCandidates of [undefined,[],[c,c],Array.from({length:101},(_,i)=>candidate('p'+i))])
    assert.throws(()=>prepareAssistantRequest({...request(),editorialCandidates}),invalidRequest);
  assert.throws(()=>prepareAssistantRequest({...request(),purpose:'triage'}),invalidRequest);
  assert.throws(()=>prepareAssistantRequest({...request(),assistantTools:{}}),invalidRequest);
  assert.throws(()=>prepareAssistantRequest({...request(),currentTime:'2026-09-27T00:00:00Z'}),invalidRequest);
});

test('all-provenance action intent review accepts nonreply candidates but holds inappropriate actions',()=>{
  for(const kind of ['close','hide','delete']){
    const c={...candidate(),kind,text:'',textSha256:hash('')};
    const prepared=prepareAssistantRequest(request([c]));
    assert.equal(validateEditorialResult(result([c]),prepared).editorial[0].decision,'accept');
    const held={...result([c]),editorial:[decision(c,{decision:'hold',checks:{...pass,intent:'fail'},reason:'Удаление личной истории не обосновано правилами.'})]};
    assert.equal(validateEditorialResult(held,prepared).editorial[0].decision,'hold');
    assert.throws(()=>validateEditorialResult({...held,editorial:[{...held.editorial[0],decision:'revise',proposedText:'Спасибо!'}]},prepared),invalidResponse);
  }
});

test('semantic revision and hold contract fail closed without blanket holds on supported replies',()=>{
  const c=candidate(),prepared=prepareAssistantRequest(request());
  const revised=decision(c,{decision:'revise',checks:{...pass,intent:'fail'},
    reason:'Ценовое желание не требует лекции о стоимости; достаточно лёгкого ответа.',proposedText:'Было бы здорово 🙂'});
  assert.equal(validateEditorialResult({...result(),editorial:[revised]},prepared).editorial[0].proposedText,revised.proposedText);
  assert.equal(validateEditorialResult(result(),prepared).editorial[0].decision,'accept');
  for(const patch of [{decision:'accept',checks:{...pass,companyRules:'fail'}},
    {decision:'hold'},{decision:'revise',proposedText:'Another'},
    {decision:'hold',checks:{...pass,factualScope:'uncertain'},proposedText:'Another'},
    {decision:'revise',checks:{...pass,intent:'fail'},proposedText:c.text},
    {decision:'revise',checks:{...pass,intent:'fail'},proposedText:' '},
    {reason:' '},{checks:{...pass,intent:'okay'}},{checks:{...pass,extra:'pass'}}])
    assert.throws(()=>validateEditorialResult({...result(),editorial:[decision(c,patch)]},prepared),invalidResponse);
});

test('editorial output rejects missing, repeated, swapped or stale echoes and action leakage',()=>{
  const list=[candidate(),candidate('p2','item-2','Спасибо за приглашение!')],prepared=prepareAssistantRequest(request(list)),valid=result(list);
  for(const field of ['proposalId','proposalRevision','itemId','textSha256','contextDigest','rulesDigest']){
    const altered={...valid.editorial[0],[field]:valid.editorial[1][field]};
    if(altered[field]===valid.editorial[0][field])altered[field]=field==='proposalRevision'?4:hash('changed');
    assert.throws(()=>validateEditorialResult({...valid,editorial:[altered,valid.editorial[1]]},prepared),invalidResponse);
  }
  for(const editorial of [[],[valid.editorial[0]],[valid.editorial[0],valid.editorial[0]]])
    assert.throws(()=>validateEditorialResult({...valid,editorial},prepared),invalidResponse);
  for(const patch of [{proposals:[{itemId:'item-1',kind:'close',text:''}]},{sources:['invented']},
    {lookup:{kind:'search_comments',query:'Find'}},{toolCalls:[]}])
    assert.throws(()=>validateEditorialResult({...valid,...patch},prepared),invalidResponse);
});

test('editorial schema and prompts enforce checklist while CLI forbids all web calls',()=>{
  const prepared=prepareAssistantRequest(request()),schema=editorialOutputSchema(prepared);
  assert.deepEqual(schema.required,['text','sources','proposals','editorial']);
  assert.equal(schema.properties.proposals.maxItems,0);
  assert.equal(schema.properties.editorial.minItems,1);
  const instructions=editorialInstructions('baw-russia');
  for(const pattern of [/BAW Russia/,/personal story is not automatically an objection/,/price sarcasm/,/invitation/,
    /factual correction, limitation or escalation/,/not fixed character substitutions/,/checks.companyRules/,/contextDigest and rulesDigest exactly/])assert.match(instructions,pattern);
  const args=assistantCliArgs('C:/fixture',false);
  assert.ok(args.includes('web_search="disabled"'));assert.ok(!args.includes('standalone_web_search'));
  const webEvent=JSON.stringify({type:'item.completed',item:{type:'web_search',id:'web-1',action:{type:'search'},query:'external'}});
  assert.throws(()=>admitAssistantEvents(webEvent,false),{code:'ASSISTANT_ISOLATION_FAILED'});
  assert.match(reviewInstructions('likeavto'),/generationEditorial covering every FINAL proposals entry/);
  assert.ok(outputSchema(new Set(['item-1']),true,true).required.includes('generationEditorial'));
});

test('generation proof hashes exact final bytes, covers close and is compatible with legacy direct validation',()=>{
  const proposals=[{itemId:'item-1',kind:'reply_and_close',text:'Спасибо! '},{itemId:'item-2',kind:'close',text:''}];
  const value={text:'Review',sources:[],proposals,generationEditorial:proposals.map(p=>({...p,
    decision:'accept',reason:'Соответствует намерению и текущим правилам без новых фактов.',checks:pass}))};
  const evidence=admitGenerationEditorial(value,true);
  assert.equal(evidence.contract,'communityhero-editorial-v1');assert.equal(evidence.version,1);
  assert.equal(evidence.entries[0].textSha256,hash('Спасибо! '));assert.equal(evidence.entries[1].textSha256,hash(''));
  assert.equal(evidence.entries[0].text,undefined);
  assert.equal(admitGenerationEditorial({...value,generationEditorial:undefined}),undefined);
  assert.throws(()=>admitGenerationEditorial({...value,generationEditorial:undefined},true),invalidResponse);
  assert.equal(validateAssistantResult(value,new Set(['item-1','item-2'])).proposals.length,2);
  for(const generationEditorial of [[],[value.generationEditorial[0],value.generationEditorial[0]],
    value.generationEditorial.map((entry,i)=>i===0?{...entry,text:entry.text.trim()}:entry),
    value.generationEditorial.map((entry,i)=>i===0?{...entry,kind:'close'}:entry),
    value.generationEditorial.map((entry,i)=>i===0?{...entry,checks:{...pass,factualScope:'uncertain'}}:entry)])
    assert.throws(()=>admitGenerationEditorial({...value,generationEditorial},true),invalidResponse);
  assert.deepEqual(retainGenerationEditorial(evidence,{proposals:[proposals[1]]}).entries,[evidence.entries[1]]);
  assert.equal(retainGenerationEditorial(evidence,{proposals:[{...proposals[0],text:'Спасибо!'}]}).entries.length,0);
});

test('review quality hold drops corresponding generation proof and preserves independent candidate proof',async()=>{
  const items=[{id:'item-1'},{id:'item-2'}],proposals=items.map(item=>({itemId:item.id,kind:'reply_and_close',text:'Supported reply '+item.id}));
  const assessments=items.map(item=>({itemId:item.id,outcome:'reply',reason:'Supported'}));
  const value={text:'Review',sources:[],proposals,assessments,evidence:[{itemId:'item-1',url:'https://example.com/exact',title:'Official',
    claim:'Extracted fact unavailable',extraction:{status:'empty',observedAt:'2026-09-27T00:00:00Z'}}],
    generationEditorial:proposals.map(p=>({...p,decision:'accept',reason:'Ответ уместен и основан на приложенных фактах.',checks:pass}))};
  const prepared=prepareAssistantRequest({purpose:'triage_review',items,firstPass:{text:'First',sources:[],proposals,assessments}});
  const proof=admitGenerationEditorial(value,true);
  const reviewed=await admitReviewWithRepair(value,prepared,{calls:0,openedUrls:[],completedActivity:[]},{});
  assert.deepEqual(reviewed.admitted.proposals,[proposals[1]]);
  assert.deepEqual(retainGenerationEditorial(proof,reviewed.admitted).entries,[proof.entries[1]]);
});

test('editorial context keeps current field and total byte limits without truncation',()=>{
  const req=request();req.materials=Array.from({length:30},(_,i)=>({id:'m'+i,text:'x'.repeat(24000)}));
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_CONTEXT_TOO_LARGE'});
  const c=candidate();c.text='x'.repeat(12001);c.textSha256=hash(c.text);
  assert.throws(()=>prepareAssistantRequest(request([c])),invalidRequest);
});

test('cached research material preserves exact validated scope and extraction for editorial review',()=>{
  const quality={claimKind:'product_specification',scope:{model:'Q06',trim:'specific',market:'China',modelYear:'2026'},
    sourceScope:{model:'Q06',trim:'other',market:'China',observedAt:'2026-09-27'},
    extraction:{status:'missing_table',observedAt:'2026-09-27',rowLabels:['Motors'],columnLabels:['specific'],values:[]}};
  const req=request();req.materials=[{id:'cached',kind:'reference',trust:'source_only',text:'Claim from source',...quality}];
  const material=prepareAssistantRequest(req).payload.materials[0];
  for(const [key,value] of Object.entries(quality))assert.deepEqual(material[key],value);
  for(const patch of [{claimKind:'verified_fact'},{scope:{model:'Q06',secret:'hidden'}},
    {sourceScope:{market:123}},{extraction:{status:'invented',values:['guess']}}])
    assert.throws(()=>prepareAssistantRequest({...req,materials:[{...req.materials[0],...patch}]}),invalidRequest);
});

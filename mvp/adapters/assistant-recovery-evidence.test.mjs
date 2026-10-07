import test from 'node:test';import assert from 'node:assert/strict';import {createHash} from 'node:crypto';
import {prepareAssistantRequest,expandCompactOutput,admitSinglePassResult,admitAssistantEvents,singlePassMetadata,recoveryPublicUrl,recoveryEvidenceSnapshot} from './assistant.mjs';
const sha=x=>createHash('sha256').update(x).digest('hex');
const req=()=>({purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',recoveryEvidenceContract:'held_candidates_v1',items:[{id:'a',text:'Question'},{id:'b',text:'Context'}]});
const raw=()=>({text:'Prepared',evidence:[{itemId:'a',url:'https://example.com/actual?article=12',title:'Exact source',claim:'Bounded claim',claimKind:'source_statement',scope:null,sourceScope:null,extraction:null}],decisions:['a','b'].map(itemId=>({itemId,action:'reply_and_close',text:'Exact paid reply '+itemId,reason:'Exact intent',tags:[],editorial:{decision:'accept',reason:'Checked final',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}},basis:itemId==='a'?'web':'context',evidenceIndices:itemId==='a'?[0]:[],dependsOnItemIds:[],moderationRuleRefs:[]}))});
const event=(id,action,query)=>JSON.stringify({type:'item.completed',item:{id,type:'web_search',action:{type:action},query}});
const stream=[event('private-id','search','PRIVATE CUSTOMER QUERY'),event('page','other','https://example.com/other'),event('ref','other','turn0search0'),event('structured','other','{"open":[{"ref_id":"PRIVATE"}]}')].join('\n');
function run(value=raw(),request=req(),trace=admitAssistantEvents(stream,true,8,true,true)){
 const p=prepareAssistantRequest(request),result=admitSinglePassResult(expandCompactOutput(value,p.ids),p,trace);return {p,result,metadata:singlePassMetadata(p,result)};
}
test('new recovery selector is digest-bound and absent historical result/trace stays unchanged',()=>{
 const p=prepareAssistantRequest(req());assert.equal(p.recoveryEvidence,true);assert.equal(JSON.parse(p.input).recoveryEvidenceContract,'held_candidates_v1');
 const old=req();delete old.recoveryEvidenceContract;const q=prepareAssistantRequest(old);assert.notEqual(sha(p.input),sha(q.input));assert.equal(run(raw(),old).metadata.quarantinedRecovery,undefined);
 assert.equal(admitAssistantEvents(stream,true,8,true).recoveryActivity,undefined);
 for(const mutate of [r=>r.recoveryEvidenceContract='other',r=>r.recoveryEvidenceContract=null,r=>delete r.responseContract,r=>delete r.preparationMode]){const r=req();mutate(r);assert.throws(()=>prepareAssistantRequest(r),{code:'ASSISTANT_INVALID_REQUEST'});}
});
test('unobserved paid candidate and exact evidence survive privately without becoming proposal authority',()=>{
 const {p,result,metadata}=run(),q=metadata.quarantinedRecovery;assert.deepEqual(result.admitted.proposals.map(p=>p.itemId),['b']);
 assert.equal(q.admitted,false);assert.equal(q.inputSha256,sha(p.input));assert.equal(q.items[0].itemId,'a');assert.equal(q.items[0].text,'Exact paid reply a');assert.equal(q.items[0].textSha256,sha(q.items[0].text));
 assert.equal(q.items[0].sources[0].url,'https://example.com/actual?article=12');assert.equal(q.items[0].sources[0].trust,'source_only');
 assert.match(q.items[0].holdReason,/Источник/);assert.equal(result.evidence.length,0);assert.equal(q.omittedItemsCount,0);
});
test('observed literal URL and opaque ref survive, but customer searches/tool arguments/IDs never do',()=>{
 const q=run().metadata.quarantinedRecovery;assert.equal(q.activities.length,4);assert.equal(q.activities[1].requestedUrl,'https://example.com/other');assert.equal(q.activities[2].referenceId,'turn0search0');
 assert.doesNotMatch(JSON.stringify(q),/PRIVATE|private-id|ref_id/);assert.ok(q.activities.every(a=>!Object.hasOwn(a,'finalUrl')));
 const sameHost=run();assert.deepEqual(sameHost.result.admitted.proposals.map(p=>p.itemId),['b'],'Same-origin path remains unobserved');
});
test('sensitive URLs/candidates are explicitly omitted without discarding unaffected valid work',()=>{
 for(const url of ['https://example.com/?token=secret','https://example.com/?access%5Fkey=secret','https://example.com/?q=PRIVATE','http://127.0.0.1/a','https://u:p@example.com/a'])assert.equal(recoveryPublicUrl(url),null);
 const value=raw();value.decisions[0].text='Bearer abcdefghijklmnopqrstuvwxyz';const q=run(value);assert.equal(q.metadata.quarantinedRecovery.items.length,0);assert.equal(q.metadata.quarantinedRecovery.omittedItemsCount,1);assert.deepEqual(q.result.admitted.proposals.map(p=>p.itemId),['b']);
 const sensitive=admitAssistantEvents(event('i','other','https://example.com/?session=SECRET'),true,8,true,true);assert.equal(sensitive.recoveryActivity[0].requestedUrl,undefined);
});
test('bounded activity retention never caps research admission and reports overflow',()=>{
 const s=Array.from({length:520},(_,n)=>event(String(n),'other',`https://example.com/${n}`)).join('\n');const t=admitAssistantEvents(s,true,8,true,true);
 assert.equal(t.calls,520);assert.equal(t.openedUrls.length,520);assert.equal(t.recoveryActivity.length,512);assert.equal(t.omittedActivitiesCount,8);
 const q=run(raw(),req(),t).metadata.quarantinedRecovery;assert.ok(Buffer.byteLength(JSON.stringify(q))<=512*1024);assert.equal(q.omittedActivitiesCount,8);
});

test('final byte bound includes omission counter growth at the exact 512KiB boundary',()=>{
 const ids=Array.from({length:100},(_,i)=>`i${i}`),limit=512*1024;
 const value={proposals:ids.map(itemId=>({itemId,kind:'reply_and_close',text:'x'.repeat(11000)})),
  assessments:ids.map(itemId=>({itemId,reason:'r'})),
  generationEditorial:ids.map(itemId=>({itemId,decision:'accept',reason:'e',checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}})),
  decisionEvidence:ids.map(itemId=>({itemId,dependsOnItemIds:[]}))};
 const blocked=new Map(ids.map(id=>[id,'h'])),prepared={input:'exact immutable input'},trace={};
 value.proposals[46].text='';
 const prefix=recoveryEvidenceSnapshot(value,prepared,trace,[],new Map([...blocked].slice(0,47)));
 const padding=limit-Buffer.byteLength(JSON.stringify(prefix));assert.ok(padding>0&&padding<=12000);
 value.proposals[46].text='x'.repeat(padding);
 const exact=recoveryEvidenceSnapshot(value,prepared,trace,[],new Map([...blocked].slice(0,47)));
 assert.equal(Buffer.byteLength(JSON.stringify(exact)),limit);assert.equal(exact.items.length,47);
 const final=recoveryEvidenceSnapshot(value,prepared,trace,[],blocked);
 assert.ok(Buffer.byteLength(JSON.stringify(final))<=limit);
 assert.equal(final.items.length,46);assert.equal(final.omittedItemsCount,54);
 assert.equal(final.items.length+final.omittedItemsCount,100);
 for(const row of final.items){assert.equal(row.text,value.proposals.find(p=>p.itemId===row.itemId).text);assert.equal(row.textSha256,sha(row.text));}
});

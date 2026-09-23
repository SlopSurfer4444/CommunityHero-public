// Isolated process and deterministic adapter only; no social-network requests.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile,readFile} from 'node:fs/promises';
import {createServer} from 'node:net';
import path from 'node:path';
import os from 'node:os';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.COMMUNITYHERO_TEST_BINARY||path.join(root,'server/target/knowledge-check/debug/communityhero-server.exe');
const pg=process.env.COMMUNITYHERO_FEEDBACK_TEST_PG;
if(pg)assert.match(new URL(pg).pathname,/^\/ch_feedback_[a-z0-9_]+$/,'isolated feedback DB name required');
const temp=await mkdtemp(path.join(os.tmpdir(),'ch-feedback-http-'));
const scenario=path.join(temp,'scenario.json'),effects=path.join(temp,'effects.jsonl');
const baseScenario={autoData:true,createdAt:new Date().toISOString(),triageOutcome:'reply',runMetadata:{schemaVersion:1,model:'fake-fixture',reasoningEffort:'low',promptVersion:'fixture-v1',instructionSha256:'a'.repeat(64),inputSha256:'b'.repeat(64),cliSha256:'c'.repeat(64),elapsedMs:12,completedAt:new Date().toISOString()}};
await writeFile(scenario,JSON.stringify(baseScenario));
const reservation=createServer();await new Promise(resolve=>reservation.listen(0,'127.0.0.1',resolve));
const port=reservation.address().port;await new Promise(resolve=>reservation.close(resolve));
const base=`http://127.0.0.1:${port}`;let child,stderr='',csrf='';
async function run(file,args,env){const p=spawn(file,args,{windowsHide:true,env:{...process.env,...env},stdio:['ignore','ignore','pipe']});let err='';p.stderr.on('data',b=>err+=b);await new Promise((resolve,reject)=>{p.once('error',reject);p.once('exit',c=>c===0?resolve():reject(Error(`Fixture process failed: ${err}`)));});}
if(pg){const seed=path.join(temp,'seed.json');await writeFile(seed,JSON.stringify({account:'LikeAvto',items:[],posts:[],branches:[],conversations:[],proposals:[],approvals:[],operations:[],materials:[],jobs:[],audit:[],settings:{},sync:{status:'never'}}));await run(process.env.COMMUNITYHERO_TEST_MIGRATOR||path.join(root,'server/target/product-check/debug/migrate-workspace.exe'),['apply',seed],{COMMUNITYHERO_MIGRATION_DATABASE_URL:pg});}
async function until(check,timeout=45000){const end=Date.now()+timeout;while(Date.now()<end){if(child&&child.exitCode!==null)throw Error(`Test server exited: ${stderr}`);const v=await check();if(v)return v;await new Promise(r=>setTimeout(r,150));}throw Error('Feedback test timeout');}
async function start(){child=spawn(binary,[],{windowsHide:true,cwd:path.join(root,'server'),env:{...process.env,COMMUNITYHERO_DATABASE_URL:pg,COMMUNITYHERO_PORT:String(port),COMMUNITYHERO_DATA_DIR:temp,COMMUNITYHERO_NODE:process.execPath,COMMUNITYHERO_BRIDGE:path.join(root,'tests/fake-bridge.mjs'),COMMUNITYHERO_TEST_SCENARIO:scenario,COMMUNITYHERO_TEST_EFFECTS:effects},stdio:['ignore','ignore','pipe']});child.stderr.on('data',b=>stderr+=b);await until(async()=>{try{return (await fetch(base+'/api/health')).ok;}catch{return false;}},10000);}
async function stop(){if(child&&child.exitCode===null)await new Promise(resolve=>{child.once('exit',resolve);child.kill();});}
async function api(url,method='GET',body){const r=await fetch(base+url,{method,headers:{'Content-Type':'application/json','X-CSRF-Token':csrf,Origin:base},...(body===undefined?{}:{body:JSON.stringify(body)})});return {status:r.status,value:await r.json()};}
async function boot(){const r=await api('/api/bootstrap');assert.equal(r.status,200);csrf=r.value.csrfToken;return r.value;}
try{
 await start();let b=await until(async()=>{const b=await boot();return b.proposals.find(p=>p.prepareRunId)&&b;});
 let item=b.items[0];const source=b.proposals.find(p=>p.prepareRunId);
 assert.equal(source.generationMetadata.model,'fake-fixture');
 const refs={itemId:item.id,sourceProposalId:source.id,sourceProposalRevision:source.revision,draftSessionId:'review-session',sessionId:'test-operator'};
 const exposure={...refs,kind:'proposal_presented',eventId:'exposure-1'};
 assert.equal((await api('/api/feedback/events','POST',exposure)).status,200);
 assert.equal((await api('/api/feedback/events','POST',exposure)).status,200);
 let body={...refs,expectedRevision:item.revision,draft:'Исправленный ответ без обещания.',draftEdited:true,eventId:'edit-1'};
 let edit=await api(`/api/items/${item.id}`,'PATCH',body);assert.equal(edit.status,200,JSON.stringify(edit.value));
 assert.equal((await api(`/api/items/${item.id}`,'PATCH',body)).status,200,'lost response retry is idempotent');
 assert.equal((await api(`/api/items/${item.id}`,'PATCH',{...body,draft:'different'})).status,409,'same event different payload rejected');
 let catalog=(await api('/api/knowledge')).value;
 const first=catalog.feedback.find(e=>e.eventId==='edit-1');assert.equal(first.before,source.text);assert.equal(first.after,body.draft);
 body={...refs,expectedRevision:edit.value.revision,draft:'',draftEdited:true,eventId:'clear-1'};
 edit=await api(`/api/items/${item.id}`,'PATCH',body);assert.equal(edit.status,200);assert.equal(edit.value.draftEdited,true);
 await stop();await start();b=await boot();item=b.items.find(i=>i.id===item.id);assert.equal(item.draft,'');assert.equal(item.draftEdited,true);
 body={...refs,expectedRevision:item.revision,draft:source.text,draftEdited:true,eventId:'undo-1'};
 edit=await api(`/api/items/${item.id}`,'PATCH',body);assert.equal(edit.status,200);
 // A separately confirmed decision creates a reviewable hypothesis, never a rule.
 const changedRefs={...refs,draftSessionId:'candidate-session'};
 const changed=await api('/api/proposals','POST',{...changedRefs,expectedRevision:edit.value.revision,kind:'close',text:'',eventId:'decision-change'});
 assert.equal(changed.status,200,JSON.stringify(changed.value));
 assert.equal((await api('/api/approvals','POST',{proposals:[{id:changed.value.id,revision:changed.value.revision}]})).status,200);
 const candidate=(await api('/api/feedback/report')).value.candidates[0];
 assert.ok(candidate);assert.equal(candidate.status,'pending_review');assert.equal(candidate.beforeAction,'reply');assert.equal(candidate.afterAction,'close');
 const knowledgeBefore=(await api('/api/knowledge')).value.versions;
 assert.equal((await api('/api/feedback/events','POST',{...changedRefs,kind:'candidate_reviewed',candidateId:candidate.id,decision:'approved',eventId:'candidate-review-1'})).status,200);
 assert.equal((await api('/api/feedback/events','POST',{...changedRefs,kind:'feedback_labelled',label:'unnecessary_reply',note:'Isolated fixture label',eventId:'label-1'})).status,200);
 assert.deepEqual((await api('/api/knowledge')).value.versions,knowledgeBefore,'review does not promote knowledge');
 assert.equal((await api('/api/feedback/report')).value.candidates[0].status,'approved');
 const proposed=await api('/api/proposals','POST',{...refs,expectedRevision:edit.value.revision,kind:'reply_and_close',text:source.text,eventId:'decision-1'});
 assert.equal(proposed.status,200,JSON.stringify(proposed.value));assert.equal(proposed.value.sourceProposalId,source.id);assert.equal(proposed.value.prepareRunId,undefined,'origin is not reused as approval evidence');
 const approval=await api('/api/approvals','POST',{proposals:[{id:proposed.value.id,revision:proposed.value.revision}]});assert.equal(approval.status,200,JSON.stringify(approval.value));
 await writeFile(scenario,JSON.stringify({...baseScenario,uncertain:true,readbackUnknown:true}));
 assert.equal((await api(`/api/approvals/${approval.value.id}/execute`,'POST',{})).status,200);
 b=await until(async()=>{const b=await boot();return b.operations.some(o=>o.status==='unknown')&&b;});
 const op=b.operations.find(o=>o.status==='unknown');
 await writeFile(scenario,JSON.stringify(baseScenario));assert.equal((await api(`/api/operations/${op.id}/reconcile`,'POST',{})).status,200);
 await until(async()=>{const b=await boot();return b.operations.some(o=>o.id===op.id&&o.status==='succeeded');});
 catalog=(await api('/api/knowledge')).value;
 assert.equal(catalog.feedback.filter(e=>e.kind==='proposal_presented').length,1);
 assert.equal(catalog.feedback.filter(e=>e.kind==='review_confirmed').length,2);
 assert.ok(catalog.feedback.some(e=>e.kind==='proposal_cleared'));
 assert.ok(catalog.feedback.some(e=>e.kind==='execution_unknown'));
 assert.ok(catalog.feedback.some(e=>e.kind==='execution_verified'));
 assert.equal((await readFile(effects,'utf8')).trim().split('\n').length,1,'one fake execution despite reconciliation');
 const report=(await api('/api/feedback/report')).value;
 assert.equal(report.metrics.unchangedAmongComparableConfirmed.numerator,1,'undo is not a final negative vote');
 assert.equal(report.metrics.unchangedAmongComparableConfirmed.denominator,2,'autosaves are not votes');
 assert.equal(report.breakdown.confirmedComparableBy.model['fake-fixture'],2);
 await writeFile(path.join(temp,'report.json'),JSON.stringify(report,null,2));
 const beforeRestart=catalog.feedback;await stop();await start();await boot();
 assert.deepEqual((await api('/api/knowledge')).value.feedback,beforeRestart,'immutable observations survive restart');
 console.log(JSON.stringify({ok:true,storage:pg?'postgres':'sqlite',events:beforeRestart.length,report:path.join(temp,'report.json'),fakeExecutions:1,liveExecutions:0}));
}finally{await stop();}

// Run against an isolated Rust server configured with fake-bridge.mjs.
// Never point this harness at a real adapter.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile,readFile,copyFile,mkdir} from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const temp=await mkdtemp(path.join(os.tmpdir(),'communityhero-mvp-acceptance-'));
const scenario=path.join(temp,'scenario.json'),effects=path.join(temp,'effects.jsonl');
await writeFile(scenario,'{}');
const port=String(process.env.COMMUNITYHERO_TEST_PORT||'4196');
const base=`http://127.0.0.1:${port}`;
const pgUrl=process.env.COMMUNITYHERO_TEST_PG_URL;
const restorePgUrl=process.env.COMMUNITYHERO_TEST_PG_RESTORE_URL;
async function importSnapshot(file,url){
 const binary=process.env.COMMUNITYHERO_TEST_MIGRATOR||path.join(root,'server/target/debug/migrate-workspace.exe');
 const child=spawn(binary,['apply',file],{windowsHide:true,env:{...process.env,COMMUNITYHERO_MIGRATION_DATABASE_URL:url},stdio:['ignore','ignore','pipe']});
 let error='';child.stderr.on('data',b=>error+=b.toString());
 await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',code=>code===0?resolve():reject(Error(`Test import failed: ${error}`)));});
}
let server,stderr='';
async function start(directory=temp,databaseUrl=pgUrl){
 server=spawn(process.env.COMMUNITYHERO_TEST_BINARY||path.join(root,'server/target/debug/communityhero-server.exe'),[],{cwd:path.join(root,'server'),windowsHide:true,env:{...process.env,COMMUNITYHERO_DATABASE_URL:databaseUrl,COMMUNITYHERO_PORT:port,COMMUNITYHERO_DATA_DIR:directory,COMMUNITYHERO_NODE:process.execPath,COMMUNITYHERO_BRIDGE:path.join(root,'tests/fake-bridge.mjs'),COMMUNITYHERO_TEST_SCENARIO:scenario,COMMUNITYHERO_TEST_EFFECTS:effects},stdio:['ignore','ignore','pipe']});
 server.stderr.on('data',b=>{stderr+=b.toString();});
 for(let n=0;n<100;n++){if(server.exitCode!==null)throw Error(`server exited: ${stderr}`);try{if((await fetch(base+'/api/health')).ok)return;}catch{}await new Promise(r=>setTimeout(r,100));}
 throw Error('server startup timeout');
}
async function stop(){if(!server||server.exitCode!==null)return;await new Promise(resolve=>{server.once('exit',resolve);server.kill();});}
let csrf='';
async function api(url,method='GET',body){const response=await fetch(base+url,{method,headers:{'Content-Type':'application/json','X-CSRF-Token':csrf,Origin:base},...(body===undefined?{}:{body:JSON.stringify(body)})});let value;try{value=await response.json();}catch{value={};}return{status:response.status,value};}
async function bootstrap(){const r=await api('/api/bootstrap');assert.equal(r.status,200);csrf=r.value.csrfToken;return r.value;}
async function awaitState(predicate){for(let n=0;n<100;n++){const b=await bootstrap();if(predicate(b))return b;await new Promise(r=>setTimeout(r,100));}throw Error('state timeout');}
try{
 if(pgUrl){
  assert.ok(restorePgUrl,'PG acceptance requires a separate restore database');
  assert.notEqual(restorePgUrl,pgUrl,'Restore must use a different database');
  const seed=path.join(temp,'seed.json');
  await writeFile(seed,JSON.stringify({account:'LikeAvto',items:[],posts:[],branches:[],conversations:[],proposals:[],approvals:[],operations:[],materials:[],jobs:[],audit:[],settings:{},sync:{status:'never'}}));
  await importSnapshot(seed,pgUrl);
 }
 await start();let b=await bootstrap();assert.ok(csrf);
 assert.equal((await fetch(base+'/api/sync',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'})).status,403,'csrf required');
 assert.equal((await fetch(base+'/api/bootstrap',{headers:{Origin:'https://example.com'}})).status,403,'foreign origins denied');
 await api('/api/sync','POST',{});b=await awaitState(x=>x.items.length>0);
 let item=b.items[0];
 let edit=await api(`/api/items/${item.id}`,'PATCH',{expectedRevision:item.revision,draft:'Human draft'});assert.equal(edit.status,200);
 assert.equal((await api(`/api/items/${item.id}`,'PATCH',{expectedRevision:item.revision,draft:'stale overwrite'})).status,409);
 b=await bootstrap();item=b.items[0];assert.equal(item.draft,'Human draft');
 const mat=await api('/api/materials','POST',{title:'Verified fact',text:'Persistent knowledge',kind:'knowledge'});assert.equal(mat.status,200);
 const convo=await api('/api/conversations','POST',{title:'Acceptance',itemIds:[item.id]});assert.equal(convo.status,200);const convoId=convo.value.id;
 assert.ok(convoId);
 await api(`/api/conversations/${convoId}/messages`,'POST',{text:'Prepare reply',itemIds:[item.id]});
 b=await awaitState(x=>x.proposals.length>0);
 assert.equal(b.items[0].draft,'Human draft','model must not overwrite manual draft');
 const generated=b.proposals[0];
 assert.ok(generated.prepareBundleId,'AI proposal retains exact evidence bundle');
 assert.ok(b.jobs.some(j=>j.kind==='assistant'&&j.prepareBundle&&!j.prepareBundle.request),'bootstrap does not duplicate private evidence snapshots');
 const changed=await api('/api/materials','POST',{title:'Changed policy',text:'A newly supplied global fact requires another review',kind:'knowledge'});
 let knowledge=(await api('/api/knowledge')).value;
 const entry=knowledge.entries.find(e=>e.sourceMaterialId===changed.value.id);
 assert.ok(entry,'new material enters the knowledge catalog');
 assert.equal(entry.status,'pending_review','unreviewed facts are quarantined');
 const activated=await api(`/api/knowledge/${entry.id}/versions`,'POST',{expectedVersionId:entry.currentVersionId,status:'active',trust:'verified',provenance:'Isolated deterministic test fixture, not a real fact'});
 assert.equal(activated.status,200,JSON.stringify(activated.value));
 assert.equal((await api('/api/approvals','POST',{proposals:[{id:generated.id,revision:generated.revision}]})).status,409,'changed material invalidates generated proposal');
 await stop();await start();b=await bootstrap();assert.equal(b.items[0].draft,'Human draft');assert.ok(b.conversations.some(c=>c.id===convoId));assert.ok(b.materials.some(m=>m.title==='Verified fact'));
 const superseded=await api('/api/proposals','POST',{itemId:item.id,kind:'reply_and_close',text:'First revision',expectedRevision:b.items[0].revision});assert.equal(superseded.status,200);
 const oldApproval=await api('/api/approvals','POST',{proposals:[{id:superseded.value.id,revision:superseded.value.revision}]});assert.equal(oldApproval.status,200);
 assert.equal((await api(`/api/proposals/${superseded.value.id}`,'PATCH',{expectedRevision:superseded.value.revision,text:'Edited after approval'})).status,200);
 assert.equal((await api(`/api/approvals/${oldApproval.value.id}/execute`,'POST',{})).status,409,'edit invalidates exact approval');
 b=await bootstrap();
 const stale=await api('/api/proposals','POST',{itemId:item.id,kind:'close',text:'',expectedRevision:b.items[0].revision});assert.equal(stale.status,200);
 const staleApproval=await api('/api/approvals','POST',{proposals:[{id:stale.value.id,revision:stale.value.revision}]});assert.equal(staleApproval.status,200);
 await writeFile(scenario,JSON.stringify({contextChanged:true}));
 await api(`/api/approvals/${staleApproval.value.id}/execute`,'POST',{});
 await awaitState(x=>x.operations.some(o=>o.proposalId===stale.value.id&&o.status==='stale'));
 await writeFile(scenario,'{}');b=await bootstrap();
 const manual=await api('/api/proposals','POST',{itemId:item.id,kind:'reply_and_close',text:'Approved exact reply',expectedRevision:b.items[0].revision});assert.equal(manual.status,200);
 const p=manual.value;assert.ok(p.id);
 const approval=await api('/api/approvals','POST',{proposals:[{id:p.id,revision:p.revision}]});assert.equal(approval.status,200);assert.ok(approval.value.id);
 await writeFile(scenario,JSON.stringify({uncertain:true,readbackUnknown:true}));
 await api(`/api/approvals/${approval.value.id}/execute`,'POST',{});
 b=await awaitState(x=>x.operations.some(o=>String(o.status||o.state).toLowerCase()==='unknown'));
 assert.ok((await api(`/api/approvals/${approval.value.id}/execute`,'POST',{})).status>=400,'no repeat execution');
 const unknown=b.operations.find(o=>String(o.status||o.state).toLowerCase()==='unknown');
 await writeFile(scenario,JSON.stringify({uncertain:true}));
 await api(`/api/operations/${unknown.id}/reconcile`,'POST',{});
 b=await awaitState(x=>x.operations.some(o=>['confirmed','verified','succeeded'].includes(String(o.status||o.state).toLowerCase())));
 const effectLines=(await readFile(effects,'utf8')).trim().split('\n');assert.equal(effectLines.length,1,'exactly one fake external attempt');
 const backup=await api('/api/backup','POST',{});assert.equal(backup.status,200);assert.ok(backup.value.path);
 const restored=path.join(temp,'restored');await mkdir(restored);
 if(pgUrl){await importSnapshot(backup.value.path,restorePgUrl);}else{await copyFile(backup.value.path,path.join(restored,'workspace.sqlite'));}
 await stop();await start(restored,restorePgUrl);b=await bootstrap();assert.equal(b.items[0].draft,'Human draft');assert.ok(b.conversations.some(c=>c.id===convoId));assert.ok(b.operations.some(o=>o.id===unknown.id));
 console.log(JSON.stringify({ok:true,test:'http-acceptance',database:pgUrl?'postgres':'sqlite',directory:temp,checks:['csrf','origin','read','draft-CAS','persistent-chat','persistent-knowledge','model-does-not-overwrite','restart','approval-invalidated-on-edit','fresh-context-before-dispatch','approval','uncertain-send','no-retry','readback','backup-restore'],externalEffects:'fake only'}));
}finally{await stop();}

// Real compiled Rust server + API + its own conductor child. Synthetic transport/model.
// Requires an explicit candidate. Never builds, reads company state or calls real providers.
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile,rename,stat,readdir} from 'node:fs/promises';
import {appendFileSync} from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import {createHash,randomUUID} from 'node:crypto';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {prepareLifecycleFixture} from './conductor-acceptance-lifecycle-fixture.mjs';
const mvp=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const account=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_ACCOUNT??'likeavto';
assert.ok(['likeavto','baw-russia'].includes(account),'Unsupported synthetic acceptance account');
const caseNames=['scale','admission','repair','effect','pause','revoked','corrupt','isolation'];
const requestedCases=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CASES?.split(',').map(name=>name.trim())??caseNames;
assert.ok(requestedCases.length&&requestedCases.every(name=>caseNames.includes(name))&&new Set(requestedCases).size===requestedCases.length,'Unknown or duplicate acceptance case selection');
const skippedCases=account==='baw-russia'?[{name:'isolation',reason:'BAW-only scope excludes the separate two-company fixture; not executed or passed'}]:[];
const selectedCases=requestedCases.filter(name=>!skippedCases.some(row=>row.name===name));
assert.ok(selectedCases.length,'No executable acceptance cases in the selected company scope');
const itemId=index=>`item-${account}-${index}`,providerId=index=>`${account}-provider-${index}`;
if(process.argv[2]==='--plan'){
  assert.equal(process.argv.length,3);console.log(JSON.stringify({account,selectedCases,skippedCases,
    exampleItemId:itemId(0),exampleProviderId:providerId(0),nativeExecuted:false,fixturesCreated:false}));process.exit(0);
}
assert.equal(process.argv.length,2,'Use no arguments for execution or --plan for a read-only selection check');
const binary=process.env.COMMUNITYHERO_TEST_BINARY;
assert.ok(binary&&path.isAbsolute(binary),'Explicit absolute candidate binary required; this harness never builds');
await stat(binary);
// Explicit ROOT-owned execution transport; scenario assertions stay below.
const controlModule=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_MODULE;
assert.ok(controlModule&&path.isAbsolute(controlModule),'Bounded acceptance control module required');
assert.equal(createHash('sha256').update(await readFile(controlModule)).digest('hex'),process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CONTROL_SHA256,'Acceptance control source drift');
const executionControl=await (await import(pathToFileURL(controlModule).href)).getAcceptanceControl();
const outputOverride=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_OUTPUT;
if(outputOverride)assert.ok(path.isAbsolute(outputOverride),'Acceptance output override must be absolute');
const output=outputOverride??path.resolve(mvp,'runs/engine-v80-20261001/acceptance',`run-${new Date().toISOString().replace(/[:.]/g,'-')}-${randomUUID()}`);
await mkdir(path.dirname(output),{recursive:true});await mkdir(output);
const children=new Set(),results=[],started=Date.now();
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const candidateHash=sha(await readFile(binary));
const diagnostics=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_DIAGNOSTICS==='1';
async function sourcePins(){
  const files=['tests/conductor-acceptance-lifecycle-fixture.mjs','tests/conductor-acceptance.mjs','tests/conductor-acceptance-bridge.mjs','tests/conductor-acceptance-fixture.mjs','tests/conductor-acceptance-fixture.test.mjs',...(diagnostics?['tests/conductor-acceptance-observer.mjs']:[])];
  async function walk(folder){for(const entry of await readdir(path.join(mvp,folder),{withFileTypes:true})){const file=path.join(folder,entry.name);if(entry.isDirectory())await walk(file);else if(entry.name.endsWith('.mjs'))files.push(file);}}
  await walk('cli');await walk('adapters');
  return Object.fromEntries(await Promise.all(files.sort().map(async file=>[file.replaceAll(path.sep,'/'),sha(await readFile(path.join(mvp,file)))])));
}
const initialSources=await sourcePins();
async function lines(file){try{return (await readFile(file,'utf8')).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);}catch(error){if(error.code==='ENOENT')return [];throw error;}}
async function settings(fixture,value){const file=path.join(fixture,'scenario.json'),temp=file+'.tmp';await writeFile(temp,JSON.stringify(value));await rename(temp,file);}
async function freePort(){const server=net.createServer();await new Promise((resolve,reject)=>server.listen(0,'127.0.0.1',resolve).once('error',reject));const port=server.address().port;await new Promise(resolve=>server.close(resolve));return port;}
async function stop(child,hard=false){
  if(!child||!children.has(child)||child.exitCode!==null||child.signalCode!==null)return;
  await executionControl.stopServer(child,{mode:hard?'crash':'stop'});
  assert.ok(child.exitCode!==null||child.signalCode!==null,'Harness-created process did not stop');
}
async function eventually(task,predicate,message,timeout=60000){const deadline=Date.now()+timeout;let last;while(Date.now()<deadline){last=await task();if(predicate(last))return last;await sleep(100);}throw Error(`${message}; last=${JSON.stringify(last).slice(0,2500)}`);}
async function fixture(name,scenario,fixtureAccount=account){
  const folder=path.join(output,name),fixture=path.join(folder,'fixture'),data=path.join(folder,'data');
  await mkdir(fixture,{recursive:true});await settings(fixture,scenario);
  return {folder,fixture,data,account:fixtureAccount,scenario,effects:path.join(fixture,'effects.jsonl'),trace:path.join(fixture,'trace.jsonl')};
}
async function launch(f){
  // The checkpoint binds the canonical base URL. A process restart preserves
  // fixture identity; allocate a different port only for a different fixture.
  const port=f.port??(f.port=await freePort()),base=`http://127.0.0.1:${port}`;
  const clean=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.toUpperCase().startsWith('COMMUNITYHERO_')&&!['DATABASE_URL','CODEX_HOME','OPENAI_API_KEY','NODE_OPTIONS'].includes(key.toUpperCase())));
  const lifecycleEnv=await prepareLifecycleFixture(f,{binary,mvp,output});
  const child=await executionControl.launchServer(binary,[],{cwd:path.join(mvp,'server'),windowsHide:true,env:{...clean,...lifecycleEnv,
    ...(diagnostics?{NODE_OPTIONS:`--import=${pathToFileURL(path.join(mvp,'tests/conductor-acceptance-observer.mjs')).href}`}:{ }),
    COMMUNITYHERO_MEDIA_EVIDENCE_DIR:path.join(f.folder,'media-evidence'),COMMUNITYHERO_ACCOUNT:f.account,COMMUNITYHERO_PORT:String(port),COMMUNITYHERO_DATA_DIR:f.data,
    COMMUNITYHERO_NODE:process.execPath,COMMUNITYHERO_BRIDGE:path.join(mvp,'tests/conductor-acceptance-bridge.mjs'),
    COMMUNITYHERO_CONDUCTOR_FIXTURE_ROOT:f.fixture,COMMUNITYHERO_EXTERNAL_WRITES:'enabled',
    COMMUNITYHERO_BACKGROUND_DISABLED:'1',COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED:'1',
    COMMUNITYHERO_COMMENT_PREPARATION_DISABLED:'1',COMMUNITYHERO_AUTOMATIC_READBACK_DISABLED:'1',COMMUNITYHERO_MEDIA_PREPARATION_ENABLED:'0'},
    stdio:['ignore','pipe','pipe']});
  const launchNumber=f.launchNumber=(f.launchNumber??0)+1;
  // Only our isolated process is logged; the fixture environment contains no
  // credentials or authenticated session. Persist crash diagnostics immediately.
  const stderrFile=path.join(f.folder,`server-${launchNumber}.stderr.log`),stdoutFile=path.join(f.folder,`server-${launchNumber}.stdout.log`);
  children.add(child);let stderr='';child.stderr.on('data',chunk=>{stderr+=chunk;appendFileSync(stderrFile,chunk);});child.stdout.on('data',chunk=>appendFileSync(stdoutFile,chunk));
  child.on('error',error=>stderr+=error.message);
  let csrf='',storageGeneration='';
  async function api(route,method='GET',body){const response=await fetch(base+route,{method,signal:AbortSignal.timeout(20000),
    headers:{'Content-Type':'application/json',Origin:base,'X-CSRF-Token':csrf,...(storageGeneration?{'X-Communityhero-Workspace-Generation':storageGeneration}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
    let value;try{value=await response.json();}catch{value={};}return {status:response.status,value};}
  async function ok(route,method='GET',body){const response=await api(route,method,body);assert.equal(response.status,200,`${route}: ${JSON.stringify(response.value)}`);return response.value;}
  await eventually(async()=>{if(child.exitCode!==null)throw Error(`Server exited: ${stderr}`);try{return (await api('/api/health')).status;}catch{return 0;}},status=>status===200,'server startup',30000);
  const initial=await ok('/api/bootstrap');csrf=initial.csrfToken;assert.ok(csrf);
  assert.equal(initial.account,f.account==='baw-russia'?'BAW Russia':'LikeAvto','Fixture server changed company');
  if(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_PG==='isolated-candidate'){
    storageGeneration=initial.storageGeneration;assert.match(storageGeneration,/^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/);assert.equal(storageGeneration,lifecycleEnv.COMMUNITYHERO_EXPECTED_STORAGE_GENERATION);
    const admitted=await ok('/api/maintenance/connection/admit','POST',{});assert.equal(admitted.status,'admitted');assert.equal(admitted.caseSha256,lifecycleEnv.COMMUNITYHERO_CONNECTION_ADMISSION_CASE_SHA256);assert.equal(admitted.admittedContinuationProof.kind,'verified-connection-continuation-admission');assert.equal(admitted.admittedContinuationProof.storageGeneration,storageGeneration);assert.equal(admitted.admittedContinuationProof.owner.releaseSha256,process.env.COMMUNITYHERO_WAVE_CORE_SHA256);
  }
  const status=run=>ok(`/api/conductor/runs/${run}`);
  const campaign=async(ids,mode='execute',requestId=randomUUID(),limits={})=>ok('/api/conductor/runs','POST',{
    requestId,scope:{itemIds:ids},mode,actionKinds:['reply_and_close'],limits:{batchSize:60,maxRepairRounds:2,maxCycles:5000,...limits}});
  const waitRun=(run,timeout=120000)=>eventually(()=>status(run),job=>['completed','blocked','paused','revoked','recovery_required'].includes(job.status),`campaign ${run}`,timeout);
  async function sync(){const response=await ok('/api/sync','POST',{mode:'open'});await eventually(()=>ok(`/api/engine/jobs/${response.jobId}`),job=>['completed','failed'].includes(job.status),'fixture sync');const state=await ok('/api/bootstrap');assert.equal(state.items.length,f.scenario.itemCount);return state;}
  return {child,base,api,ok,status,campaign,waitRun,sync,stderr:()=>stderr};
}
async function captureExecutionCheckpoint(folder,before,runId,baseUrl) {
  const original=before.operations.filter(operation=>operation.status==='dispatching');
  assert.ok(original.length&&original.every(operation=>operation.conductorRunId===runId),'Interrupted operations must belong to the original run');
  const ids=values=>[...new Set(values)].sort(),matches=[];
  let files;try{files=await readdir(folder,{recursive:true,withFileTypes:true});}catch(error){if(error.code==='ENOENT')return null;throw error;}
  for(const entry of files) {
    assert.equal(entry.isSymbolicLink(),false,'Linked execution checkpoint');
    const file=path.join(entry.parentPath,entry.name),relative=path.relative(folder,file).replaceAll(path.sep,'/');
    if(!entry.isFile()||!/^cycle-[1-9]\d*-slice-[1-9]\d*\.json(?:\.ready\/[a-f0-9]{64}\.json)?$/.test(relative))continue;
    const saved=JSON.parse(await readFile(file,'utf8'));
    if(typeof saved.executeRequestId!=='string'||typeof saved.executeJobId!=='string')continue;
    assert.equal(saved.account,before.account,'Execution checkpoint company differs');assert.equal(saved.baseUrl,baseUrl,'Execution checkpoint server differs');
    if(relative.includes('.ready/')) {
      const [parent,batch]=relative.split('.ready/'),state=JSON.parse(await readFile(path.join(folder,parent),'utf8'));
      assert.ok(state.readyBatches?.some(row=>row.id===batch.slice(0,-5)),'Execution child is not declared by its owned parent');
    }
    const job=before.jobs.find(row=>row.id===saved.executeJobId),approval=before.approvals.find(row=>row.id===saved.approvalId);
    if(!job||job.kind!=='execute'||job.conductorRunId!==runId||job.refId!==saved.approvalId||!approval||approval.conductorRunId!==runId)continue;
    if(!original.every(operation=>operation.approvalId===saved.approvalId&&operation.grantGeneration===job.grantGeneration)||approval.grantGeneration!==job.grantGeneration)continue;
    if(JSON.stringify(ids(saved.itemIds))!==JSON.stringify(ids(original.map(operation=>operation.itemId))))continue;
    if(JSON.stringify(ids(saved.approvedProposals?.map(row=>row.id)||[]))!==JSON.stringify(ids(original.map(operation=>operation.proposalId))))continue;
    if(JSON.stringify(ids(saved.approvedProposals.map(row=>`${row.id}:${row.revision}`)))!==JSON.stringify(ids(approval.proposals.map(row=>`${row.id}:${row.revision}`))))continue;
    matches.push({...saved,checkpointFile:file});
  }
  assert.ok(matches.length<=1,'Ambiguous original execution checkpoint');return matches[0]??null;
}
async function evidence(f){const effects=await lines(f.effects),trace=await lines(f.trace);const keys=effects.map(row=>`${row.account}:${row.action.itemId}`);
  assert.equal(new Set(keys).size,keys.length,'Provider was dispatched twice for one exact recipient; fake ledger does not dedup');
  assert.ok([...effects,...trace].every(row=>row.account===f.account),'Cross-company fake bridge call');return {effects,trace};}
async function record(name,details){const result={name,...details};results.push(result);await writeFile(path.join(output,'progress.json'),JSON.stringify({startedAt:new Date(started).toISOString(),results},null,2));console.log(JSON.stringify(result));}
async function scale(){
  const scaleStarted=Date.now();
  const itemCount=Number(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_COUNT??240);assert.ok(Number.isInteger(itemCount)&&itemCount>200&&itemCount<=1200,'Single-page synthetic fixture supports 201..1200 exact recipients');
  const held=Array.from({length:200},(_,index)=>itemId(index)),f=await fixture('scale',{itemCount,heldItemIds:held});const s=await launch(f);
  try{const state=await s.sync(),ids=state.items.map(item=>item.id),requestId='one-exact-start';const start=await s.campaign(ids,'execute',requestId);
    const replay=await s.campaign(ids,'execute',requestId);assert.equal(replay.runId,start.runId);assert.equal(replay.replayed,true);
    const conflict=await s.api('/api/conductor/runs','POST',{requestId,scope:{itemIds:ids.slice(1)},mode:'execute',actionKinds:['reply_and_close']});assert.equal(conflict.status,409);
    const job=await s.waitRun(start.runId,300000),proof=await evidence(f);
    await writeFile(path.join(f.folder,'campaign.json'),JSON.stringify(job,null,2));
    assert.equal(proof.effects.length,itemCount-200,JSON.stringify({status:job.status,error:job.error,progress:job.conductor.progress}));assert.ok(!proof.effects.some(row=>Number(row.action.itemId.split('-').at(-1))<200),'Held family caused fake provider effects');
    assert.equal(job.status,'completed','Independent ready family did not reach an autonomous terminal disposition');
    assert.equal(job.result?.summary?.held,200);assert.equal(job.result?.summary?.verified,itemCount-200);
    assert.deepEqual(new Set(job.result?.itemHolds?.map(row=>row.itemId)),new Set(held),'Canonical result omitted or mis-scoped held recipients');
    assert.deepEqual(new Set(proof.effects.map(row=>row.action.itemId)),new Set(state.items.filter(item=>!held.includes(item.id)).map(item=>item.itemId)),'Provider effects did not match the exact admitted ready recipients');
    const calls=proof.trace.filter(row=>row.event==='started'&&row.operation==='assistant'&&row.preparationMode==='single_pass_v1');
    assert.ok(calls.length>=Math.ceil(itemCount/100),'Manifest did not split above model recipient capacity');
    assert.ok(calls.every(row=>row.itemIds.length<=100));assert.deepEqual(new Set(calls.flatMap(row=>row.itemIds)),new Set(ids));
    const editorial=proof.trace.filter(row=>row.event==='started'&&row.operation==='assistant'&&row.candidates?.length);
    function phaseMs(startedRows){const pids=new Set(startedRows.map(row=>row.pid)),completed=proof.trace.filter(row=>row.event==='completed'&&row.operation==='assistant'&&pids.has(row.pid));return completed.length?Math.max(...completed.map(row=>Date.parse(row.at)))-Math.min(...startedRows.map(row=>Date.parse(row.at))):null;}
    await sleep(500);assert.equal((await lines(f.effects)).length,proof.effects.length,'Terminal campaign was not quiescent');assert.equal((await s.status(start.runId)).status,'completed');
    await record('one-start-exact-manifest',{itemCount,preparedCalls:calls.length,distinctPreparationScopes:new Set(calls.map(row=>JSON.stringify(row.itemIds))).size,editorialCalls:editorial.length,
      modelCalls:calls.length+editorial.length,wallTimeMs:Date.now()-scaleStarted,preparationEnvelopeMs:phaseMs(calls),editorialEnvelopeMs:phaseMs(editorial),
      effects:proof.effects.length,verified:job.result.summary.verified,heldFamilyRecipients:held.length,quiescent:true,status:job.status,runId:start.runId});
  }finally{await stop(s.child);}
}
async function recovery(name,stage){
  const uncertain=['context:before','execute:after_effect'].includes(stage),itemCount=uncertain?120:12;
  const uncertainIds=Array.from({length:60},(_,index)=>providerId(index));
  const f=await fixture(name,{itemCount,gates:[stage],...(uncertain?{gateProviderIds:uncertainIds,unknownProviderIds:uncertainIds}:{}),reviseItemIds:[itemId(0)]});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));
    await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.event==='gate'&&row.stage===stage),`${name} controlled crash boundary`);
    if(stage==='execute:after_effect')assert.ok((await lines(f.effects)).length>0,'Crash boundary did not exercise external-effect seam');
    let before,receipt,executeRequestId,protectedProviderIds;
    if(uncertain){
      before=await s.ok('/api/bootstrap');assert.ok(before.operations.some(operation=>operation.status==='dispatching'),'Crash did not follow durable execution admission');
      if(stage==='context:before')assert.equal((await lines(f.effects)).length,0,'Admission boundary already produced an effect');
      const checkpointFolder=path.join(f.data,'conductor',start.runId,'queue.json.slices');
      const saved=await eventually(()=>captureExecutionCheckpoint(checkpointFolder,before,start.runId,s.base),Boolean,'captured original execute admission identity');
      executeRequestId=saved.executeRequestId;receipt=await s.ok(`/api/local-admissions/execute/${executeRequestId}`);assert.equal(receipt.status,'committed');
      assert.equal(receipt.result.jobId,saved.executeJobId);assert.equal(receipt.result.approvalId,saved.approvalId);assert.equal(receipt.result.requestId,executeRequestId);
      const original=before.operations.filter(operation=>operation.status==='dispatching');protectedProviderIds=original.map(operation=>operation.action.itemId);
      assert.equal(new Set(protectedProviderIds).size,original.length,'Interrupted provider targets must be exact and unique');
      assert.deepEqual(new Set(protectedProviderIds),new Set(original.map(operation=>before.items.find(item=>item.id===operation.itemId)?.itemId)),'Provider targets differ from original canonical recipients');
    }
    await stop(s.child,true);await settings(f.fixture,{...f.scenario,gates:[],...(uncertain?{unknownProviderIds:protectedProviderIds}:{})});s=await launch(f);
    let job=await s.waitRun(start.runId,uncertain?600000:120000),proof=await evidence(f);
    if(uncertain){
      const state=await s.ok('/api/bootstrap'),unknown=state.operations.filter(operation=>operation.status==='unknown');
      const original=before.operations.filter(operation=>operation.status==='dispatching'),protectedIds=new Set(original.map(operation=>operation.itemId));
      assert.ok(protectedIds.size<itemCount,'Fixture did not leave independent recipients outside the interrupted admission');
      assert.ok(original.every(operation=>unknown.some(current=>current.id===operation.id&&current.itemId===operation.itemId)),'Original admitted operation identities lost UNKNOWN quarantine');
      assert.ok(state.operations.filter(operation=>protectedIds.has(operation.itemId)).every(operation=>original.some(prior=>prior.id===operation.id)),'Restart created a second operation for an uncertain recipient');
      assert.ok(state.items.filter(item=>!protectedIds.has(item.id)).every(item=>['closed','deleted'].includes(item.workflow)),'Engine did not autonomously finish unrelated recipients while UNKNOWN remained');
      const afterReceipt=await s.ok(`/api/local-admissions/execute/${executeRequestId}`);assert.equal(afterReceipt.status,'committed');assert.deepEqual(afterReceipt.result,receipt.result);assert.equal(afterReceipt.payloadHash,receipt.payloadHash);
      assert.equal(job.result?.summary?.unknown,protectedIds.size);assert.equal(job.result?.summary?.verified,itemCount-protectedIds.size);
      if(stage==='context:before')assert.ok(!proof.effects.some(row=>protectedIds.has(state.items.find(item=>item.itemId===row.action.itemId)?.id)),'UNKNOWN admission was blindly retried');
      await writeFile(path.join(f.folder,'quarantine.json'),JSON.stringify({originalOperationIds:original.map(operation=>operation.id),executeRequestId,executeJobId:receipt.result.jobId,unknown:protectedIds.size,independentVerified:itemCount-protectedIds.size},null,2));
    }else assert.equal(proof.effects.length,12,'Recovery did not finish every unaffected exact recipient');
    if(stage==='editorial_repaired:before') {
      const revisions=proof.trace.filter(row=>row.event==='started'&&row.candidates?.some(candidate=>candidate.itemId===itemId(0))).flatMap(row=>row.candidates.filter(candidate=>candidate.itemId===itemId(0)));
      assert.ok(revisions.some(candidate=>candidate.text.startsWith('Synthetic initial reply')),'Original editorial candidate was not exercised');
      assert.ok(revisions.some(candidate=>candidate.text.startsWith('Repaired synthetic reply')),'Saved repair was not independently re-reviewed');
      const original=revisions.find(candidate=>candidate.text.startsWith('Synthetic initial reply')),repaired=revisions.find(candidate=>candidate.text.startsWith('Repaired synthetic reply'));
      assert.ok(repaired.proposalRevision>original.proposalRevision,'Repair did not save a new proposal revision');
      assert.notEqual(repaired.textSha256,original.textSha256,'Repair did not change exact reviewed bytes');
      assert.ok(proof.trace.some(row=>row.event==='completed'&&row.editorial?.some(entry=>entry.proposalId===repaired.proposalId&&entry.proposalRevision===repaired.proposalRevision&&entry.textSha256===repaired.textSha256&&entry.decision==='accept')),'No fresh accepted exact repaired revision');
    }
    await record(name,{runId:start.runId,status:job.status,effects:proof.effects.length,traceCalls:proof.trace.length});
  }finally{await stop(s.child);}
}
async function revoked(){
  const f=await fixture('revoked-durable-grant',{itemCount:12,gates:['assistant:after']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.stage==='assistant:after'),'grant revocation fixture boundary');await stop(s.child,true);
    if(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_PG==='isolated-candidate')await executionControl.revokeFixtureJob(f,start.runId);else{
    const file=path.join(f.data,'workspace.sqlite');assert.ok(path.resolve(file).startsWith(path.resolve(output)+path.sep),'Fixture mutation escaped owned output');
    const db=new DatabaseSync(file);try{const state=JSON.parse(db.prepare('SELECT payload FROM workspace WHERE id=1').get().payload),job=state.jobs.find(job=>job.id===start.runId);
      assert.equal(job.kind,'conductor');job.conductor.desiredState='revoked';job.conductor.leaseGeneration++;job.status='revoked';db.prepare('UPDATE workspace SET payload=? WHERE id=1').run(JSON.stringify(state));
    }finally{db.close();}}
    await settings(f.fixture,{...f.scenario,gates:[]});s=await launch(f);assert.equal((await s.status(start.runId)).conductor.desiredState,'revoked');
    assert.equal((await s.api(`/api/conductor/runs/${start.runId}/resume`,'POST',{})).status,409);await sleep(500);assert.equal((await lines(f.effects)).length,0);
    await record('revoked-durable-grant-fails-closed',{runId:start.runId,effects:0,method:process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_PG==='isolated-candidate'?'Seed revoked desiredState in own stopped PG fixture; not an operator-token revocation API':'Seed revoked desiredState in own stopped SQLite fixture; not an operator-token revocation API'});
  }finally{await stop(s.child);}
}
async function pause(){
  const f=await fixture('pause',{itemCount:12,gates:['assistant:after']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.stage==='assistant:after'),'inflight prepare before pause');
    await s.ok(`/api/conductor/runs/${start.runId}/pause`,'POST',{});const paused=await eventually(()=>s.status(start.runId),job=>job.conductor.desiredState==='paused','pause generation fence');
    await settings(f.fixture,{...f.scenario,gates:[]});await sleep(500);assert.equal((await lines(f.effects)).length,0,'Paused late model result triggered provider');
    await stop(s.child,true);s=await launch(f);assert.equal((await s.status(start.runId)).conductor.desiredState,'paused');await sleep(500);assert.equal((await lines(f.effects)).length,0);
    await s.ok(`/api/conductor/runs/${start.runId}/resume`,'POST',{});const job=await s.waitRun(start.runId);const proof=await evidence(f);assert.equal(proof.effects.length,12,JSON.stringify(job));
    await record('pause-late-result-restart-resume',{pausedGeneration:paused.conductor.leaseGeneration,finalGeneration:job.conductor.leaseGeneration,effects:proof.effects.length});
  }finally{await stop(s.child);}
}
async function corrupt(){
  const f=await fixture('checkpoint-corruption',{itemCount:12,gates:['assistant:after']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.stage==='assistant:after'),'checkpoint fixture boundary');await stop(s.child,true);
    const checkpoint=path.join(f.data,'conductor',start.runId,'queue.json');await writeFile(checkpoint,'{"version":999,"corrupt":true}');
    s=await launch(f);const job=await s.waitRun(start.runId);assert.ok(['blocked','recovery_required'].includes(job.status),'Corruption did not produce an explicit blocked recovery disposition');assert.equal((await lines(f.effects)).length,0);
    await record('checkpoint-corruption-fails-closed',{runId:start.runId,status:job.status,effects:0});
  }finally{await stop(s.child);}
}
async function isolation(){
  assert.notEqual(account,'baw-russia','Two-company fixture is outside BAW-only acceptance');
  const a=await fixture('likeavto-isolation',{itemCount:4},'likeavto'),b=await fixture('baw-isolation',{itemCount:4},'baw-russia');const sa=await launch(a),sb=await launch(b);
  try{const [aa,bb]=await Promise.all([sa.sync(),sb.sync()]);const runA=await sa.campaign(aa.items.map(item=>item.id),'prepare','same-request');
    const runB=await sb.campaign(bb.items.map(item=>item.id),'prepare','same-request');assert.notEqual(runA.runId,runB.runId);
    assert.equal((await sb.api(`/api/conductor/runs/${runA.runId}`)).status,404);
    const foreign=await sb.api('/api/conductor/runs','POST',{requestId:'foreign-target',scope:{itemIds:[aa.items[0].id]},mode:'prepare',actionKinds:[]});assert.ok(foreign.status>=400);
    const jobs=await Promise.all([sa.waitRun(runA.runId),sb.waitRun(runB.runId)]);
    for(const job of jobs){assert.equal(job.status,'completed','Company preparation did not complete');assert.equal(job.result?.summary?.prepared,4,'Company preparation did not save its own exact proposals');}
    assert.equal((await evidence(a)).effects.length,0);assert.equal((await evidence(b)).effects.length,0);
    await record('two-company-isolation',{runA:runA.runId,runB:runB.runId,externalEffects:0});
  }finally{await stop(sa.child);await stop(sb.child);}
}
let failure;
const cases={scale,admission:()=>recovery('restart-after-local-admission','context:before'),repair:()=>recovery('restart-after-saved-repair','editorial_repaired:before'),
  effect:()=>recovery('restart-after-provider-effect','execute:after_effect'),pause,revoked,corrupt,isolation};
try{assert.ok(selectedCases.length&&selectedCases.every(name=>Object.hasOwn(cases,name)),'Unknown acceptance case selection');for(const name of selectedCases)await cases[name]();}
catch(error){failure=error;console.error(error.stack);}
finally{for(const child of children)await stop(child,true);const finalCandidateHash=sha(await readFile(binary));
  if(finalCandidateHash!==candidateHash&&!failure)failure=Error('Candidate binary changed during acceptance');
  const sources=await sourcePins(),sourceDrift=[...new Set([...Object.keys(initialSources),...Object.keys(sources)])].filter(file=>initialSources[file]!==sources[file]);
  if(sourceDrift.length&&!failure)failure=Error(`JavaScript source changed during acceptance: ${sourceDrift.join(', ')}`);
  const receipt={ok:!failure,accountScope:account,skippedCases,startedAt:new Date(started).toISOString(),finishedAt:new Date().toISOString(),
  binary:{path:binary,sha256:candidateHash,finalSha256:finalCandidateHash},lifecycleFixtureInputs:{classification:'NONRELEASE synthetic production-bootstrap fixture',core:{path:process.env.COMMUNITYHERO_WAVE_CORE_PATH,sha256:process.env.COMMUNITYHERO_WAVE_CORE_SHA256},nativeTest:{path:process.env.COMMUNITYHERO_WAVE_TEST_EXECUTABLE,sha256:process.env.COMMUNITYHERO_WAVE_TEST_SHA256}},admission:process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_ADMISSION??'unreviewed',diagnostics,initialSources,sources,sourceDrift,output,selectedCases,results,error:failure?.message??null,boundedExecution:await executionControl.receipts(),
  limitations:['Synthetic model judgment and provider receipts do not establish live semantic quality or throughput.',
    'Mirrored public text uses separate post/platform IDs; this does not prove admitted video/audio equivalence.',
    'Loopback local-owner grant cannot exercise authenticated operator-token rotation; dedicated Rust authority fixture covers that boundary.',
    'This run uses isolated SQLite. PostgreSQL acceptance remains the campaign PG fixture owner responsibility.']};
  await writeFile(path.join(output,'receipt.json'),JSON.stringify(receipt,null,2));console.log(JSON.stringify({receipt:path.join(output,'receipt.json'),ok:receipt.ok}));}
if(failure)process.exitCode=1;

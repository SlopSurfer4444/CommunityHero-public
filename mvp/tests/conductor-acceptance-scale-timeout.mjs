// Real compiled Rust server + API + its own conductor child. Synthetic transport/model.
// Requires an explicit candidate. Never builds, reads company state or calls real providers.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile,rename,stat,readdir} from 'node:fs/promises';
import {appendFileSync} from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import {createHash,randomUUID} from 'node:crypto';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
const mvp=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const binary=process.env.COMMUNITYHERO_TEST_BINARY;
assert.ok(binary&&path.isAbsolute(binary),'Explicit absolute candidate binary required; this harness never builds');
await stat(binary);
const output=path.resolve(mvp,'runs/engine-v80-20261001/acceptance',`run-${new Date().toISOString().replace(/[:.]/g,'-')}-${randomUUID()}`);
await mkdir(output,{recursive:true});
const children=new Set(),results=[],started=Date.now();
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const candidateHash=sha(await readFile(binary));
const diagnostics=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_DIAGNOSTICS==='1';
const scaleObservationBudgetMs=Number(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_SCALE_TIMEOUT_MS??300000);
const recoveryObservationBudgetMs=Number(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_RECOVERY_TIMEOUT_MS??120000);
assert.ok(Number.isInteger(scaleObservationBudgetMs)&&scaleObservationBudgetMs>=300000&&scaleObservationBudgetMs<=1800000,'Declared scale observation budget outside 300000..1800000ms');
assert.ok(Number.isInteger(recoveryObservationBudgetMs)&&recoveryObservationBudgetMs>=120000&&recoveryObservationBudgetMs<=600000,'Declared recovery observation budget outside 120000..600000ms');
async function sourcePins(){
  const files=['tests/conductor-acceptance.mjs','tests/conductor-acceptance-bridge.mjs','tests/conductor-acceptance-fixture.mjs','tests/conductor-acceptance-fixture.test.mjs',...(diagnostics?['tests/conductor-acceptance-observer.mjs']:[])];
  files.push('tests/conductor-acceptance-scale-timeout.mjs');
  async function walk(folder){for(const entry of await readdir(path.join(mvp,folder),{withFileTypes:true})){const file=path.join(folder,entry.name);if(entry.isDirectory())await walk(file);else if(entry.name.endsWith('.mjs'))files.push(file);}}
  await walk('cli');await walk('adapters');
  return Object.fromEntries(await Promise.all(files.sort().map(async file=>[file.replaceAll(path.sep,'/'),sha(await readFile(path.join(mvp,file)))])));
}
const initialSources=await sourcePins();
const canonicalHarnessPath='tests/conductor-acceptance.mjs';
async function lines(file){try{return (await readFile(file,'utf8')).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);}catch(error){if(error.code==='ENOENT')return [];throw error;}}
async function settings(fixture,value){const file=path.join(fixture,'scenario.json'),temp=file+'.tmp';await writeFile(temp,JSON.stringify(value));await rename(temp,file);}
async function freePort(){const server=net.createServer();await new Promise((resolve,reject)=>server.listen(0,'127.0.0.1',resolve).once('error',reject));const port=server.address().port;await new Promise(resolve=>server.close(resolve));return port;}
async function stop(child,hard=false){
  if(!child||!children.has(child)||child.exitCode!==null||child.signalCode!==null)return;
  // Windows tree kill targets only an exact PID created and still owned by this harness.
  if(process.platform==='win32'){
    const killer=spawn('taskkill',['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
    await new Promise(resolve=>killer.once('exit',resolve));
  }else child.kill(hard?'SIGKILL':'SIGTERM');
  await Promise.race([new Promise(resolve=>child.once('exit',resolve)),sleep(3000)]);
  assert.ok(child.exitCode!==null||child.signalCode!==null,'Harness-created process did not stop');
}
async function eventually(task,predicate,message,timeout=60000){const deadline=Date.now()+timeout;let last;while(Date.now()<deadline){last=await task();if(predicate(last))return last;await sleep(100);}throw Error(`${message}; last=${JSON.stringify(last).slice(0,2500)}`);}
async function fixture(name,scenario,account='likeavto'){
  const folder=path.join(output,name),fixture=path.join(folder,'fixture'),data=path.join(folder,'data');
  await mkdir(fixture,{recursive:true});await settings(fixture,scenario);
  return {folder,fixture,data,account,scenario,effects:path.join(fixture,'effects.jsonl'),trace:path.join(fixture,'trace.jsonl')};
}
async function launch(f){
  // The checkpoint binds the canonical base URL. A process restart preserves
  // fixture identity; allocate a different port only for a different fixture.
  const port=f.port??(f.port=await freePort()),base=`http://127.0.0.1:${port}`;
  const clean=Object.fromEntries(Object.entries(process.env).filter(([key])=>!key.toUpperCase().startsWith('COMMUNITYHERO_')&&!['DATABASE_URL','CODEX_HOME','OPENAI_API_KEY','NODE_OPTIONS'].includes(key.toUpperCase())));
  const child=spawn(binary,[],{cwd:path.join(mvp,'server'),windowsHide:true,env:{...clean,
    ...(diagnostics?{NODE_OPTIONS:`--import=${pathToFileURL(path.join(mvp,'tests/conductor-acceptance-observer.mjs')).href}`}:{ }),
    COMMUNITYHERO_ACCOUNT:f.account,COMMUNITYHERO_PORT:String(port),COMMUNITYHERO_DATA_DIR:f.data,
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
  let csrf='';
  async function api(route,method='GET',body){const response=await fetch(base+route,{method,signal:AbortSignal.timeout(20000),
    headers:{'Content-Type':'application/json',Origin:base,'X-CSRF-Token':csrf},...(body===undefined?{}:{body:JSON.stringify(body)})});
    let value;try{value=await response.json();}catch{value={};}return {status:response.status,value};}
  async function ok(route,method='GET',body){const response=await api(route,method,body);assert.equal(response.status,200,`${route}: ${JSON.stringify(response.value)}`);return response.value;}
  await eventually(async()=>{if(child.exitCode!==null)throw Error(`Server exited: ${stderr}`);try{return (await api('/api/health')).status;}catch{return 0;}},status=>status===200,'server startup',30000);
  const initial=await ok('/api/bootstrap');csrf=initial.csrfToken;assert.ok(csrf);
  const status=run=>ok(`/api/conductor/runs/${run}`);
  const campaign=async(ids,mode='execute',requestId=randomUUID(),limits={})=>ok('/api/conductor/runs','POST',{
    requestId,scope:{itemIds:ids},mode,actionKinds:['reply_and_close'],limits:{batchSize:60,maxRepairRounds:2,maxCycles:5000,...limits}});
  const waitRun=(run,timeout=120000)=>eventually(()=>status(run),job=>['completed','blocked','paused','revoked','recovery_required'].includes(job.status),`campaign ${run}`,timeout);
  async function sync(){const response=await ok('/api/sync','POST',{mode:'open'});await eventually(()=>ok(`/api/engine/jobs/${response.jobId}`),job=>['completed','failed'].includes(job.status),'fixture sync');const state=await ok('/api/bootstrap');assert.equal(state.items.length,f.scenario.itemCount);return state;}
  return {child,base,api,ok,status,campaign,waitRun,sync,stderr:()=>stderr};
}
async function evidence(f){const effects=await lines(f.effects),trace=await lines(f.trace);const keys=effects.map(row=>`${row.account}:${row.action.itemId}`);
  assert.equal(new Set(keys).size,keys.length,'Provider was dispatched twice for one exact recipient; fake ledger does not dedup');
  assert.ok([...effects,...trace].every(row=>row.account===f.account),'Cross-company fake bridge call');return {effects,trace};}
async function record(name,details){const result={name,...details};results.push(result);await writeFile(path.join(output,'progress.json'),JSON.stringify({startedAt:new Date(started).toISOString(),results},null,2));console.log(JSON.stringify(result));}
async function scale(){
  const scaleStarted=Date.now();
  const itemCount=Number(process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_COUNT??240);assert.ok(Number.isInteger(itemCount)&&itemCount>200&&itemCount<=1200,'Single-page synthetic fixture supports 201..1200 exact recipients');
  const held=Array.from({length:200},(_,index)=>`item-likeavto-${index}`),f=await fixture('scale',{itemCount,heldItemIds:held});const s=await launch(f);
  try{const state=await s.sync(),ids=state.items.map(item=>item.id),requestId='one-exact-start';const start=await s.campaign(ids,'execute',requestId);
    const replay=await s.campaign(ids,'execute',requestId);assert.equal(replay.runId,start.runId);assert.equal(replay.replayed,true);
    const conflict=await s.api('/api/conductor/runs','POST',{requestId,scope:{itemIds:ids.slice(1)},mode:'execute',actionKinds:['reply_and_close']});assert.equal(conflict.status,409);
    const job=await s.waitRun(start.runId,scaleObservationBudgetMs),proof=await evidence(f);
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
  const caseStarted=Date.now();
  const uncertain=['context:before','execute:after_effect'].includes(stage),itemCount=uncertain?120:12;
  const uncertainIds=Array.from({length:60},(_,index)=>`likeavto-provider-${index}`);
  const f=await fixture(name,{itemCount,gates:[stage],...(uncertain?{gateProviderIds:uncertainIds,unknownProviderIds:uncertainIds}:{}),reviseItemIds:uncertain?[]:['item-likeavto-0']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));
    await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.event==='gate'&&row.stage===stage),`${name} controlled crash boundary`);
    if(stage==='execute:after_effect')assert.ok((await lines(f.effects)).length>0,'Crash boundary did not exercise external-effect seam');
    let before,receipt,executeRequestId;
    if(uncertain){
      before=await s.ok('/api/bootstrap');assert.ok(before.operations.some(operation=>operation.status==='dispatching'),'Crash did not follow durable execution admission');
      if(stage==='context:before')assert.equal((await lines(f.effects)).length,0,'Admission boundary already produced an effect');
      const checkpoint=path.join(f.data,'conductor',start.runId,'queue.json.slices','cycle-1-slice-1.json');
      const admitted=before.operations.filter(operation=>operation.status==='dispatching');
      assert.equal(admitted.length,60,'Fixture did not capture the exact first 60 admitted operations');
      const approvalIds=new Set(admitted.map(operation=>operation.approvalId));assert.equal(approvalIds.size,1,'Ambiguous original execution approval');
      const saved=await eventually(async()=>{
        const parent=JSON.parse(await readFile(checkpoint,'utf8')),matches=[];
        for(const batch of parent.readyBatches??[]){
          assert.ok(typeof batch.id==='string'&&/^[a-f0-9]{64}$/.test(batch.id),'Invalid captured ready batch identity');
          const sidecar=path.join(checkpoint+'.ready',batch.id+'.json');assert.ok(path.resolve(sidecar).startsWith(path.resolve(f.data)+path.sep),'Ready sidecar escaped owned fixture');
          let candidate;try{candidate=JSON.parse(await readFile(sidecar,'utf8'));}catch(error){if(error.code==='ENOENT')continue;throw error;}
          if(typeof candidate.executeRequestId==='string'&&typeof candidate.executeJobId==='string'&&approvalIds.has(candidate.approvalId))matches.push(candidate);
        }
        assert.ok(matches.length<=1,'Ambiguous matching execution sidecars');return matches[0]??{};
      },row=>typeof row.executeRequestId==='string','captured exact ready-sidecar execution admission identity');
      const originalJob=await s.ok('/api/engine/jobs/'+saved.executeJobId);assert.equal(originalJob.kind,'execute');assert.equal(originalJob.refId,saved.approvalId);
      executeRequestId=saved.executeRequestId;receipt=await s.ok('/api/local-admissions/execute/'+executeRequestId);assert.equal(receipt.status,'committed');
      assert.equal(receipt.result.jobId,saved.executeJobId);assert.equal(receipt.result.approvalId,saved.approvalId);assert.equal(receipt.payloadHash,sha(JSON.stringify({approvalId:saved.approvalId})));
    }
    await stop(s.child,true);await settings(f.fixture,{...f.scenario,gates:[]});s=await launch(f);
    let job=await s.waitRun(start.runId,recoveryObservationBudgetMs),proof=await evidence(f);
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
      const revisions=proof.trace.filter(row=>row.event==='started'&&row.candidates?.some(candidate=>candidate.itemId==='item-likeavto-0')).flatMap(row=>row.candidates.filter(candidate=>candidate.itemId==='item-likeavto-0'));
      assert.ok(revisions.some(candidate=>candidate.text.startsWith('Synthetic initial reply')),'Original editorial candidate was not exercised');
      assert.ok(revisions.some(candidate=>candidate.text.startsWith('Repaired synthetic reply')),'Saved repair was not independently re-reviewed');
      const original=revisions.find(candidate=>candidate.text.startsWith('Synthetic initial reply')),repaired=revisions.find(candidate=>candidate.text.startsWith('Repaired synthetic reply'));
      assert.ok(repaired.proposalRevision>original.proposalRevision,'Repair did not save a new proposal revision');
      assert.notEqual(repaired.textSha256,original.textSha256,'Repair did not change exact reviewed bytes');
      assert.ok(proof.trace.some(row=>row.event==='completed'&&row.editorial?.some(entry=>entry.proposalId===repaired.proposalId&&entry.proposalRevision===repaired.proposalRevision&&entry.textSha256===repaired.textSha256&&entry.decision==='accept')),'No fresh accepted exact repaired revision');
    }
    await record(name,{runId:start.runId,status:job.status,effects:proof.effects.length,traceCalls:proof.trace.length,wallTimeMs:Date.now()-caseStarted,observationBudgetMs:recoveryObservationBudgetMs});
  }finally{await stop(s.child);}
}
async function revoked(){
  const f=await fixture('revoked-durable-grant',{itemCount:12,gates:['assistant:after']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.stage==='assistant:after'),'grant revocation fixture boundary');await stop(s.child,true);
    const file=path.join(f.data,'workspace.sqlite');assert.ok(path.resolve(file).startsWith(path.resolve(output)+path.sep),'Fixture mutation escaped owned output');
    const db=new DatabaseSync(file);try{const state=JSON.parse(db.prepare('SELECT payload FROM workspace WHERE id=1').get().payload),job=state.jobs.find(job=>job.id===start.runId);
      assert.equal(job.kind,'conductor');job.conductor.desiredState='revoked';job.conductor.leaseGeneration++;job.status='revoked';db.prepare('UPDATE workspace SET payload=? WHERE id=1').run(JSON.stringify(state));
    }finally{db.close();}
    await settings(f.fixture,{...f.scenario,gates:[]});s=await launch(f);assert.equal((await s.status(start.runId)).conductor.desiredState,'revoked');
    assert.equal((await s.api(`/api/conductor/runs/${start.runId}/resume`,'POST',{})).status,409);await sleep(500);assert.equal((await lines(f.effects)).length,0);
    await record('revoked-durable-grant-fails-closed',{runId:start.runId,effects:0,method:'Seed revoked desiredState in own stopped SQLite fixture; not an operator-token revocation API'});
  }finally{await stop(s.child);}
}
async function pause(){
  const caseStarted=Date.now();
  const f=await fixture('pause',{itemCount:12,gates:['assistant:after']});let s=await launch(f);
  try{const state=await s.sync(),start=await s.campaign(state.items.map(item=>item.id));await eventually(()=>lines(f.trace),rows=>rows.some(row=>row.stage==='assistant:after'),'inflight prepare before pause');
    await s.ok(`/api/conductor/runs/${start.runId}/pause`,'POST',{});const paused=await eventually(()=>s.status(start.runId),job=>job.conductor.desiredState==='paused','pause generation fence');
    await settings(f.fixture,{...f.scenario,gates:[]});await sleep(500);assert.equal((await lines(f.effects)).length,0,'Paused late model result triggered provider');
    await stop(s.child,true);s=await launch(f);assert.equal((await s.status(start.runId)).conductor.desiredState,'paused');await sleep(500);assert.equal((await lines(f.effects)).length,0);
    await s.ok(`/api/conductor/runs/${start.runId}/resume`,'POST',{});const job=await s.waitRun(start.runId,recoveryObservationBudgetMs);const proof=await evidence(f);assert.equal(proof.effects.length,12,JSON.stringify(job));
    await record('pause-late-result-restart-resume',{pausedGeneration:paused.conductor.leaseGeneration,finalGeneration:job.conductor.leaseGeneration,effects:proof.effects.length,wallTimeMs:Date.now()-caseStarted,observationBudgetMs:recoveryObservationBudgetMs});
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
  const a=await fixture('likeavto-isolation',{itemCount:4}),b=await fixture('baw-isolation',{itemCount:4},'baw-russia');const sa=await launch(a),sb=await launch(b);
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
const selectedCases=process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_CASES?.split(',').map(name=>name.trim())??Object.keys(cases);
try{assert.ok(selectedCases.length&&selectedCases.every(name=>Object.hasOwn(cases,name)),'Unknown acceptance case selection');for(const name of selectedCases)await cases[name]();}
catch(error){failure=error;console.error(error.stack);}
finally{for(const child of children)await stop(child,true);const finalCandidateHash=sha(await readFile(binary));
  if(finalCandidateHash!==candidateHash&&!failure)failure=Error('Candidate binary changed during acceptance');
  const sources=await sourcePins(),sourceDrift=[...new Set([...Object.keys(initialSources),...Object.keys(sources)])].filter(file=>initialSources[file]!==sources[file]);
  if(sourceDrift.length&&!failure)failure=Error(`JavaScript source changed during acceptance: ${sourceDrift.join(', ')}`);
  const receipt={ok:!failure,startedAt:new Date(started).toISOString(),finishedAt:new Date().toISOString(),
  binary:{path:binary,sha256:candidateHash,finalSha256:finalCandidateHash},admission:process.env.COMMUNITYHERO_CONDUCTOR_ACCEPTANCE_ADMISSION??'unreviewed',diagnostics,scaleObservationBudgetMs,recoveryObservationBudgetMs,derivedFrom:{path:canonicalHarnessPath,sha256:initialSources[canonicalHarnessPath]},initialSources,sources,sourceDrift,output,selectedCases,results,error:failure?.message??null,
  limitations:['Synthetic model judgment and provider receipts do not establish live semantic quality or throughput.',
    'Mirrored public text uses separate post/platform IDs; this does not prove admitted video/audio equivalence.',
    'Loopback local-owner grant cannot exercise authenticated operator-token rotation; dedicated Rust authority fixture covers that boundary.',
    'This run uses isolated SQLite. PostgreSQL acceptance remains the campaign PG fixture owner responsibility.']};
  await writeFile(path.join(output,'receipt.json'),JSON.stringify(receipt,null,2));console.log(JSON.stringify({receipt:path.join(output,'receipt.json'),ok:receipt.ok}));}
if(failure)process.exitCode=1;

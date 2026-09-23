// Explicit opt-in: only a separately restored performance database, no provider.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp,writeFile} from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const url=process.env.COMMUNITYHERO_PERFORMANCE_DATABASE_URL;
assert.ok(url && /^communityhero_preparation_perf_\d+$/.test(new URL(url).pathname.slice(1)), 'Isolated restored performance database required');
const binary=process.env.COMMUNITYHERO_TEST_BINARY;
assert.ok(binary,'Explicit candidate binary required');
const dir=await mkdtemp(path.join(os.tmpdir(),'communityhero-scale-'));
const base='http://127.0.0.1:4196';
const scenario=path.join(dir,'scenario.json');
await writeFile(scenario,JSON.stringify({autoData:false}));
let stderr='';
const child=spawn(binary,[],{windowsHide:true,cwd:path.join(root,'server'),env:{...process.env,
 COMMUNITYHERO_DATABASE_URL:url,COMMUNITYHERO_PORT:'4196',COMMUNITYHERO_DATA_DIR:dir,
 COMMUNITYHERO_ACCESS_FILE:undefined,COMMUNITYHERO_PUBLIC_ORIGIN:undefined,
 COMMUNITYHERO_NODE:process.execPath,COMMUNITYHERO_BRIDGE:path.join(root,'tests/fake-bridge.mjs'),
 COMMUNITYHERO_TEST_SCENARIO:scenario,COMMUNITYHERO_BACKGROUND_DISABLED:'1',
 COMMUNITYHERO_EXTERNAL_WRITES:'disabled'},stdio:['ignore','ignore','pipe']});
child.stderr.on('data',b=>stderr+=b.toString());
const timings=[];
async function request(route,options={}) {
 const start=performance.now();
 const response=await fetch(base+route,{...options,signal:AbortSignal.timeout(15000)});
 const body=await response.json();
 assert.ok(response.ok,`${route}: ${response.status}`);
 timings.push({route,ms:Math.round(performance.now()-start)});
 return body;
}
try {
 const until=Date.now()+30000;
 while(true){
  if(child.exitCode!==null)throw Error('Candidate stopped during startup');
  try{await fetch(base+'/api/health',{signal:AbortSignal.timeout(1000)});break;}catch{}
  assert.ok(Date.now()<until,'Candidate startup timeout');
  await new Promise(r=>setTimeout(r,250));
 }
 const session=await request('/api/session');
 const headers={'content-type':'application/json','origin':base,'x-csrf-token':session.csrfToken};
 const post=(route,body)=>request(route,{method:'POST',headers,body:JSON.stringify(body)});
 const initial=await request('/api/bootstrap');
 assert.ok(initial.items.length>1000,'Fixture must include real-sized imported history');
 const operations=initial.operations.length;
 const previews=await Promise.all([
  request('/api/maintenance/preparation'),request('/api/bootstrap'),
  post('/api/maintenance/preparation/restart',{runId:'offline-scale-v1',apply:false})
 ]);
 const config=await post('/api/maintenance/preparation',{enabled:true,dailyLimit:100,itemDailyLimit:2,debounceSeconds:30});
 assert.equal(config.configuration.enabled,true);
 const restart=await post('/api/maintenance/preparation/restart',{runId:'offline-scale-v1',apply:true});
 const repeat=await post('/api/maintenance/preparation/restart',{runId:'offline-scale-v1',apply:true});
 assert.deepEqual(repeat,restart,'Restart must be idempotent');
 const final=await request('/api/bootstrap');
 assert.equal(final.operations.length,operations);
 assert.equal(previews[0].workerEnabled,false,'Offline scale test never starts preparation');
 assert.ok(!stderr.includes('pool_timeout'),'No database pool timeouts');
 assert.ok(timings.every(x=>x.ms<10000),'All endpoints fit the DB pool acquisition budget with margin');
 console.log(JSON.stringify({ok:true,items:initial.items.length,eligible:restart.eligibleCount,externalEffects:0,timings}));
} finally {
 if(child.exitCode===null)await new Promise(resolve=>{child.once('exit',resolve);child.kill();});
}

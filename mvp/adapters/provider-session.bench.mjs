// Offline IPC benchmark: synthetic payloads only, no provider or credential call.
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {performance} from 'node:perf_hooks';
import {runProcess} from './process.mjs';

const count=100,parallelism=4;
const folder=await mkdtemp(path.join(tmpdir(),'communityhero-offline-ipc-'));
try{
  const once=path.join(folder,'once.mjs'),worker=path.join(folder,'worker.mjs');
  await writeFile(once,"let s='';for await(const c of process.stdin)s+=c;const r=JSON.parse(s);process.stdout.write(JSON.stringify({ok:true,result:{itemId:r.itemId}}));");
  await writeFile(worker,`import {serveProviderSession} from ${JSON.stringify(new URL('./provider-session.mjs',import.meta.url).href)};await serveProviderSession({account:'likeavto',session:{run:async r=>({itemId:r.itemId}),close:async()=>{}}});`);
  let next=0;const baselineStart=performance.now();
  await Promise.all(Array.from({length:parallelism},async()=>{
    while(next<count){const index=next++;const result=await runProcess(process.execPath,[once],{input:JSON.stringify({itemId:String(index)})});if(JSON.parse(result.stdout).result.itemId!==String(index))throw new Error('BENCH_CORRELATION');}
  }));
  const baselineMs=performance.now()-baselineStart;
  const workerStart=performance.now();
  await new Promise((resolve,reject)=>{
    const child=spawn(process.execPath,[worker],{windowsHide:true,stdio:['pipe','pipe','pipe']});
    const pending=new Set();let sent=0,received=0;const timer=setTimeout(()=>{child.kill();reject(new Error('BENCH_TIMEOUT'));},30000);
    child.on('error',reject);child.stderr.resume();
    const dispatch=()=>{
      while(sent<count&&pending.size<parallelism){const id=String(sent++);pending.add(id);child.stdin.write(JSON.stringify({id,request:{account:'likeavto',operation:'context',itemId:id}})+'\n');}
      if(received===count)child.stdin.end();
    };
    createInterface({input:child.stdout}).on('line',line=>{
      try{const r=JSON.parse(line);if(r.type==='retiring')return;if(!r.ok||!pending.delete(r.id)||r.result.itemId!==r.id)throw new Error('BENCH_CORRELATION');received++;dispatch();}
      catch(error){child.kill();reject(error);}
    });
    child.on('close',code=>{clearTimeout(timer);if(code===0&&received===count)resolve();else reject(new Error('BENCH_WORKER_FAILED'));});dispatch();
  });
  console.log(JSON.stringify({kind:'offline-synthetic-ipc-only',requests:count,parallelism,baseline:{nodeLaunches:count,elapsedMs:baselineMs},finiteWorker:{nodeLaunches:1,elapsedMs:performance.now()-workerStart},limitations:['one Node boundary per baseline request, not full production stack','no HTTP, OS credentials, PostgreSQL or model calls','wall time is host/load-specific; not a production throughput forecast']},null,2));
}finally{await rm(folder,{recursive:true,force:true});}

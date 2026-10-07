import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough,Writable} from 'node:stream';
import {serveProviderSession} from './provider-session.mjs';

const request=id=>({id,request:{operation:'context',account:'likeavto',itemId:id}});
function harness(run,limits={}){
  const input=new PassThrough(),output=new PassThrough();let text='',closes=0;
  output.on('data',chunk=>{text+=chunk;
    // Match Rust: acknowledge retirement with EOF before awaiting pending work.
    if(String(chunk).split('\n').filter(Boolean).some(line=>JSON.parse(line).type==='retiring'))input.end();
  });
  const done=serveProviderSession({account:'likeavto',input,output,limits,session:{run,close:async()=>{closes++;}}});
  return {input,done,rows:()=>text.trim().split('\n').filter(Boolean).map(JSON.parse),closes:()=>closes,
    send:value=>input.write(JSON.stringify(value)+'\n')};
}
test('finite worker correlates concurrent out-of-order responses and EOF drains',async()=>{
  let active=0,max=0;const calls=[];
  const h=harness(async r=>{active++;max=Math.max(max,active);calls.push(r.itemId);await new Promise(resolve=>setTimeout(resolve,r.itemId==='slow'?20:1));active--;return {itemId:r.itemId};});
  h.send(request('slow'));h.send(request('fast'));h.input.end();await h.done;
  const results=h.rows().filter(r=>r.id);assert.deepEqual(results.map(r=>r.id),['fast','slow']);assert.equal(max,2);assert.equal(h.closes(),1);assert.equal(calls.length,2);
});
test('request cap signals retirement but does not cancel active execution',async()=>{
  let release;const gate=new Promise(resolve=>release=resolve);let finished=false;
  const h=harness(async()=>{await gate;finished=true;return {verified:true};},{maxRequests:1});
  h.send(request('one'));await new Promise(resolve=>setImmediate(resolve));
  assert.deepEqual(h.rows(),[{type:'retiring'}]);assert.equal(h.closes(),0);assert.equal(finished,false);
  release();await h.done;assert.equal(h.rows()[1].id,'one');assert.equal(h.rows()[1].ok,true);assert.equal(h.closes(),1);
});
test('lifetime retirement drains active requests; no helper is closed mid-request',async()=>{
  let release;const gate=new Promise(resolve=>release=resolve);
  const h=harness(async()=>{await gate;return 'done';},{lifetimeMs:10});h.send(request('one'));
  await new Promise(resolve=>setTimeout(resolve,20));assert.equal(h.closes(),0);assert.ok(h.rows().some(r=>r.type==='retiring'));
  release();await h.done;assert.equal(h.closes(),1);assert.equal(h.rows().find(r=>r.id==='one').result,'done');
});
test('malformed and duplicate frames retire without replaying an accepted execute',async()=>{
  for(const tail of [JSON.stringify(request('one'))+'\n','{broken}\n']){
    let calls=0;const h=harness(async()=>{calls++;return 'done';});h.send(request('one'));h.input.write(tail);await h.done;
    assert.equal(calls,1);assert.ok(h.rows().some(r=>r.type==='fatal'));assert.equal(h.closes(),1);
  }
});
test('failed execute is returned once and never internally retried',async()=>{
  let calls=0;const h=harness(async()=>{calls++;throw Object.assign(new Error('secret reply and bearer'),{code:'TRANSPORT_ERROR'});});
  h.send({id:'one',request:{account:'likeavto',operation:'execute'}});h.input.end();await h.done;
  assert.equal(calls,1);const row=h.rows().find(r=>r.id);assert.equal(row.error.code,'TRANSPORT_ERROR');assert.equal(row.error.processRole,'provider-worker');assert.equal(row.error.processId,process.pid);assert.doesNotMatch(JSON.stringify(row),/secret|bearer/);
});
test('nested helper crash retains its own role and PID rather than the surviving worker identity',async()=>{
  for(const metadata of [{processRole:'legacy-process-guard',processId:4321},{}]){
    const h=harness(async()=>{throw Object.assign(new Error('private'),{code:'ADAPTER_PROCESS_FAILED',processExit:{exitCode:7},...metadata});});
    h.send(request('one'));h.input.end();await h.done;const diagnostic=h.rows().find(r=>r.id).error;
    assert.equal(diagnostic.processRole,metadata.processRole);assert.equal(diagnostic.processId,metadata.processId);assert.equal(diagnostic.processExit.exitCode,7);
  }
});
test('oversize input is rejected before dispatch and oversize output retains correlation',async()=>{
  let calls=0;const h=harness(async()=>{calls++;},{maxRequestBytes:64});h.send({...request('one'),extra:'x'.repeat(200)});await h.done;assert.equal(calls,0);
  const out=harness(async()=> 'x'.repeat(2000),{maxResponseBytes:512});out.send(request('one'));out.input.end();await out.done;
  assert.equal(out.rows().find(r=>r.id).error.code,'ADAPTER_OUTPUT_LIMIT');
});
test('queue concurrency is bounded and idle workers close without incoming work',async()=>{
  let active=0,max=0;const h=harness(async()=>{active++;max=Math.max(max,active);await new Promise(resolve=>setImmediate(resolve));active--;return true;},{maxActive:2});
  for(let i=0;i<8;i++)h.send(request('id-'+i));h.input.end();await h.done;assert.equal(max,2);assert.equal(h.rows().filter(r=>r.id).length,8);
  const idle=harness(async()=>{throw new Error('not called');},{idleMs:5});await idle.done;assert.equal(idle.closes(),1);
});

test('response backpressure stops new admission while preserving final-control-late order',{timeout:3000},async()=>{
  const input=new PassThrough(),rows=[],calls=[];let retiring=false,release,observed,closes=0;
  const responseSeen=new Promise(resolve=>{observed=resolve;});
  const output=new Writable({highWaterMark:1,write(chunk,_encoding,callback){
    const row=JSON.parse(String(chunk));rows.push(row);
    if(row.id==='first'){
      release=callback;
      input.write(JSON.stringify(request('late'))+'\n');
      observed();
    }else{if(row.type==='retiring')input.end();callback();}
  }});
  const done=serveProviderSession({account:'likeavto',input,output,session:{
    get retiring(){return retiring;},
    async run(request){calls.push(request.itemId);retiring=true;throw Object.assign(new Error('synthetic'),{code:'TRANSPORT_ERROR'});},
    async close(){closes++;},
  }});
  try{
    input.write(JSON.stringify(request('first'))+'\n');await responseSeen;await new Promise(resolve=>setImmediate(resolve));
    assert.deepEqual(calls,['first'],'late frame must not run while its predecessor response is backpressured');
    assert.equal(closes,0);release();release=null;await done;
    assert.deepEqual(rows.map(row=>row.id??row.type),['first','retiring','late']);
    assert.equal(rows[2].error.code,'PROVIDER_SESSION_RETIRING');assert.equal(closes,1);
  }finally{release?.();input.end();await done.catch(()=>{});}
});

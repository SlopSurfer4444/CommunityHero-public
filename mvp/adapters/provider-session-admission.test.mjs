import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough} from 'node:stream';
import {serveProviderSession} from './provider-session.mjs';

const deferred=()=>{let resolve;const promise=new Promise(r=>{resolve=r;});return {promise,resolve};};
const frame=id=>JSON.stringify({id,request:{account:'likeavto',operation:'execute',itemId:id}})+'\n';
function fixture(t,session,limits={}){
  const input=new PassThrough(),output=new PassThrough(),rows=[];let buffer='';
  output.on('data',chunk=>{buffer+=chunk.toString();let end;while((end=buffer.indexOf('\n'))>=0){rows.push(JSON.parse(buffer.slice(0,end)));buffer=buffer.slice(end+1);}});
  const done=serveProviderSession({account:'likeavto',session,input,output,limits:{maxActive:1,idleMs:1000,lifetimeMs:2000,retirementAckMs:1000,...limits}});
  done.catch(()=>{});t.after(()=>{input.destroy();output.destroy();});
  return {input,output,rows,done};
}
async function tick(){await new Promise(resolve=>setImmediate(resolve));}

test('poisoned resources settle queued frames without entering session.run or replaying',{timeout:3000},async t=>{
  const started=deferred(),release=deferred(),runs=[];let retiring=false,closed=0;
  const h=fixture(t,{get retiring(){return retiring;},async run(req){runs.push(req.itemId);started.resolve();await release.promise;retiring=true;throw Object.assign(new Error('private response text'),{code:'TRANSPORT_ERROR',operation:'read-auth-proactive-refresh-fetch'});},close(){closed++;}});
  h.input.write(frame('first')+frame('queued'));await started.promise;release.resolve();await tick();h.input.end();await h.done;
  assert.deepEqual(runs,['first']);assert.equal(closed,1);
  const rejected=h.rows.find(row=>row.id==='queued');assert.equal(rejected.error.code,'PROVIDER_SESSION_RETIRING');
  assert.deepEqual(rejected.error.sessionAdmission,{version:1,state:'not-started',reason:'retiring'});
  assert.equal(JSON.stringify(h.rows).includes('private response text'),false);
  assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','queued']);
});

test('started mutation sibling settles unchanged while poisoned pending sibling is quarantined',{timeout:3000},async t=>{
  const firstGate=deferred(),secondGate=deferred(),both=deferred(),runs=[];let retiring=false,active=0,closed=0;
  const h=fixture(t,{get retiring(){return retiring;},async run(req){runs.push(req.itemId);active++;if(active===2)both.resolve();try{await (req.itemId==='first'?firstGate.promise:secondGate.promise);if(req.itemId==='first'){retiring=true;throw {code:'HTTP_ERROR',httpStatus:500,operation:'read-http'};}return {status:'unknown',mutationOutcome:'unknown',providerCallAttempted:true,providerRetryAllowed:false};}finally{active--;}} ,close(){assert.equal(active,0);closed++;}},{maxActive:2});
  h.input.write(frame('first')+frame('started')+frame('queued'));await both.promise;firstGate.resolve();await tick();h.input.end();secondGate.resolve();await h.done;
  assert.deepEqual(runs,['first','started']);assert.equal(closed,1);
  assert.deepEqual(h.rows.find(row=>row.id==='started').result,{status:'unknown',mutationOutcome:'unknown',providerCallAttempted:true,providerRetryAllowed:false});
  assert.equal(h.rows.find(row=>row.id==='queued').error.sessionAdmission.state,'not-started');
});

test('graceful EOF drains every accepted queued frame once',{timeout:3000},async t=>{
  const started=deferred(),release=deferred(),runs=[];let closed=0;
  const h=fixture(t,{retiring:false,async run(req){runs.push(req.itemId);if(req.itemId==='first'){started.resolve();await release.promise;}return {itemId:req.itemId};},close(){closed++;assert.equal(runs.length,2);}});
  h.input.write(frame('first')+frame('queued'));await started.promise;h.input.end();release.resolve();await h.done;
  assert.deepEqual(runs,['first','queued']);assert.equal(closed,1);assert.ok(h.rows.filter(row=>row.id).every(row=>row.ok));
});

test('request-budget retirement remains graceful for its previously accepted queue',{timeout:3000},async t=>{
  const release=deferred(),runs=[];let closed=0;
  const h=fixture(t,{retiring:false,async run(req){runs.push(req.itemId);await release.promise;return {};},close(){closed++;}},{maxRequests:2});
  h.input.write(frame('first')+frame('queued'));await tick();assert.equal(h.rows[0].type,'retiring');h.input.end();release.resolve();await h.done;
  assert.deepEqual(runs,['first','queued']);assert.equal(closed,1);
});

test('hostile thrown getter still produces a correlated failure and closes safely',{timeout:3000},async t=>{
  let retiring=false,closed=0;const hostile={get code(){throw Error('secret getter');}};
  const h=fixture(t,{get retiring(){return retiring;},async run(){retiring=true;throw hostile;},close(){closed++;}});
  h.input.end(frame('first'));await h.done;
  const row=h.rows.find(row=>row.id==='first');assert.equal(row.ok,false);assert.equal(row.error.code,'ADAPTER_UNAVAILABLE');assert.equal(closed,1);
  assert.equal(JSON.stringify(h.rows).includes('secret getter'),false);
});

test('late repeated retiring ID cannot produce two correlated settlements',{timeout:3000},async t=>{
  let retiring=false;const h=fixture(t,{get retiring(){return retiring;},async run(){retiring=true;throw {code:'TRANSPORT_ERROR',operation:'read-fetch'};},close(){}});
  h.input.write(frame('first'));await tick();h.input.write(frame('late')+frame('late'));h.input.end();await assert.rejects(h.done,{code:'PROVIDER_SESSION_DUPLICATE_ID'});
  assert.equal(h.rows.filter(row=>row.id==='late').length,1);
  assert.deepEqual(h.rows.find(row=>row.id==='late').error.sessionAdmission,{version:1,state:'not-started',reason:'retiring'});
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';

const fixture=fileURLToPath(new URL('./test-fixtures/provider-session-retirement-worker.mjs',import.meta.url));
const frame=(id,options={})=>JSON.stringify({id,request:{account:'likeavto',operation:'readback',itemId:id,...options}})+'\n';
function start(t,onFrame){
  const child=spawn(process.execPath,[fixture],{stdio:['pipe','pipe','pipe'],windowsHide:true});
  let buffer='',stderr='';const rows=[],stdinErrors=[];
  child.stdin.on('error',error=>stdinErrors.push(error.code));
  child.stdout.on('data',chunk=>{
    buffer+=chunk;let end;
    while((end=buffer.indexOf('\n'))>=0){const line=buffer.slice(0,end);buffer=buffer.slice(end+1);
      const row=JSON.parse(line);rows.push(row);onFrame?.(row,child);
    }
  });
  child.stderr.on('data',chunk=>{stderr+=chunk;});
  const exit=once(child,'close');
  t.after(()=>{if(child.exitCode===null&&child.signalCode===null)child.kill();});
  return {child,rows,stdinErrors,exit,closures:()=>stderr.trim().split('\n').filter(Boolean).map(JSON.parse)};
}

test('real stdio retirement answers a fragmented already-admitted frame before EOF',{timeout:5000},async t=>{
  const late=frame('late');const split=Math.floor(late.length/2);let admitted=false,retiring=false;
  const h=start(t,(row,child)=>{
    if(row.id==='first'){
      assert.equal(row.ok,false);admitted=true;
      // Reserve/write the next request upon the final response, before handling
      // the subsequent retirement control, as the parent can do on this race.
      child.stdin.write(late.slice(0,split));
    }
    if(row.type==='retiring'){
      retiring=true;assert.equal(admitted,true);
      // The tail was already admitted. Complete its frame before parent EOF.
      // Delay ensures an old worker has time to detach stdin and exit cleanly.
      setTimeout(()=>{child.stdin.end(late.slice(split));},40);
    }
  });
  h.child.stdin.write(frame('first',{fail:true}));
  const [code,signal]=await h.exit;assert.equal(code,0);assert.equal(signal,null);assert.equal(retiring,true);
  assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','late']);
  assert.equal(h.rows[2].ok,false);assert.equal(h.rows[2].error.code,'PROVIDER_SESSION_RETIRING');
  assert.deepEqual(h.stdinErrors,[]);assert.deepEqual(h.closures(),[{kind:'fixture-close',active:0,closed:1,runs:['first']}]);
});

test('real stdio EOF acknowledgment drains an accepted sibling before closing shared resources',{timeout:5000},async t=>{
  const h=start(t,(row,child)=>{if(row.type==='retiring')child.stdin.end();});
  h.child.stdin.write(frame('first',{fail:true,delayMs:15})+frame('sibling',{delayMs:80}));
  const [code]=await h.exit;assert.equal(code,0);
  assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','sibling']);
  assert.equal(h.rows[2].ok,true);assert.deepEqual(h.closures(),[{kind:'fixture-close',active:0,closed:1,runs:['first','sibling']}]);
});

test('real stdio missing EOF acknowledgment fails boundedly instead of cleanly dropping possible frames',{timeout:5000},async t=>{
  const h=start(t);h.child.stdin.write(frame('first',{fail:true}));
  const [code,signal]=await h.exit;assert.equal(code,1);assert.equal(signal,null);
  assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','fatal']);
  assert.equal(h.rows[2].error.code,'PROVIDER_SESSION_EOF_TIMEOUT');
  assert.deepEqual(h.closures(),[{kind:'fixture-close',active:0,closed:1,runs:['first']}]);
});

test('real stdio invalid trailing frames cannot masquerade as clean retirement EOF',{timeout:10000},async t=>{
  for(const [tail,code] of [['{"id":"late"','PROVIDER_SESSION_TRUNCATED_FRAME'],['{broken}\n','PROVIDER_SESSION_PROTOCOL'],[frame('first'),'PROVIDER_SESSION_DUPLICATE_ID']]){
    const h=start(t,(row,child)=>{if(row.type==='retiring')child.stdin.end(tail);});
    h.child.stdin.write(frame('first',{fail:true}));const [exitCode,signal]=await h.exit;
    assert.equal(exitCode,1);assert.equal(signal,null);
    assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','fatal']);
    assert.equal(h.rows[2].error.code,code);assert.deepEqual(h.closures(),[{kind:'fixture-close',active:0,closed:1,runs:['first']}]);
  }
});

test('real stdio EOF timeout budget never interrupts a longer accepted sibling',{timeout:5000},async t=>{
  const h=start(t);h.child.stdin.write(frame('first',{fail:true,delayMs:15})+frame('sibling',{delayMs:400}));
  const [code,signal]=await h.exit;assert.equal(code,1);assert.equal(signal,null);
  assert.deepEqual(h.rows.map(row=>row.id??row.type),['first','retiring','sibling','fatal']);
  assert.equal(h.rows[2].ok,true);assert.equal(h.rows[3].error.code,'PROVIDER_SESSION_EOF_TIMEOUT');
  assert.deepEqual(h.closures(),[{kind:'fixture-close',active:0,closed:1,runs:['first','sibling']}]);
});

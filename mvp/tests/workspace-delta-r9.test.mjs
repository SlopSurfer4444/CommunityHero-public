import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {mergeWorkspaceDelta} from '../workshop/workspace-delta.js';
const freeze=value=>{if(value&&typeof value==='object'){Object.freeze(value);Object.values(value).forEach(freeze);}return value;};
const snapshot=()=>({workspaceVersion:'r9:1',operator:{id:'owner'},items:[],approvals:[{id:'a',status:'pending',approvedBy:{id:'owner'}},{id:'b',status:'pending'}]});
const delta=changes=>({kind:'delta',baseVersion:'r9:1',workspaceVersion:'r9:2',actorId:'owner',collections:{},set:{},remove:[],...changes});

test('real browser reducer continues to reconstruct the Rust shared fixture',()=>{
  const fixture=JSON.parse(readFileSync(new URL('./fixtures/workspace-delta-contract.json',import.meta.url),'utf8'));
  assert.deepEqual(mergeWorkspaceDelta(freeze(fixture.base),freeze(fixture.delta)),fixture.current);
});

test('keyed approval change replaces complete row and shares untouched approvals',()=>{
  const before=freeze(snapshot()),row={id:'a',status:'expired'};
  const response=freeze(delta({collections:{approvals:{upsert:[row],remove:[]}}}));
  const next=mergeWorkspaceDelta(before,response);
  assert.deepEqual(next.approvals,[row,before.approvals[1]]);
  assert.equal(next.approvals[1],before.approvals[1]);assert.equal(next.items,before.items);
  assert.equal(next.approvals[0].approvedBy,undefined);assert.equal(before.approvals[0].status,'pending');
});

test('approval removal addition order and optional collection deletion reconstruct exactly',()=>{
  const before=freeze(snapshot()),rows=freeze([{id:'c',status:'new'},before.approvals[0]]);
  const next=mergeWorkspaceDelta(before,freeze(delta({collections:{approvals:{upsert:[rows[0]],remove:['b'],order:['c','a']}}})));
  assert.deepEqual(next.approvals,rows);assert.equal(next.approvals[1],before.approvals[0]);
  const removed=mergeWorkspaceDelta(before,delta({remove:['approvals']}));assert.equal('approvals' in removed,false);
});

test('legacy malformed approval IDs retain exact atomic fallback and full snapshot behavior',()=>{
  for(const approvals of [[{id:'a'},{id:'a'}],[{id:''}],[{id:9}],[{status:'legacy'}]]){
    const before=freeze(snapshot()),nextRows=freeze(approvals);
    const next=mergeWorkspaceDelta(before,freeze(delta({set:{approvals:nextRows}})));
    assert.equal(next.approvals,nextRows);assert.deepEqual(next.approvals,approvals);
    const full=freeze({...snapshot(),workspaceVersion:'restart:0',approvals:nextRows});
    assert.equal(mergeWorkspaceDelta(before,{kind:'full',snapshot:full}),full);
    const replacement=mergeWorkspaceDelta(full,{...delta({set:{approvals:[]}}),baseVersion:'restart:0'});
    assert.deepEqual(replacement.approvals,[]);
  }
});

test('malformed keyed approvals stale identity and conflicting patch fail before publication',()=>{
  const before=freeze(snapshot());
  const bad=[
    delta({actorId:'other',collections:{approvals:{upsert:[{id:'a',status:'executed'}],remove:[]}}}),
    delta({baseVersion:'stale',collections:{approvals:{upsert:[],remove:[]}}}),
    delta({collections:{approvals:{upsert:[{id:'a'},{id:'a'}],remove:[]}}}),
    delta({collections:{approvals:{upsert:[{status:'missing-id'}],remove:[]}}}),
    delta({collections:{approvals:{upsert:[],remove:['unknown'],order:['a','b']}}}),
    delta({collections:{approvals:{upsert:[{id:'c'}],remove:[]}}}),
    delta({collections:{approvals:{upsert:[],remove:[],order:['a','unknown']}}}),
    delta({collections:{approvals:{upsert:[{id:'a'}],remove:[]}},set:{approvals:[]}}),
    delta({collections:{approvals:{upsert:[{id:'a'}],remove:[]}},remove:['approvals']}),
    delta({collections:{approvals:{upsert:[{id:'a',status:'executed'}],remove:[]},items:{upsert:[{id:'c'}],remove:[]}}}),
  ];
  for(const response of bad)assert.throws(()=>mergeWorkspaceDelta(before,freeze(response)),/Invalid workspace delta/);
  assert.equal(before.approvals[0].status,'pending');assert.deepEqual(before.items,[]);
});
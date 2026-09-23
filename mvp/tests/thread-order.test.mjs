import test from 'node:test';
import assert from 'node:assert/strict';
import {chronologicalSiblings} from '../workshop/thread-order.js';
test('brand reply precedes later selected response without inventing parent links',()=>{
  const user={id:'target',parentId:'root',createdAt:'2026-09-21T16:20:00Z'};
  const brand={id:'brand',parentId:'root',createdAt:'2026-09-21T15:41:00Z'};
  const original=[user,brand],ordered=chronologicalSiblings(original);
  assert.deepEqual(ordered,[brand,user]);assert.deepEqual(original,[user,brand]);
  assert.ok(ordered.every(m=>m.parentId==='root'));
});
test('full dates and timezones determine order, unknown and equal dates stay stable',()=>{
  const rows=[{id:'later',createdAt:'2026-09-22T00:01:00+03:00'},{id:'unknown',time:'00:00'},
    {id:'earlier',createdAt:'2026-09-21T20:59:00Z'},{id:'same',createdAt:'2026-09-21T23:59:00+03:00'}];
  assert.deepEqual(chronologicalSiblings(rows).map(m=>m.id),['earlier','unknown','same','later']);
});

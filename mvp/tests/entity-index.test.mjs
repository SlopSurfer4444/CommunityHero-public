import test from 'node:test';
import assert from 'node:assert/strict';
import {createEntityIndex} from '../workshop/entity-index.js';

test('entity lookups survive bootstrap replacement, refresh and local field edits', () => {
  let data = {items:[], branches:[], posts:[]};
  const index = createEntityIndex(() => data);
  assert.equal(index.item('a'), undefined);
  const first = {id:'a',targetId:'m',draft:'old'};
  data = {items:[first,{id:'b',targetId:'m'}],branches:[{id:'branch'}],posts:[{id:'post'}]};
  assert.equal(index.item('a'), first);
  assert.equal(index.messageOwner('m'), first);
  first.draft = 'edited';
  assert.equal(index.item('a').draft, 'edited');
  data.items = [{id:'a',targetId:'n',draft:'fresh'}];
  assert.equal(index.item('a').draft, 'fresh');
  assert.equal(index.messageOwner('m'), undefined);
  assert.equal(index.messageOwner('n').id, 'a');
  data.items.push({id:'c',targetId:'o'});
  assert.equal(index.item('c').targetId, 'o');
  assert.equal(index.branch('branch'), data.branches[0]);
  assert.equal(index.post('post'), data.posts[0]);
});

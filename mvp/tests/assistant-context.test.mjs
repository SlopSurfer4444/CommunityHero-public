import test from 'node:test';
import assert from 'node:assert/strict';
import {buildAssistantContext} from '../workshop/assistant-context.js';

test('screen snapshot cannot drift to another recipient after navigation',()=>{
  const input={kind:'comment',itemId:'selected',itemIds:['selected','sibling'],filters:{postId:'post'}};
  const frozen=buildAssistantContext(input);
  input.itemId='other';input.itemIds.push('other');input.filters.postId='other-post';
  assert.deepEqual(frozen.itemIds,['selected','sibling']);
  assert.equal(frozen.screen.selectedItemId,'selected');assert.equal(frozen.screen.filters.postId,'post');
  assert.ok(Object.isFrozen(frozen.screen));assert.ok(Object.isFrozen(frozen.screen.itemIds));
});
test('large queue reports partial attachment and preserves displayed ordering',()=>{
  const context=buildAssistantContext({kind:'queue',itemIds:Array.from({length:30},(_,i)=>`item-${i}`),totalCount:90,order:'oldest'});
  assert.equal(context.itemIds.length,20);assert.equal(context.itemIds.at(-1),'item-19');
  assert.equal(context.screen.totalCount,90);assert.equal(context.screen.visibleItemCount,30);assert.equal(context.screen.truncated,true);
});
test('empty overview is described without claiming unseen comment evidence',()=>{
  const context=buildAssistantContext({kind:'analytics',label:'Аналитика',totalCount:180,filters:{period:'week',secret:'private'}});
  assert.deepEqual(context.itemIds,[]);assert.equal(context.screen.kind,'analytics');
  assert.deepEqual(context.screen.filters,{period:'week'});assert.equal(context.screen.truncated,true);
});

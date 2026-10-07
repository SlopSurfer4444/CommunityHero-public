import test from 'node:test';
import assert from 'node:assert/strict';
import {repairBrandRoles} from '../adapters/repair-brand-roles.mjs';
const binding={accountId:'LikeAvto',connector:'angryspace',id:'angryspace-likeavto-v1',providerAccountId:'likeavto',revision:1,workspaceId:'local-pilot'};
test('offline brand repair is scoped to verified identities and preserves workflow content',()=>{
  const good={id:'brand',providerItemId:'source',providerObjectId:'11341',authorId:'provider:vk_-135891342',role:'participant'};
  const d={account:'LikeAvto',connectorBinding:binding,items:[{branchId:'b',postId:'p',objectId:'11341',draft:'manual edit',contextVersion:8}],proposals:[{reply:'unchanged'}],branches:[{id:'b',postId:'p',messages:[good,{...good,id:'spoof',authorId:'other',author:'LikeAvto'}, {...good,id:'other-object',providerObjectId:'11391'}],observedMessages:[{...good}]}]};
  const {workspace,changes}=repairBrandRoles(d);
  assert.equal(changes.length,2);assert.equal(workspace.branches[0].messages[0].role,'brand');
  assert.equal(workspace.branches[0].messages[1].role,'participant');assert.equal(workspace.branches[0].messages[2].role,'participant');
  assert.equal(d.branches[0].messages[0].role,'participant');assert.deepEqual(workspace.items,d.items);assert.deepEqual(workspace.proposals,d.proposals);
  assert.equal(repairBrandRoles(workspace).changes.length,0);
  assert.throws(()=>repairBrandRoles({...d,connectorBinding:{...binding,revision:2}}),/SCOPE_MISMATCH/);
});


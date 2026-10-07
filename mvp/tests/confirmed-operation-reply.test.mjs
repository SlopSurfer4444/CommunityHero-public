import test from 'node:test';
import assert from 'node:assert/strict';
import {confirmedOperationReply,operationReplyText} from '../workshop/mvp-connection.js';

const binding={id:'connection-1',workspaceId:'workspace-1',accountId:'BAW Russia',connector:'angryspace',revision:2,providerAccountId:'baw'};
const item={id:'item-1',itemId:'comment-1',providerItemId:'comment-1',objectId:'object-1',postKey:'object-1:post-1',
  conversationKey:'object-1:thread-1',branchId:'branch-1',targetId:'target-1',providerStatus:'closed',workflow:'closed',connectorBinding:binding};
const target={id:'target-1',providerItemId:'comment-1',providerObjectId:'object-1',role:'customer'};
const operation={id:'operation-1',itemId:'item-1',status:'succeeded',updatedAt:'2026-09-24T10:00:00Z',target:{...item},
  action:{action:'reply_and_close',objectId:'object-1',itemId:'comment-1',conversationKey:'object-1:thread-1',
    reply:'Точный опубликованный ответ',readbackEvidence:{baselineReplyIds:['old-reply']}}};
const snapshot=(op=operation,messages=[target])=>({account:'BAW Russia',connectorBinding:binding,operations:[op],branches:[{id:'branch-1',messages}]});

test('confirmed reply is a separate read-only projection of an exact same-account operation',()=>{
  const before=structuredClone(snapshot()),record=confirmedOperationReply(before,item);
  assert.deepEqual(record,{operationId:'operation-1',text:'Точный опубликованный ответ',at:'2026-09-24T10:00:00Z'});
  assert.deepEqual(before,snapshot(),'projection does not write synthetic branch messages');
  assert.equal(operationReplyText(operation),'Точный опубликованный ответ');
  assert.equal(operationReplyText({action:{text:'Старый формат'}}),'Старый формат');
});

test('different account, connector revision or recipient cannot publish operation text in this conversation',()=>{
  const changed=[
    {snapshot:{...snapshot(),account:'LikeAvto'}},
    {snapshot:{...snapshot(),connectorBinding:{...binding,accountId:'LikeAvto'}},item:{...item,connectorBinding:{...binding,accountId:'LikeAvto'}}},
    {item:{...item,connectorBinding:{...binding,revision:3}}},
    {item:{...item,objectId:'other-object'}},
    {item:{...item,itemId:'other-comment'}},
    {item:{...item,conversationKey:'other-thread'}},
    {op:{...operation,itemId:'other-item'}},
    {op:{...operation,action:{...operation.action,itemId:'other-comment'}}}
  ];
  for(const entry of changed)assert.equal(confirmedOperationReply(entry.snapshot||snapshot(entry.op),entry.item||item),null);
});

test('unknown, failed, close-only and reopened work never present a published reply',()=>{
  for(const status of ['unknown','failed','dispatching'])
    assert.equal(confirmedOperationReply(snapshot({...operation,status}),item),null);
  assert.equal(confirmedOperationReply(snapshot({...operation,action:{...operation.action,action:'close'}}),item),null);
  assert.equal(confirmedOperationReply(snapshot(),{...item,providerStatus:'new',workflow:'attention'}),null);
  assert.equal(confirmedOperationReply(snapshot({...operation,action:{...operation.action,reply:''}}),item),null);
});

test('provider observed direct new reply replaces operation display; old or unrelated reply does not',()=>{
  const reply=(changes={})=>({id:'official-new',providerItemId:'new-reply',providerObjectId:'object-1',
    replyToProviderItemId:'comment-1',parentId:'target-1',role:'brand',text:'Точный опубликованный ответ',...changes});
  assert.equal(confirmedOperationReply(snapshot(operation,[target,reply()]),item),null);
  for(const changed of [{providerItemId:'old-reply'},{providerObjectId:'other-object'},
    {replyToProviderItemId:'other-comment',parentId:'other-target'},{role:'customer'},{deleted:true}])
    assert.ok(confirmedOperationReply(snapshot(operation,[target,reply(changed)]),item));
  const exact={...operation,action:{...operation.action,readbackEvidence:{expectedReplyId:'new-reply'}}};
  assert.equal(confirmedOperationReply(snapshot(exact,[target,reply()]),item),null);
  assert.ok(confirmedOperationReply(snapshot(exact,[target,reply({providerItemId:'different-reply'})]),item));
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,assistantInstructions,reviewInstructions} from './assistant.mjs';
import {safeError} from './bridge.mjs';

const alias={namespace:'commentops-fast.post-key',value:'legacy:post'};
const binding={postKey:'legacy:post',match:'legacy_connector_scoped_alias',namespace:alias.namespace};
function fixture(kind='reference') {
  return {account:'LikeAvto',purpose:'triage',
    connectorBinding:{connector:'angryspace',accountId:'LikeAvto',providerAccountId:'likeavto'},
    items:[{id:'comment',postKey:'legacy:post'}],posts:[{id:'post',postKey:'legacy:post'}],
    materials:[{id:'material',kind,postKey:'legacy:post',trust:'source_only',text:'An imported source claim',
      knowledgeEntryId:'entry',knowledgeVersionId:'version',sourceAssertion:true,legacyPostAliases:[structuredClone(alias)],
      companyImport:{companyKey:'likeavto',scope:{companyKey:'likeavto',postAliases:[structuredClone(alias)]}}}],
    knowledgeManifest:[{entryId:'entry',versionId:'version',kind,trust:'source_only',mediaBinding:[structuredClone(binding)]}]};
}

test('engine-selected imported aliases survive exact source/version/account/connector binding',()=>{
  for(const kind of ['reference','transcript','ocr']) {
    const req=fixture(kind);req.knowledgeManifest[0].mediaBinding[0].private='drop';
    const {payload,input}=prepareAssistantRequest(req);
    assert.deepEqual(payload.knowledgeManifest[0].mediaBinding,[binding]);
    assert.equal(payload.materials[0].kind,kind);
    assert.equal(payload.materials[0].trust,'source_only');
    assert.equal(payload.materials[0].postKey,'legacy:post');
    assert.equal(payload.materials[0].transcription,undefined);
    assert.equal(payload.items[0].objectId,undefined);
    assert.equal(input.includes('drop'),false);
  }
  for(const instructions of [assistantInstructions(true),reviewInstructions()]) {
    assert.match(instructions,/not shared\nmedia identity, a native platform ID, verified fact or proof of video coverage/);
  }
});

test('legacy attribution cannot migrate connector, account, source/version, target or trust',()=>{
  const mutations=[
    r=>delete r.connectorBinding,
    r=>r.connectorBinding.connector='vk',
    r=>r.connectorBinding.accountId='BAW Russia',
    r=>r.connectorBinding.providerAccountId='baw-russia',
    r=>r.materials[0].companyImport.companyKey='baw-russia',
    r=>r.materials[0].companyImport.scope.companyKey='baw-russia',
    r=>delete r.materials[0].companyImport,
    r=>delete r.materials[0].legacyPostAliases,
    r=>r.materials[0].legacyPostAliases[0].value='other:post',
    r=>r.materials[0].companyImport.scope.postAliases[0].namespace='vk.native-post',
    r=>r.materials[0].sourceAssertion=false,
    r=>r.materials[0].postKey='other:post',
    r=>r.materials[0].knowledgeEntryId='other-entry',
    r=>r.materials[0].knowledgeVersionId='other-version',
    r=>r.materials[0].trust='verified',
    r=>r.knowledgeManifest[0].trust='verified',
    r=>r.knowledgeManifest[0].mediaBinding[0].namespace='vk.native-post',
    r=>r.knowledgeManifest[0].mediaBinding[0].postKey='other:post',
    r=>r.knowledgeManifest[0].mediaBinding[0].identities=['yt:AbCdEf123_-'],
    r=>r.knowledgeManifest[0].mediaBinding[0].sourcePostKey='legacy:post',
    r=>r.knowledgeManifest[0].mediaBinding[0].authorization='account_scoped_exact_title_reuse',
    r=>{r.posts=[];r.items=[{id:'comment'}];},
  ];
  for(const mutate of mutations) {
    const req=fixture();mutate(req);
    assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'LEGACY_POST_BINDING'});
  }
  const req=fixture('rule');
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_BINDING'});
});

test('reference evidence cannot acquire cross-post media reuse semantics',()=>{
  const req=fixture();req.knowledgeManifest[0].mediaBinding=[{
    postKey:'legacy:post',sourcePostKey:'legacy:post',match:'exact_normalized_title',
    normalizedTitle:'matching title',authorization:'account_scoped_exact_title_reuse',identities:[]}];
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST',requestCategory:'MEDIA_BINDING'});
});

test('binding diagnostics cross the adapter boundary only as fixed non-sensitive codes',()=>{
  for(const requestCategory of ['MEDIA_BINDING','LEGACY_POST_BINDING']) {
    const code=`ASSISTANT_INVALID_REQUEST_${requestCategory}`;
    assert.deepEqual(safeError({code:'ASSISTANT_INVALID_REQUEST',requestCategory,message:'private comment',request:fixture()}),{ok:false,error:{code,message:code}});
    assert.equal(safeError({code}).error.code,code);
  }
  for(const requestCategory of ['PRIVATE_TEXT',null,{toString:()=> 'MEDIA_BINDING'}]) {
    assert.equal(safeError({code:'ASSISTANT_INVALID_REQUEST',requestCategory}).error.code,'ASSISTANT_INVALID_REQUEST');
  }
  assert.equal(safeError({code:'ASSISTANT_INVALID_REQUEST_PRIVATE_TEXT'}).error.code,'ASSISTANT_INVALID_REQUEST');
  assert.equal(safeError({code:'ASSISTANT_FAILED',requestCategory:'MEDIA_BINDING'}).error.code,'ASSISTANT_FAILED');
  const req=fixture();req.connectorBinding.connector='vk';
  assert.throws(()=>prepareAssistantRequest(req),error=>{
    assert.equal(safeError(error).error.code,'ASSISTANT_INVALID_REQUEST_LEGACY_POST_BINDING');return true;
  });
});

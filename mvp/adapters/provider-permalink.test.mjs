import assert from 'node:assert/strict';
import test from 'node:test';
import {projectContext,mapRead} from './provider.mjs';
import {fastConveyorNativeUrl} from '../connectors/angryspace-provider/src/provider/native-permalink.ts';
import {fastConveyorPublicSourceUrl} from '../connectors/angryspace-provider/src/transport/fast-conveyor-gateway.ts';

const helpers={fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl,fastConveyorNativeUrl,fastConveyorAuthorId:()=>undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)};
test('native actor locators survive projection separately from canonical publication and opaque bindings',()=>{
  for(const fixture of [
    {provider:'youtube',objectId:'11390',post:'https://www.youtube.com/watch?v=kMlxz6Kshh8',target:'https://www.youtube.com/watch?v=kMlxz6Kshh8&lc=target',reply:'https://www.youtube.com/watch?v=kMlxz6Kshh8&lc=target.reply'},
    {provider:'vk',objectId:'11341',post:'https://vk.ru/wall-135891342_58939',target:'https://vk.ru/wall-135891342_58939?reply=60163',reply:'https://vk.ru/wall-135891342_58939?reply=60176&thread=60163'}
  ]){
    const context={item:{id:'opaque-target',provider:fixture.provider,status:'closed',text:'Customer',url:fixture.target,reply_to_item_id:'opaque-parent'},replyTo:{id:'opaque-parent',text:'Previous',url:fixture.target},parent:{id:'opaque-post',url:fixture.post,text:'Post'},officialReplies:[{id:'opaque-reply',official:1,text:'Brand',url:fixture.reply,reply_to_item_id:'opaque-target'}]};
    const row=projectContext(context,fixture.objectId,helpers,'2026-10-01T00:00:00Z');
    const mapped=mapRead([row],{});
    assert.equal(row.nativeUrl,fixture.target);assert.equal(mapped.items[0].nativeUrl,fixture.target);
    assert.equal(mapped.posts[0].sourceUrl,fixture.post);assert.equal(mapped.items[0].sourceUrl,fixture.post);
    assert.deepEqual(mapped.branches[0].messages.map(message=>message.nativeUrl),[fixture.target,fixture.target,fixture.reply]);
    assert.deepEqual(mapped.branches[0].messages.map(message=>message.providerItemId),['opaque-parent','opaque-target','opaque-reply']);
    assert.equal(mapped.items[0].conversationKey,`${fixture.objectId}:opaque-parent`);
    assert.equal(mapped.branches[0].contextComplete,false);
    assert.equal(context.officialReplies[0].url,fixture.reply);
  }
});
test('missing or unsafe native URLs remain absent rather than inferred from IDs/post URL',()=>{
  const context={item:{id:'target',provider:'youtube',status:'new',text:'Customer',url:'https://www.youtube.com/watch?v=kMlxz6Kshh8&lc=c&token=private'},parent:{id:'post',url:'https://www.youtube.com/watch?v=kMlxz6Kshh8',text:'Post'},officialReplies:[{id:'reply',text:'Brand',url:'https://evil.test/reply'}]};
  const mapped=mapRead([projectContext(context,'11390',helpers)],{});
  assert.equal(Object.hasOwn(mapped.items[0],'nativeUrl'),false);
  assert.ok(mapped.branches[0].messages.every(message=>!Object.hasOwn(message,'nativeUrl')));
  assert.doesNotMatch(JSON.stringify(mapped),/private|evil\.test/);
});

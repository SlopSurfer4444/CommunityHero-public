import test from 'node:test';
import assert from 'node:assert/strict';
import {runMediaSource} from './media-source.mjs';

const scope=async()=>({binding:{accountKey:'likeavto',displayName:'LikeAvto',objectIds:['object-a']}});
const post={id:'post-native-opaque',postKey:'native-opaque',objectId:'object-a',title:'Video',
  sourceUrl:'https://vk.com/wall-1_2',attachments:[{type:'video',url:'https://www.youtube.com/watch?v=AbCdEf123_-&si=secret'}]};

test('connector projects a scoped public source without tracking parameters',async()=>{
  const projected=await runMediaSource({account:'likeavto',postId:post.id,post},{validateScopeFn:scope});
  assert.equal(projected.sourceUrl,'https://www.youtube.com/watch?v=AbCdEf123_-');
  assert.equal(projected.postKey,post.postKey);
  assert.equal(projected.account,'LikeAvto');
});

test('connector rejects another object and unsafe source URLs',async()=>{
  await assert.rejects(runMediaSource({account:'likeavto',postId:post.id,post:{...post,objectId:'foreign'}},{validateScopeFn:scope}),/MEDIA_POST_INVALID/);
  for(const sourceUrl of ['http://vk.com/wall-1_2','https://vk.com.evil.test/wall-1_2','https://u:p@vk.com/wall-1_2']){
    await assert.rejects(runMediaSource({account:'likeavto',postId:post.id,post:{...post,sourceUrl,attachments:[]}},{validateScopeFn:scope}),/MEDIA_SOURCE_MISSING/);
  }
});

test('VK page fallback stays connector-owned and rejects a private-media locator',async()=>{
  const vk={...post,attachments:[],sourceUrl:'https://vk.com/video-1_2'};
  const result=await runMediaSource({account:'likeavto',postId:vk.id,post:vk},{validateScopeFn:scope,
    fetchVkPageFn:async()=>`<meta property="og:video" content="https://cdn.vkuseraudio.net/video.mp4?key=a&amp;b=c">`});
  assert.equal(result.fallbackUrl,'https://cdn.vkuseraudio.net/video.mp4?key=a&b=c');
  const rejected=await runMediaSource({account:'likeavto',postId:vk.id,post:vk},{validateScopeFn:scope,
    fetchVkPageFn:async()=>`<meta property="og:video" content="https://127.0.0.1/private.mp4">`});
  assert.equal(rejected.fallbackUrl,'');
});

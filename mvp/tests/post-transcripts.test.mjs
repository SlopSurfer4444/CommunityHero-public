import test from 'node:test';
import assert from 'node:assert/strict';
import {mediaUrlIdentity,mediaTitle,postTranscripts,equivalentMediaPosts} from '../workshop/post-transcripts.js';
const url='https://www.youtube.com/watch?v=abcdefghijk';
const material={id:'t',kind:'transcript',postKey:'yt',sourceUrl:url,text:'Existing transcript'};
test('existing transcript follows exact post or confirmed video identity across platforms',()=>{
  const yt={postKey:'yt',sourceUrl:url},vk={postKey:'vk',sourceUrl:'https://vk.com/video-123_456',attachments:[{type:'video',url:'https://youtu.be/abcdefghijk'}]};
  assert.equal(postTranscripts(yt,[yt,vk],[material])[0].shared,false);
  assert.equal(postTranscripts(vk,[yt,vk],[material])[0].match,'yt:abcdefghijk');
  const sha='a'.repeat(64);
  assert.equal(postTranscripts({...vk,attachments:[],mediaSha256:sha},[{...yt,contentSha256:sha}],[material])[0].shared,true);
});
test('similar titles, thumbnails, unrelated videos and foreign accounts cannot share transcripts',()=>{
  const source={postKey:'yt',title:'Same title',sourceUrl:url};
  for(const post of [{postKey:'vk',title:'Same title'},{postKey:'vk',thumbnailUrl:url},{postKey:'vk',sourceUrl:'https://youtu.be/xxxxxxxxxxx'},{postKey:'yt',account:'Other'}])assert.deepEqual(postTranscripts(post,[source],[material]),[]);
  assert.deepEqual(postTranscripts({postKey:'vk',sourceUrl:url},[],[{...material,account:'Other'}]),[]);
  assert.deepEqual(postTranscripts({postKey:'vk',sourceUrl:url},[],[{...material,sourceMediaScope:{account:'Other'}}]),[]);
  assert.deepEqual(postTranscripts({postKey:'vk',attachments:[{type:'video',url,account:'Other'}]},[],[material]),[]);
});
test('canonical URLs reject deceptive hosts, credentials, ports and ambiguous paths',()=>{
  assert.equal(mediaUrlIdentity('https://youtube.com/shorts/abcdefghijk'),'yt:abcdefghijk');
  assert.equal(mediaUrlIdentity('https://vkvideo.ru/video-123_456'),'vk:-123_456');
  for(const value of ['https://youtube.com.evil/watch?v=abcdefghijk','https://user@youtube.com/watch?v=abcdefghijk','https://youtube.com:9000/watch?v=abcdefghijk','https://youtu.be/abcdefghijk/extra','https://youtube.com/watch?v=abcdefghijk&v=xxxxxxxxxxx'])assert.equal(mediaUrlIdentity(value),null);
});
test('shared media does not create text or duplicate identical transcript copies',()=>{
  assert.deepEqual(postTranscripts({postKey:'p'},[],[]),[]);
  assert.equal(postTranscripts({postKey:'yt'},[],[material,{...material,id:'copy'}]).length,1);
});
test('LikeAvto exact title reuses across platforms with caption fallback and trailing tags',()=>{
  const yt={postKey:'yt-failed',title:'  Бюджетный электрический кроссовер — Changan Q05 #лайкавто #автоимпорт',sourceUrl:url};
  const ig={postKey:'ig-success',title:'Публикация LikeAvto',text:'Бюджетный   электрический кроссовер — Changan Q05\nOther caption',sourceUrl:'https://instagram.com/reel/AAAA/'};
  const t={...material,postKey:ig.postKey,sourceUrl:ig.sourceUrl};
  assert.equal(mediaTitle(yt),mediaTitle(ig));
  assert.equal(postTranscripts(yt,[yt,ig],[t])[0].match,'title:'+mediaTitle(yt));
  assert.deepEqual(postTranscripts({...yt,title:'Бюджетный электрический кроссовер — Changan Q06'},[ig],[t]),[]);
  assert.deepEqual(postTranscripts(yt,[{...ig,account:'Other'}],[t]),[]);
  assert.deepEqual(postTranscripts(yt,[ig],[t],'Other'),[]);
});
test('generic and blank video titles and explicit conflicting identities cannot merge',()=>{
  const a={postKey:'a',title:'Video by likeavto_import',sourceUrl:url},b={postKey:'b',title:'Video by likeavto_import',sourceUrl:'https://instagram.com/reel/BBBB/'};
  const t={...material,postKey:'b',sourceUrl:b.sourceUrl};
  for(const title of ['', ' ', 'None','Video by likeavto_import','Clip by @likeavto_import'])assert.deepEqual(postTranscripts({...a,title},[b],[t]),[]);
  const x={...a,title:'Same exact video',mediaSha256:'a'.repeat(64)},y={...b,title:'Same exact video',contentSha256:'b'.repeat(64)};
  assert.deepEqual(postTranscripts(x,[y],[t]),[]);
  assert.deepEqual(postTranscripts({...x,mediaSha256:undefined,canonicalMediaId:'first'},[{...y,contentSha256:undefined,canonicalMediaId:'second'}],[t]),[]);
  assert.deepEqual(postTranscripts({...a,title:x.title},[x,y],[t]),[]);
});
test('equivalent sibling helper shares media rules without leaking conflicting or foreign previews',()=>{
  const target={postKey:'target',title:'Exact title',sourceUrl:url};
  const twin={postKey:'twin',title:'exact title #tag',sourceUrl:'https://instagram.com/reel/BBBB/'},foreign={...twin,postKey:'foreign',account:'Other'};
  assert.deepEqual(equivalentMediaPosts(target,[target,twin,foreign]),[twin]);
  assert.deepEqual(equivalentMediaPosts(target,[target,{...twin,mediaSha256:'a'.repeat(64)},{...twin,postKey:'conflict',mediaSha256:'b'.repeat(64)}]),[]);
});
test('one canonical transcript is identical on each platform and preserves all source records',()=>{
  const posts=[{postKey:'yt',title:'Suzuki Jimny',sourceUrl:url},{postKey:'ig',title:'Suzuki Jimny #tag',sourceUrl:'https://instagram.com/reel/Jimny/'},{postKey:'vk',title:'Suzuki Jimny',attachments:[{type:'video'}]}];
  const materials=posts.map((p,i)=>({id:'t'+i,kind:'transcript',postKey:p.postKey,text:'ASR version '+i,updatedAt:'2026-09-22T00:00:00Z',transcription:{partial:true,maxAudioSeconds:900}}));
  materials[1].transcription={partial:false,maxAudioSeconds:1200};
  materials[2].updatedAt='2026-09-23T00:00:00Z';
  for(const post of posts){const selected=postTranscripts(post,posts,materials);assert.equal(selected.length,1);assert.equal(selected[0].id,'t1');}
  assert.equal(materials.length,3);
  delete materials[1].transcription;
  assert.equal(postTranscripts(posts[0],posts,materials)[0].id,'t2');
});

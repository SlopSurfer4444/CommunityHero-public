import test from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {createHash} from 'node:crypto';
import {runMediaSource,vkPage} from './media-source.mjs';

const scope=async()=>({binding:{accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto',objectIds:['object-a']}});
const post={id:'post-native-opaque',postKey:'native-opaque',objectId:'object-a',title:'Video',
  sourceUrl:'https://vk.com/wall-1_2',attachments:[{type:'video',url:'https://www.youtube.com/watch?v=AbCdEf123_-&si=secret'}]};
const vkPost={...post,attachments:[],sourceUrl:'https://vk.com/video-1_2'};
const vkRequest=()=>({account:'likeavto',postId:vkPost.id,post:vkPost});
function pageRequests(sequence,inspect=()=>{}){
  const calls=[];
  const request=(url,options,callback)=>{
    const req=new EventEmitter();let timeout;
    req.setTimeout=(milliseconds,fn)=>{assert.equal(milliseconds,45000);timeout=fn;};
    req.destroy=error=>req.emit('error',error);
    req.end=()=>queueMicrotask(()=>{
      calls.push(url.href);inspect(url,options);
      const next=sequence.shift();assert.ok(next,'unexpected extra page request');
      if(next.timeout){timeout();return;}
      if(next.error){req.emit('error',next.error);return;}
      const res=new EventEmitter();res.statusCode=next.status??200;res.headers=next.headers??{};res.resume=()=>{};res.destroy=()=>{};
      callback(res);
      if(next.aborted){res.emit('aborted');return;}
      if(next.body!==undefined)res.emit('data',Buffer.from(next.body));res.emit('end');
    });return req;
  };return {request,calls};
}
function scopedPage(options){return runMediaSource(vkRequest(),{validateScopeFn:scope,fetchVkPageFn:url=>vkPage(url,{lookup:async()=>[{address:'8.8.8.8',family:4}],...options})});}

test('connector projects a scoped public source without tracking parameters',async()=>{
  const projected=await runMediaSource({account:'likeavto',postId:post.id,post},{validateScopeFn:scope});
  assert.equal(projected.sourceUrl,'https://www.youtube.com/watch?v=AbCdEf123_-');
  assert.equal(projected.postKey,post.postKey);
  assert.equal(projected.account,'LikeAvto');
  assert.equal(projected.sourceDiscovery,undefined,'non-VK result remains unchanged');
});

test('VK embed source retains only extractor-required query fields',async()=>{
  const vk={...post,attachments:[{type:'video',url:'https://vkvideo.ru/video_ext.php?oid=-1234567890&id=123456789&hash=0123456789abcdef&__ref=secret&api_hash=private'}]};
  const projected=await runMediaSource({account:'likeavto',postId:vk.id,post:vk},{validateScopeFn:scope,fetchVkPageFn:async()=>''});
  assert.equal(projected.sourceUrl,'https://vkvideo.ru/video_ext.php?oid=-1234567890&id=123456789&hash=0123456789abcdef');
  assert.equal(projected.fallbackUrl,'');
  assert.deepEqual(projected.sourceDiscovery,{schemaVersion:1,platform:'vk',status:'no_supported_locator',category:null,stage:'locator_parse'});
});

test('VK embed source rejects malformed or ambiguous identity fields',async()=>{
  for(const query of ['oid=-1&id=2&id=3','oid=-1&id=2&hash=bad','oid=oops&id=2','oid=-1','oid=-1&id=2&hash=12345678&hash=abcdefgh']){
    const vk={...post,sourceUrl:'',attachments:[{type:'video',url:`https://vkvideo.ru/video_ext.php?${query}`}]};
    await assert.rejects(runMediaSource({account:'likeavto',postId:vk.id,post:vk},{validateScopeFn:scope}),/MEDIA_SOURCE_MISSING/);
  }
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
  assert.equal(result.sourceDiscovery.status,'fallback_resolved');
  assert.equal(JSON.stringify(result.sourceDiscovery).includes('key='),false);
  const rejected=await runMediaSource({account:'likeavto',postId:vk.id,post:vk},{validateScopeFn:scope,
    fetchVkPageFn:async()=>`<meta property="og:video" content="https://127.0.0.1/private.mp4">`});
  assert.equal(rejected.fallbackUrl,'');
  assert.equal(rejected.sourceDiscovery.status,'no_supported_locator');
});

test('VK HTTP failures keep closed discovery categories without changing primary source or fallback behavior',async()=>{
  for(const [status,category] of [[401,'auth_required'],[403,'http_forbidden'],[429,'rate_limited'],[404,'unavailable'],[410,'unavailable'],[503,'network'],[418,'http_failed']]){
    const fixture=pageRequests([{status,headers:{'set-cookie':'PRIVATE'},body:'PRIVATE HTML'}]);
    const projected=await scopedPage({request:fixture.request});
    assert.equal(projected.sourceUrl,vkPost.sourceUrl);assert.equal(projected.postKey,vkPost.postKey);assert.equal(projected.account,'LikeAvto');assert.equal(projected.fallbackUrl,'');
    assert.deepEqual(projected.sourceDiscovery,{schemaVersion:1,platform:'vk',status:'failed',category:`${category}`,stage:'http'});
    assert.equal(JSON.stringify(projected.sourceDiscovery).includes('PRIVATE'),false);assert.equal(fixture.calls.length,1,'no new retry');
  }
});

test('VK discovery distinguishes timeout, network, TLS, size and incomplete response without raw errors',async()=>{
  for(const [response,category] of [[{timeout:true},'timeout'],[{error:Object.assign(new Error('PRIVATE signed locator'),{code:'ECONNRESET'})},'network'],
    [{error:Object.assign(new Error('PRIVATE certificate'),{code:'CERT_HAS_EXPIRED'})},'tls'],
    [{headers:{'content-length':String(12*1024*1024+1)}},'too_large'],[{body:Buffer.alloc(12*1024*1024+1)},'too_large'],[{aborted:true},'incomplete_response']]){
    const fixture=pageRequests([response]);const projected=await scopedPage({request:fixture.request});
    assert.equal(projected.sourceDiscovery.category,`${category}`);assert.equal(projected.sourceDiscovery.stage,'http');assert.equal(projected.fallbackUrl,'');
    assert.equal(JSON.stringify(projected.sourceDiscovery).includes('PRIVATE'),false);assert.equal(fixture.calls.length,1);
  }
});

test('VK discovery keeps public DNS/socket pinning and distinguishes DNS failure from unsafe admission',async()=>{
  const fixture=pageRequests([{body:'No supported video'}],(_url,options)=>{
    options.lookup('ignored',{all:false},(error,address,family)=>{assert.equal(error,null);assert.equal(address,'8.8.8.8');assert.equal(family,4);});
    assert.equal(options.agent,false);assert.equal(options.headers.Cookie,undefined);assert.equal(options.headers.Authorization,undefined);
  });
  assert.equal((await scopedPage({request:fixture.request})).sourceDiscovery.status,'no_supported_locator');
  const mixed=await scopedPage({request:()=>assert.fail('private DNS must not open a request'),lookup:async()=>[{address:'8.8.8.8'},{address:'127.0.0.1'}]});
  assert.equal(mixed.sourceDiscovery.category,'invalid_source');assert.equal(mixed.sourceDiscovery.stage,'dns');
  const failed=await scopedPage({request:()=>assert.fail('failed DNS must not open a request'),lookup:async()=>{throw Object.assign(new Error('PRIVATE DNS'),{code:'EAI_AGAIN'});}});
  assert.equal(failed.sourceDiscovery.category,'network');assert.equal(failed.sourceDiscovery.stage,'dns');
});

test('VK redirects stay provider-bound and bounded, with closed rejection and limit categories',async()=>{
  for(const location of ['https://vk.com.evil.test/private?token=PRIVATE','https://127.0.0.1/private','http://vk.com/video-1_2',undefined,'https://vk.com:444/private']){
    const fixture=pageRequests([{status:302,headers:{location}}]);const projected=await scopedPage({request:fixture.request});
    assert.equal(projected.sourceDiscovery.category,'redirect_rejected');assert.equal(projected.sourceDiscovery.stage,'redirect');assert.equal(fixture.calls.length,1);
    assert.equal(JSON.stringify(projected.sourceDiscovery).includes('PRIVATE'),false);
  }
  const exhausted=pageRequests(Array.from({length:3},()=>({status:302,headers:{location:'/video-1_2'}})));
  assert.equal((await scopedPage({request:exhausted.request})).sourceDiscovery.category,'redirect_limit');assert.equal(exhausted.calls.length,3);
  const success=pageRequests([{status:302,headers:{location:'https://vkvideo.ru/video-1_2'}},{body:'<meta property="og:video" content="https://cdn.vkuser.net/video.mp4?signature=PRIVATE">'}]);
  const projected=await scopedPage({request:success.request});assert.equal(projected.sourceUrl,vkPost.sourceUrl);assert.equal(projected.sourceDiscovery.status,'fallback_resolved');assert.equal(success.calls.length,2);
  assert.equal(JSON.stringify(projected.sourceDiscovery).includes('PRIVATE'),false);
});

test('VK DNS deadline and caller cancellation remain distinct, and unknown errors are never inferred from text',async()=>{
  for(const [reason,category] of [[new DOMException('PRIVATE timeout','TimeoutError'),'timeout'],[new DOMException('PRIVATE abort','AbortError'),'cancelled']]){
    const controller=new AbortController();let start;
    const started=new Promise(resolve=>{start=resolve;});
    const pending=scopedPage({signal:controller.signal,lookup:()=>{start();return new Promise(()=>{});},request:()=>assert.fail('aborted DNS must not request')});
    await started;controller.abort(reason);const projected=await pending;
    assert.equal(projected.sourceDiscovery.category,`${category}`);assert.equal(projected.sourceDiscovery.stage,'dns');assert.equal(projected.fallbackUrl,'');
  }
  const unknown=await runMediaSource(vkRequest(),{validateScopeFn:scope,fetchVkPageFn:async()=>{throw Object.assign(new Error('HTTP Error403 PRIVATE'),{code:'PRIVATE',discoveryStage:'PRIVATE'});}});
  assert.deepEqual(unknown.sourceDiscovery,{schemaVersion:1,platform:'vk',status:'failed',category:'unknown',stage:'unknown'});
  const invalid=await runMediaSource(vkRequest(),{validateScopeFn:scope,fetchVkPageFn:async()=>({html:'PRIVATE'})});
  assert.equal(invalid.sourceDiscovery.category,'invalid_response');assert.equal(invalid.sourceDiscovery.stage,'locator_parse');
});

const nativeBinding={id:'native-connection-opaque',workspaceId:'native-workspace-opaque',accountId:'LikeAvto',
  connector:'angryspace',revision:7,providerAccountId:'likeavto'};
const twoVideos=()=>({...structuredClone(post),connectorBinding:structuredClone(nativeBinding),attachments:[
  {type:'video',url:'https://www.youtube.com/watch?v=First123_-'},
  {type:'video',url:'https://www.youtube.com/watch?v=Second123_-'}]});
function assetRequest(selectedPost,index){
  const attachment=selectedPost.attachments[index];
  const identity=createHash('sha256').update(JSON.stringify([
    typeof attachment.type==='string'?attachment.type:null,
    typeof attachment.sourceUrl==='string'?attachment.sourceUrl:null,
    typeof attachment.source_url==='string'?attachment.source_url:null,
    typeof attachment.url==='string'?attachment.url:null,
    typeof attachment.id==='string'?attachment.id:null,
    typeof attachment.canonicalMediaId==='string'?attachment.canonicalMediaId:null,
  ])).digest('hex');
  return {account:'likeavto',postId:selectedPost.id,post:selectedPost,assetPin:{schemaVersion:1,contract:'VideoSpeechAssetPin.v1',
    companyId:'LikeAvto',connectorBinding:structuredClone(nativeBinding),postId:selectedPost.id,postKey:selectedPost.postKey,
    sourceVersion:'a'.repeat(64),attachmentIndex:index,attachmentIdentity:identity}};
}
function isolatedSource(request,fetchVkPageFn=async()=>assert.fail('unexpected discovery of unselected source')){
  return runMediaSource(request,{validateScopeFn:scope,fetchVkPageFn});
}

test('native pin selects exactly the second video and echoes every native field',async()=>{
  const request=assetRequest(twoVideos(),1),before=structuredClone(request);
  assert.equal(request.assetPin.attachmentIdentity,'b3a94ea07a3a89315a8188fb8bfe0fbb5b039e4ed1befc25a3ec4f4e6347451b',
    'known-string ordered JSON-array identity vector for the native hash contract');
  const result=await isolatedSource(request);
  assert.equal(result.sourceUrl,'https://www.youtube.com/watch?v=Second123_-');
  assert.deepEqual(result.assetPin,before.assetPin);
  assert.deepEqual(request,before,'projection cannot mutate the captured native request');
});

test('same locator at distinct indices keeps distinct exact asset pins',async()=>{
  const selected=twoVideos();selected.attachments[0]=structuredClone(selected.attachments[1]);
  const first=assetRequest(selected,0),second=assetRequest(selected,1);
  assert.equal(first.assetPin.attachmentIdentity,second.assetPin.attachmentIdentity);
  const a=await isolatedSource(first),b=await isolatedSource(second);
  assert.equal(a.sourceUrl,b.sourceUrl);
  assert.equal(a.assetPin.attachmentIndex,0);assert.equal(b.assetPin.attachmentIndex,1);
  assert.notDeepEqual(a.assetPin,b.assetPin,'equal bytes/locators do not merge native attachment applicability');
});

test('selected pin rejects changed index, reordered identity and malformed native fields before discovery',async()=>{
  for(const change of [
    request=>request.post.attachments.reverse(),
    request=>request.assetPin.attachmentIndex=0,
    request=>request.assetPin.attachmentIndex=2,
    request=>request.assetPin.attachmentIndex=-1,
    request=>request.assetPin.attachmentIndex=0.5,
    request=>request.assetPin.attachmentIndex=10001,
    request=>request.assetPin.postId='different-post',
    request=>request.assetPin.postKey='different-post-key',
    request=>request.assetPin.sourceVersion='not-a-sha',
    request=>request.assetPin.attachmentIdentity='b'.repeat(64),
    request=>request.assetPin.contract='OtherPin.v1',
    request=>request.assetPin.schemaVersion=2,
    request=>request.assetPin.extra='unapproved locator',
    request=>delete request.assetPin.sourceVersion,
    request=>request.assetPin=null,
    request=>request.post.attachmentsState='unknown',
    request=>request.post.attachments[1].type='photo',
  ]){
    const request=assetRequest(twoVideos(),1);change(request);
    await assert.rejects(isolatedSource(request),/MEDIA_ASSET_INVALID/);
  }
});

test('native company and connector scope cannot be borrowed or guessed from transport keys',async()=>{
  for(const change of [
    request=>request.assetPin.companyId='BAW Russia',
    request=>request.assetPin.connectorBinding.accountId='likeavto',
    request=>request.assetPin.connectorBinding.providerAccountId='baw-russia',
    request=>request.assetPin.connectorBinding.connector='vk',
    request=>request.assetPin.connectorBinding.workspaceId='foreign-workspace',
    request=>request.assetPin.connectorBinding.id='foreign-connection',
    request=>request.assetPin.connectorBinding.revision++,
    request=>request.post.account='BAW Russia',
    request=>request.post.attachments[1].accountId='BAW Russia',
  ]){
    const request=assetRequest(twoVideos(),1);change(request);
    await assert.rejects(isolatedSource(request),/ACCOUNT_SCOPE_MISMATCH/);
  }
  for(const change of [
    request=>request.assetPin.connectorBinding.extra=true,
    request=>request.assetPin.connectorBinding.revision=0,
    request=>request.assetPin.connectorBinding.revision=1.5,
  ]){
    const request=assetRequest(twoVideos(),1);change(request);
    await assert.rejects(isolatedSource(request),/MEDIA_ASSET_INVALID/);
  }
  const request=assetRequest(twoVideos(),1);delete request.post.connectorBinding;
  assert.deepEqual((await isolatedSource(request)).assetPin.connectorBinding,nativeBinding,
    'native opaque workspace/id/revision are not replaced with invented provider defaults');
});

test('missing selected locator never uses a sibling video or post fallback',async()=>{
  for(const replacement of [{type:'video'},{type:'video',url:'https://evil.invalid/video'},
    {type:'video',url:'http://vk.com/video-1_2'},{type:'video',url:'https://vkvideo.ru/video_ext.php?oid=-1&id=2&id=3'}]){
    const selected=twoVideos();selected.attachments[1]=replacement;
    selected.sourceUrl='https://vk.com/video-9_9';selected.attachmentSourceUrl='https://vk.com/video-8_8';
    await assert.rejects(isolatedSource(assetRequest(selected,1)),/MEDIA_SELECTED_SOURCE_MISSING/);
  }
});

test('explicit selected index beyond twenty is addressed directly without truncation',async()=>{
  const selected=twoVideos();selected.attachments=Array.from({length:24},()=>({type:'photo'}));
  selected.attachments[23]={type:'video',url:'https://www.youtube.com/watch?v=Second123_-'};
  const request=assetRequest(selected,23),result=await isolatedSource(request);
  assert.equal(result.sourceUrl,'https://www.youtube.com/watch?v=Second123_-');
  assert.equal(result.assetPin.attachmentIndex,23);
});

test('selected VK discovery preserves only the selected extractor identity and exact pin',async()=>{
  const selected=twoVideos();selected.attachments[1]={type:'video',
    source_url:'https://vkvideo.ru/video_ext.php?oid=-9&id=99&hash=12345678&tracking=discarded'};
  const request=assetRequest(selected,1),calls=[];
  const result=await isolatedSource(request,async url=>{calls.push(url);return '<meta property="og:video" content="https://cdn.vkuser.net/selected.mp4">';});
  assert.deepEqual(calls,['https://vkvideo.ru/video_ext.php?oid=-9&id=99&hash=12345678']);
  assert.equal(result.fallbackUrl,'https://cdn.vkuser.net/selected.mp4');
  assert.deepEqual(result.assetPin,request.assetPin);
});

test('legacy multi-video requests reject ambiguity including a second video beyond twenty',async()=>{
  for(const selected of [twoVideos(),{...twoVideos(),attachments:[twoVideos().attachments[0],
    ...Array.from({length:23},()=>({type:'photo'})),twoVideos().attachments[1]]}]){
    await assert.rejects(isolatedSource({account:'likeavto',postId:selected.id,post:selected}),/MEDIA_ASSET_AMBIGUOUS/);
  }
});

test('selected BAW source keeps display company distinct from transport account',async()=>{
  const request=assetRequest(twoVideos(),1);
  request.account='baw-russia';request.assetPin.companyId='BAW Russia';
  const connector={...nativeBinding,id:'baw-opaque-connection',accountId:'BAW Russia',providerAccountId:'baw-russia'};
  request.assetPin.connectorBinding=structuredClone(connector);request.post.connectorBinding=structuredClone(connector);
  const result=await runMediaSource(request,{validateScopeFn:async account=>{
    assert.equal(account,'baw-russia');return {binding:{accountKey:account,providerAccountId:account,displayName:'BAW Russia',objectIds:['object-a']}};
  },fetchVkPageFn:async()=>assert.fail('no VK source selected')});
  assert.equal(result.account,'BAW Russia');assert.deepEqual(result.assetPin,request.assetPin);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  attachmentEvidence,
  postAttachmentEvidence,
  commentMediaSourceGaps,
  postMediaSourceGaps,
  imageUrl,
  publicAddress,
  downloadImage,
  validateImage,
  imageFailureMetadata,
  stageAssistantImages,
  validateVisualSelection,
  conservativeImageEvidenceBytes,
  admitImageDependentProposals,
} from './assistant-images.mjs';
import {prepareAssistantRequest,validateAssistantResult,deterministicMediaHold,runAssistant,assistantCliArgs,generationMetadata} from './assistant.mjs';

const onePixelPng = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC',
  'base64',
);

function selectionFixture(postImages=[]) {
  return {triage:true,payload:{items:[{id:'a',postId:'p'},{id:'b',postId:'p'},{id:'text',postId:'p'}],
    posts:[{id:'p',attachmentStatus:'present',attachments:[
      {type:'photo',url:'https://cdn.example/first.png'},{type:'photo',url:'https://cdn.example/second.png'}]}],
    branches:[],visualSelection:{version:1,postImages}},input:'before'};
}
const photoNeed=(itemId,attachmentIndices=[1])=>({itemId,postId:'p',attachmentIndices,reason:'Verify the exact visual detail needed for this recipient.'});

test('explicit text-first selection omits post photos honestly while comment images remain mandatory',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-selected-text-'));
  try {
    const prepared=selectionFixture();
    prepared.payload.items[0].attachments=[{type:'photo',url:'https://cdn.example/comment.png'}];
    prepared.payload.posts[0].attachments[1]={type:'photo',preview_url:'https://cdn.example/preview.png'};
    const downloaded=[];
    const result=await stageAssistantImages(prepared,home,{download:async url=>{downloaded.push(url);return {bytes:onePixelPng,mime:'image/png'};}});
    assert.deepEqual(downloaded,['https://cdn.example/comment.png']);
    assert.deepEqual(result.blockedItemIds,[]);
    assert.equal(result.manifest[0].origin,'comment_attachment');
    for(const row of prepared.payload.imageEvidence.postImageObservations){
      assert.equal(row.selectionStatus,'not_requested');assert.equal(row.observationStatus,'not_observed');
      assert.deepEqual(row.availableAttachmentIndices,[0,1]);assert.deepEqual(row.observedAttachmentIndices,[]);
    }
    assert.match(prepared.input,/not_observed/);
  }finally {await fs.rm(home,{recursive:true,force:true});}
});

test('selected shared post slot downloads once and failed pixels hold only declared dependent recipients',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-selected-shared-'));
  try {
    const successful=selectionFixture([photoNeed('a'),photoNeed('b')]);let calls=0;
    const result=await stageAssistantImages(successful,home,{download:async url=>{
      calls++;assert.equal(url,'https://cdn.example/second.png');return {bytes:onePixelPng,mime:'image/png'};}});
    assert.equal(calls,1);assert.equal(result.manifest.length,1);
    assert.deepEqual(result.manifest[0].itemIds,['a','b']);assert.equal(result.manifest[0].attachmentIndex,1);
    assert.equal(successful.payload.imageEvidence.postImageObservations[0].observationStatus,'observed');
    assert.equal(successful.payload.imageEvidence.postImageObservations[2].observationStatus,'not_observed');
    assert.deepEqual(successful.payload.imageEvidence.postImageObservations[2].observedAttachmentIndices,[]);
    const failed=selectionFixture([photoNeed('a'),photoNeed('b')]);calls=0;
    const held=await stageAssistantImages(failed,home,{download:async()=>{calls++;throw new Error('unavailable');}});
    assert.equal(calls,1);assert.deepEqual(held.blockedItemIds,['a','b']);
    assert.deepEqual(held.failureEvidence[0].itemIds,['a','b']);
    assert.equal(failed.payload.imageEvidence.postImageObservations[2].selectionStatus,'not_requested');
    assert.equal(failed.payload.imageEvidence.postImageObservations[2].observationStatus,'not_observed');
  }finally {await fs.rm(home,{recursive:true,force:true});}
});

test('unselected missing photo source cannot hold an independently selected readable source',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-selected-readable-'));
  try {
    const prepared=selectionFixture([photoNeed('a',[0])]);
    prepared.payload.posts[0].attachments[1]={type:'photo',preview_url:'https://cdn.example/preview.png'};
    const result=await stageAssistantImages(prepared,home,{download:async()=>({bytes:onePixelPng,mime:'image/png'})});
    assert.deepEqual(result.blockedItemIds,[]);assert.equal(result.manifest[0].attachmentIndex,0);
    const missing=selectionFixture([photoNeed('b')]);
    missing.payload.posts[0].attachments[1]={type:'photo',preview_url:'https://cdn.example/preview.png'};
    const held=await stageAssistantImages(missing,home,{download:async()=>{throw new Error('must not download');}});
    assert.deepEqual(held.blockedItemIds,['b']);
    assert.equal(missing.payload.imageEvidence.postImageObservations[1].observationStatus,'not_observed');
  }finally {await fs.rm(home,{recursive:true,force:true});}
});

test('visual selection rejects foreign sources, duplicate declarations and unbounded or unsupported indices before downloads',()=>{
  const payload=selectionFixture().payload;
  assert.equal(validateVisualSelection(undefined,payload),undefined);
  assert.deepEqual(validateVisualSelection({version:1,postImages:[photoNeed('a',[1,0])]},payload).postImages[0].attachmentIndices,[0,1]);
  const badRows=[{...photoNeed('a'),itemId:'foreign'},{...photoNeed('a'),postId:'other'},
    photoNeed('a',[-1]),photoNeed('a',[2]),photoNeed('a',[1,1]),photoNeed('a',[]),
    {...photoNeed('a'),reason:' '},{...photoNeed('a'),reason:'x'.repeat(501)},
    {...photoNeed('a'),unexpected:true}];
  for(const row of badRows)assert.throws(()=>validateVisualSelection({version:1,postImages:[row]},payload),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>validateVisualSelection({version:1,postImages:[photoNeed('a'),photoNeed('a')]},payload),{code:'ASSISTANT_INVALID_REQUEST'});
  const ambiguous=structuredClone(payload);ambiguous.branches=[{id:'branch',postId:'other'}];ambiguous.items[0].branchId='branch';
  assert.throws(()=>validateVisualSelection({version:1,postImages:[photoNeed('a')]},ambiguous),{code:'ASSISTANT_INVALID_REQUEST'});
  const unsupported=structuredClone(payload);unsupported.posts[0].attachments[1].type='video';
  assert.throws(()=>validateVisualSelection({version:1,postImages:[photoNeed('a')]},unsupported),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('selected-image preflight envelope bounds staged success, failure and honest unknown default observations',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-selected-budget-'));
  try {
    for(const kind of ['success','failure','unknown']){
      const prepared=selectionFixture(kind==='unknown'?[]:[photoNeed('a'),photoNeed('b')]);
      if(kind==='unknown'){delete prepared.payload.posts[0].attachments;prepared.payload.posts[0].attachmentStatus='unknown';}
      const bound=conservativeImageEvidenceBytes(prepared);
      await stageAssistantImages(prepared,home,{download:async()=>{
        if(kind==='failure')throw new Error('failed');return {bytes:onePixelPng,mime:'image/png'};}});
      assert.ok(Buffer.byteLength(',"imageEvidence":')+Buffer.byteLength(JSON.stringify(prepared.payload.imageEvidence))<=bound,kind);
      if(kind==='unknown')for(const row of prepared.payload.imageEvidence.postImageObservations){
        assert.equal(row.sourceStatus,'unknown');assert.equal(row.observationStatus,'not_observed');
      }
    }
  }finally {await fs.rm(home,{recursive:true,force:true});}
});
// Container fixtures intentionally test structural admission, not pixel decode.
function fixtureCrc(bytes){let c=0xffffffff;for(const byte of bytes){c^=byte;for(let bit=0;bit<8;bit++)c=c&1?0xedb88320^(c>>>1):c>>>1;}return (c^0xffffffff)>>>0;}
function pngChunk(kind,data){const b=Buffer.alloc(data.length+12);b.writeUInt32BE(data.length);b.write(kind,4);data.copy(b,8);b.writeUInt32BE(fixtureCrc(b.subarray(4,-4)),b.length-4);return b;}
function pngDimensionsFixture(width,height){const b=Buffer.from(onePixelPng);b.writeUInt32BE(width,16);b.writeUInt32BE(height,20);b.writeUInt32BE(fixtureCrc(b.subarray(12,29)),29);return b;}
function paddedPng(size){return Buffer.concat([onePixelPng.subarray(0,33),pngChunk('npAd',Buffer.alloc(size-onePixelPng.length-12)),onePixelPng.subarray(33)]);}
const onePixelWebp=Buffer.from('UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA','base64');
// Encoded once offline from a synthetic 2x2 red lavfi frame with admitted
// FFmpeg8.1.1 (09948d4c...de0f73); tests have no decoder/tool dependency.
const twoPixelJpeg=Buffer.from('/9j/4AAQSkZJRgABAgAAAQABAAD//gAQTGF2YzYyLjI4LjEwMQD/2wBDAAgEBAQEBAUFBQUFBQYGBgYGBgYGBgYGBgYHBwcICAgHBwcGBgcHCAgICAkJCQgICAgJCQoKCgwMCwsODg4RERT/xABMAAEBAAAAAAAAAAAAAAAAAAAABgEBAQAAAAAAAAAAAAAAAAAABgcQAQAAAAAAAAAAAAAAAAAAAAARAQAAAAAAAAAAAAAAAAAAAAD/wAARCAACAAIDASIAAhEAAxEA/9oADAMBAAIRAxEAPwCLAE1/f//Z','base64');
// SOF/SOS/entropy/EOI fixture for marker framing, not a decoded JPEG claim.
const framedJpeg=Buffer.from([255,216,255,192,0,11,8,0,1,0,1,1,1,0x11,0,255,218,0,8,1,1,0,0,63,0,0x42,255,0,0x73,255,208,0x64,255,217]);
function webpChunk(kind,data){const b=Buffer.alloc(8+data.length+(data.length&1));b.write(kind);b.writeUInt32LE(data.length,4);data.copy(b,8);return b;}
function webpContainer(...chunks){const b=Buffer.concat([Buffer.from('RIFF\0\0\0\0WEBP','binary'),...chunks]);b.writeUInt32LE(b.length-8,4);return b;}

test('PNG structural validation rejects header-only, missing data/terminator, truncation, CRC and trailing bytes',()=>{
  const corruptCrc=Buffer.from(onePixelPng);corruptCrc[29]^=1;
  const ihdr=onePixelPng.subarray(8,33),iend=onePixelPng.subarray(-12);
  const cases=[onePixelPng.subarray(0,33),onePixelPng.subarray(0,-1),onePixelPng.subarray(0,-12),corruptCrc,
    Buffer.concat([onePixelPng,Buffer.from([0])]),Buffer.concat([onePixelPng.subarray(0,8),ihdr,iend]),
    Buffer.concat([onePixelPng.subarray(0,33),pngChunk('IDAT',Buffer.alloc(0)),iend])];
  for(const bytes of cases)assert.throws(()=>validateImage(bytes,'image/png'),{code:'ASSISTANT_MEDIA_UNAVAILABLE',imageCategory:'image_integrity'});
  assert.deepEqual(validateImage(onePixelPng,'image/png'),{extension:'png',width:1,height:1});
  const invalidHeader=Buffer.from(onePixelPng);invalidHeader[26]=1;invalidHeader.writeUInt32BE(fixtureCrc(invalidHeader.subarray(12,29)),29);
  assert.throws(()=>validateImage(invalidHeader,'image/png'),{imageCategory:'image_integrity'});
});

test('JPEG marker framing requires bounded segments, frame, scan entropy and exact EOI',()=>{
  assert.deepEqual(validateImage(twoPixelJpeg,'image/jpeg'),{extension:'jpg',width:2,height:2});
  assert.throws(()=>validateImage(twoPixelJpeg.subarray(0,-2),'image/jpeg'),{imageCategory:'image_integrity'});
  assert.deepEqual(validateImage(framedJpeg,'image/jpeg'),{extension:'jpg',width:1,height:1});
  for(const bytes of [framedJpeg.subarray(0,15),framedJpeg.subarray(0,-2),Buffer.concat([framedJpeg,Buffer.from([0])]),
    Buffer.concat([framedJpeg.subarray(0,25),Buffer.from([255,217])]),Buffer.from([255,216,255,192,255,255,8,0,1])])
    assert.throws(()=>validateImage(bytes,'image/jpeg'),{imageCategory:'image_integrity'});
  const invalidScan=Buffer.from(framedJpeg);invalidScan[20]=2;
  assert.throws(()=>validateImage(invalidScan,'image/jpeg'),{imageCategory:'image_integrity'});
});

test('WebP structural validation requires exact RIFF/chunks and an image bitstream beyond VP8X dimensions',()=>{
  assert.deepEqual(validateImage(onePixelWebp,'image/webp'),{extension:'webp',width:1,height:1});
  const vp8x=Buffer.alloc(10),badRiff=Buffer.from(onePixelWebp);badRiff.writeUInt32LE(12345,4);
  const vp8lHeader=Buffer.from([47,0,0,0,0]);
  for(const bytes of [badRiff,onePixelWebp.subarray(0,-1),webpContainer(webpChunk('VP8X',vp8x)),
    webpContainer(webpChunk('VP8L',vp8lHeader)),webpContainer(webpChunk('VP8 ',Buffer.alloc(10))),Buffer.concat([onePixelWebp,Buffer.from([0])])])
    assert.throws(()=>validateImage(bytes,'image/webp'),{imageCategory:'image_integrity'});
  vp8x[0]=2;const frame=Buffer.concat([Buffer.alloc(16),onePixelWebp.subarray(12)]);
  const animated=webpContainer(webpChunk('VP8X',vp8x),webpChunk('ANIM',Buffer.alloc(6)),webpChunk('ANMF',frame));
  assert.deepEqual(validateImage(animated,'image/webp'),{extension:'webp',width:1,height:1});
  assert.throws(()=>validateImage(animated.subarray(0,-1),'image/webp'),{imageCategory:'image_integrity'});
});

test('GIF and MIME spoofing remain unsupported or invalid rather than attached images',()=>{
  const gif=Buffer.from('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7','base64');
  assert.throws(()=>validateImage(gif,'image/gif'),{imageCategory:'image_unsupported_format'});
  assert.throws(()=>validateImage(gif,'image/png'),{imageCategory:'image_integrity'});
  assert.throws(()=>validateImage(onePixelPng,'image/jpeg'),{imageCategory:'image_integrity'});
});

const imageBatch=count=>({triage:false,input:'before',payload:{items:Array.from({length:count},(_,index)=>({
  id:`image-${index}`,attachments:[{type:'photo',url:`https://cdn.example/${index}.png`}],
}))}});
function deferred() {
  let resolve,reject;
  const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});
  return {promise,resolve,reject};
}

test('bounded image window preserves exact sequential manifests despite reversed completion',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-window-'));
  const baselineHome=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-order-'));
  const gates=Array.from({length:8},deferred),firstWindow=deferred(),secondWindow=deferred();
  const started=[],completed=[];
  const bytes=Array.from({length:8},(_,index)=>{
    return pngDimensionsFixture(index+1,1);
  });
  let active=0,peak=0;
  try {
    const running=stageAssistantImages(imageBatch(8),home,{concurrency:100,download:async url=>{
      const index=Number(new URL(url).pathname.slice(1,-4));
      started.push(index);peak=Math.max(peak,++active);
      if(started.length===4)firstWindow.resolve();
      if(started.length===8)secondWindow.resolve();
      await gates[index].promise;active--;completed.push(index);
      return {bytes:bytes[index],mime:'image/png'};
    }});
    await firstWindow.promise;
    for(const index of [3,2,1])gates[index].resolve();
    await new Promise(resolve=>setImmediate(resolve));
    assert.deepEqual(started,[0,1,2,3],'completed later images cannot refill the bounded buffer window');
    assert.deepEqual(await fs.readdir(home),[],'admission waits for original first candidate');
    gates[0].resolve();
    await secondWindow.promise;
    for(const index of [7,6,5,4])gates[index].resolve();
    const parallel=await running;
    const sequential=await stageAssistantImages(imageBatch(8),baselineHome,{concurrency:1,
      download:async url=>({bytes:bytes[Number(new URL(url).pathname.slice(1,-4))],mime:'image/png'})});
    assert.equal(peak,4);assert.equal(active,0);
    assert.deepEqual(completed.slice(0,4),[3,2,1,0]);
    assert.deepEqual(parallel.manifest,sequential.manifest);
    assert.deepEqual(parallel.paths.map(file=>path.basename(file)),sequential.paths.map(file=>path.basename(file)));
    for(const [index,file] of parallel.paths.entries())assert.deepEqual(await fs.readFile(file),bytes[index]);
  } finally {
    for(const gate of gates)gate.resolve();
    await fs.rm(home,{recursive:true,force:true});await fs.rm(baselineHome,{recursive:true,force:true});
  }
});

test('a stalled source has its own deadline and an independent fast source remains admitted',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-source-timeout-'));
  const events=[];let active=0;
  try {
    const result=await stageAssistantImages(imageBatch(2),home,{sourceTimeoutMs:30,totalTimeoutMs:500,
      download:async(url,{signal})=>{
        active++;events.push(`start:${url}`);
        try {
          if(url.endsWith('/0.png'))await new Promise((_,reject)=>signal.addEventListener('abort',()=>{
            events.push('slow-aborted');reject(signal.reason);
          },{once:true}));
          events.push('fast-completed');return {bytes:onePixelPng,mime:'image/png'};
        } finally {active--;}
      }});
    assert.deepEqual(result.blockedItemIds,['image-0']);
    assert.deepEqual(result.manifest.map(image=>image.itemId),['image-1']);
    assert.ok(events.indexOf('fast-completed')<events.indexOf('slow-aborted'));
    assert.equal(active,0);
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('total deadline and caller cancellation drain active downloads and hold remaining recipients',async()=>{
  for(const cancellation of ['total','caller']){
    const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-cancel-'));
    const controller=new AbortController(),windowStarted=deferred();
    let active=0,started=0,aborted=0;
    try {
      const running=stageAssistantImages(imageBatch(8),home,{signal:controller.signal,
        totalTimeoutMs:cancellation==='total'?30:500,sourceTimeoutMs:500,
        download:async(_url,{signal})=>{
          active++;started++;if(started===4)windowStarted.resolve();
          try {
            await new Promise((_,reject)=>signal.addEventListener('abort',()=>{
              aborted++;reject(signal.reason);
            },{once:true}));
          } finally {active--;}
        }});
      await windowStarted.promise;
      if(cancellation==='caller')controller.abort(new Error('test cancellation'));
      const result=await running;
      assert.equal(started,4);assert.equal(aborted,4);assert.equal(active,0);
      assert.equal(result.paths.length,0);assert.equal(result.blockedItemIds.length,8);
      assert.ok(result.unavailableItems.every(item=>item.reason==='image_unavailable'));
    } finally {await fs.rm(home,{recursive:true,force:true});}
  }
});

test('late downloader rejection is observed after its source deadline',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-late-error-'));
  const late=deferred();
  try {
    const result=await stageAssistantImages(imageBatch(1),home,{sourceTimeoutMs:10,
      download:()=>late.promise});
    assert.deepEqual(result.blockedItemIds,['image-0']);
    late.reject(new Error('late failure from cancelled transport'));
    await new Promise(resolve=>setImmediate(resolve));
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('cancellation holds buffered successes behind a slow head without writing or starting another source',async()=>{
  for(const cancellation of ['caller','total']){
    const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-buffered-cancel-'));
    const controller=new AbortController(),buffered=deferred(),started=[];
    let active=0;
    try {
      const running=stageAssistantImages(imageBatch(8),home,{signal:controller.signal,
        sourceTimeoutMs:500,totalTimeoutMs:cancellation==='total'?50:500,
        download:async(url,{signal})=>{
          started.push(url);active++;
          try {
            if(url.endsWith('/0.png'))await new Promise((_,reject)=>signal.addEventListener('abort',()=>{
              reject(signal.reason);
            },{once:true}));
            if(url.endsWith('/3.png'))buffered.resolve();
            return {bytes:onePixelPng,mime:'image/png'};
          } finally {active--;}
        }});
      await buffered.promise;
      // Let all successful results reach the pending window before aborting.
      await new Promise(resolve=>setImmediate(resolve));
      assert.equal(active,1);assert.deepEqual(await fs.readdir(home),[]);
      if(cancellation==='caller')controller.abort(new Error('cancel buffered successes'));
      const result=await running;
      assert.equal(started.length,4,'no later source starts after cancellation');
      assert.equal(active,0);assert.deepEqual(result.paths,[]);assert.deepEqual(result.manifest,[]);
      assert.deepEqual(result.blockedItemIds,Array.from({length:8},(_,index)=>`image-${index}`));
      assert.ok(result.unavailableItems.every(item=>item.reason==='image_unavailable'));
      assert.deepEqual(await fs.readdir(home),[],'completed buffers cannot produce files after cancellation');
    } finally {await fs.rm(home,{recursive:true,force:true});}
  }
});

test('validation failures stay local and a file-write failure aborts outstanding transports',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-local-error-'));
  try {
    const local=await stageAssistantImages(imageBatch(2),home,{download:async url=>({
      bytes:url.endsWith('/0.png')?Buffer.from('invalid'):onePixelPng,mime:'image/png',
    })});
    assert.deepEqual(local.blockedItemIds,['image-0']);
    assert.deepEqual(local.manifest.map(image=>image.itemId),['image-1']);
    let active=0,aborted=0;
    await assert.rejects(stageAssistantImages(imageBatch(4),path.join(home,'missing-directory'),{
      download:async(url,{signal})=>{
        if(url.endsWith('/0.png'))return {bytes:onePixelPng,mime:'image/png'};
        active++;
        try {
          await new Promise((_,reject)=>signal.addEventListener('abort',()=>{
            aborted++;reject(signal.reason);
          },{once:true}));
        } finally {active--;}
      },
    }),{code:'ENOENT'});
    assert.equal(aborted,3);assert.equal(active,0);
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('concurrent byte and pixel admission retains the original cumulative caps',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-byte-cap-'));
  const pixelHome=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-pixel-cap-'));
  try {
    const full=paddedPng(8*1024*1024);
    const result=await stageAssistantImages(imageBatch(10),home,{download:async()=>({bytes:full,mime:'image/png'})});
    assert.equal(result.manifest.length,8);
    assert.deepEqual(result.blockedItemIds,['image-8','image-9']);
    assert.ok(result.unavailableItems.every(item=>item.reason==='image_budget_exceeded'));
    const written=await Promise.all(result.paths.map(file=>fs.stat(file)));
    assert.equal(written.reduce((sum,file)=>sum+file.size,0),64*1024*1024);
    const large=pngDimensionsFixture(12000,2000);
    const pixels=await stageAssistantImages(imageBatch(10),pixelHome,{download:async()=>({bytes:large,mime:'image/png'})});
    assert.equal(pixels.manifest.reduce((sum,image)=>sum+image.width*image.height,0),192_000_000);
    assert.deepEqual(pixels.blockedItemIds,['image-8','image-9']);
  } finally {
    await fs.rm(home,{recursive:true,force:true});await fs.rm(pixelHome,{recursive:true,force:true});
  }
});

test('controlled synthetic workload records sequential and parallel image-stage timings',async t=>{
  const runs=[];
  for(const concurrency of [1,4]){
    const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-workload-'));
    let active=0,peak=0;const startedAt=performance.now();
    try {
      const result=await stageAssistantImages(imageBatch(8),home,{concurrency,download:async()=>{
        peak=Math.max(peak,++active);
        await new Promise(resolve=>setTimeout(resolve,25));active--;
        return {bytes:onePixelPng,mime:'image/png'};
      }});
      runs.push({concurrency,peak,elapsedMs:Math.round(performance.now()-startedAt),manifest:result.manifest});
      assert.equal(peak,concurrency);assert.equal(active,0);
    } finally {await fs.rm(home,{recursive:true,force:true});}
  }
  assert.deepEqual(runs[0].manifest,runs[1].manifest);
  t.diagnostic(JSON.stringify(runs.map(({manifest,...receipt})=>receipt)));
});

test('twenty post photos expose a batch budget hold, and an eight-photo post succeeds intact in its own batch',async()=>{
  const posts=[['a',5],['b',4],['c',8],['d',3]].map(([id,count])=>({id,attachments:
    Array.from({length:count},(_,index)=>({type:'photo',url:`https://cdn.example/${id}/${index}.png`}))}));
  const items=posts.flatMap(post=>Array.from({length:post.id==='c'?10:1},(_,index)=>({
    id:`${post.id}-${index}`,postId:post.id,attachments:[],attachmentsState:'none',text:'Спасибо, пока помечтаем!'})));
  const held=items.filter(item=>item.postId==='c').map(item=>item.id);
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-budget-repro-'));
  const singleHome=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-budget-single-'));
  try {
    const prepared=prepareAssistantRequest({purpose:'triage',items,posts});const downloads=[];
    const images=await stageAssistantImages(prepared,home,{download:async url=>{
      downloads.push(url);return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.equal(images.paths.length,12);assert.deepEqual(images.blockedItemIds,held);
    assert.ok(images.unavailableItems.every(item=>item.reason==='image_budget_exceeded'));
    assert.equal(downloads.some(url=>url.includes('/c/')),false,'budget rejection is not a failed network read');
    const candidate={text:'Review',sources:[],proposals:items.map(item=>({itemId:item.id,kind:'reply_and_close',text:'Спасибо!'})),
      assessments:items.map(item=>({itemId:item.id,outcome:'reply',reason:'Grounded',tags:[]}))};
    const admitted=admitImageDependentProposals(candidate,images);
    assert.deepEqual(admitted.proposals,candidate.proposals.filter(proposal=>!held.includes(proposal.itemId)));
    assert.ok(admitted.assessments.filter(a=>held.includes(a.itemId)).every(a=>a.outcome==='needs_attention'&&/лимита изображений/.test(a.reason)));
    assert.doesNotMatch(admitted.text,/не удалось прочитать/);
    const smaller=prepareAssistantRequest({purpose:'triage',items:items.filter(item=>item.postId==='c'),posts:[posts[2]]});
    const retried=await stageAssistantImages(smaller,singleHome,{download:async()=>({bytes:onePixelPng,mime:'image/png'})});
    assert.equal(retried.paths.length,8);assert.deepEqual(retried.blockedItemIds,[]);
    assert.ok(retried.manifest.every(image=>image.postId==='c'&&image.itemIds.length===10));
    assert.deepEqual(retried.manifest.map(image=>image.attachmentIndex),[0,1,2,3,4,5,6,7]);
    assert.equal(admitImageDependentProposals(candidate,retried),candidate);
  } finally {
    await fs.rm(home,{recursive:true,force:true});await fs.rm(singleHome,{recursive:true,force:true});
  }
});

test('budget and actual image failures keep distinct reasons without admitting either recipient',()=>{
  const candidate={text:'Review',proposals:[{itemId:'budget'},{itemId:'download'},{itemId:'safe'}],
    assessments:['budget','download','safe'].map(itemId=>({itemId,outcome:'reply',reason:'Original'}))};
  const result=admitImageDependentProposals(candidate,{blockedItemIds:['budget','download'],unavailableItems:[
    {itemId:'budget',reason:'image_budget_exceeded'},{itemId:'download',reason:'image_unavailable'}]});
  assert.deepEqual(result.proposals,[{itemId:'safe'}]);
  assert.match(result.assessments[0].reason,/лимита изображений/);
  assert.match(result.assessments[1].reason,/не удалось прочитать/);
  assert.deepEqual(result.assessments[2],candidate.assessments[2]);
});

test('interactive discussion and lookup survive unreadable media while unsafe recipient proposals are removed',async()=>{
  for(const attachment of [{type:'video',url:'https://cdn.example/video.mp4'},{type:'photo',url:'https://cdn.example/unavailable.png'}]){
    const prepared=prepareAssistantRequest({instruction:'Привет, помоги найти комментарий',lookupAllowed:true,items:[{id:'media',attachments:[attachment]},{id:'text',text:'Спасибо'}]});
    const images=await stageAssistantImages(prepared,'unused-private-home',{download:async()=>{throw new Error('network unavailable');}});
    assert.deepEqual(images.paths,[]);
    assert.deepEqual(images.blockedItemIds,['media']);
    const input=JSON.parse(prepared.input);
    assert.equal(input.imageEvidence.status,'unavailable');
    assert.equal(input.imageEvidence.unavailableItems[0].itemId,'media');
    const lookup=validateAssistantResult({text:'Найду комментарий.',sources:[],proposals:[],lookup:{kind:'search_comments',query:'пример'}},prepared.ids,false,true);
    assert.deepEqual(admitImageDependentProposals(lookup,images).lookup,lookup.lookup);
    const candidate=validateAssistantResult({text:'Вот варианты.',sources:[],proposals:[{itemId:'media',kind:'reply_and_close',text:'Unverified image interpretation'},{itemId:'text',kind:'reply_and_close',text:'Рады помочь!'}]},prepared.ids,false,true);
    const admitted=admitImageDependentProposals(candidate,images);
    assert.deepEqual(admitted.proposals.map(p=>p.itemId),['text']);
    assert.match(admitted.text,/не удалось прочитать/);
    for(const purpose of ['triage','triage_review']){
      const strict={...prepared,triage:true,payload:{...prepared.payload,purpose}};
      const isolated=await stageAssistantImages(strict,'unused-private-home',{download:async()=>{throw new Error('unavailable');}});
      assert.deepEqual(isolated.blockedItemIds,['media']);
      const masked=admitImageDependentProposals({text:'Review',proposals:[{itemId:'media',kind:'reply_and_close',text:'Guess'}],
        assessments:[{itemId:'media',outcome:'reply',reason:'Guess'},{itemId:'text',outcome:'needs_attention',reason:'Other'}],
        evidence:[{itemId:'media',url:'https://example.com/unopened'},{itemId:'text',url:'https://example.com/opened'}]},isolated);
      assert.equal(masked.assessments[0].outcome,'needs_attention');
      assert.deepEqual(masked.proposals,[]);
      assert.deepEqual(masked.evidence.map(source=>source.itemId),['text']);
    }
  }
});

function response(statusCode, headers = {}, body = Buffer.alloc(0)) {
  const res = new EventEmitter();
  res.statusCode = statusCode;
  res.headers = headers;
  res.resume = () => {};
  res.destroy = () => {};
  res._body = body;
  return res;
}

function requestSequence(responses, inspect = () => {}) {
  const calls = [];
  const request = (url, options, callback) => {
    const req = new EventEmitter();
    req.setTimeout = () => {};
    req.destroy = error => req.emit('error', error);
    req.end = () => {
      calls.push({url: new URL(url.href), options});
      try {
        inspect(url, options, calls.length - 1);
        const next = responses.shift();
        if (!next) throw new Error('Unexpected request');
        const res = response(next.statusCode, next.headers, next.body);
        callback(res);
        if (next.statusCode === 200) {
          queueMicrotask(() => {
            if (res._body.length) res.emit('data', res._body);
            res.emit('end');
          });
        }
      } catch (error) {
        queueMicrotask(() => req.emit('error', error));
      }
    };
    return req;
  };
  return {request, calls};
}

test('HTTP acquisition failures retain only closed categories and bounded rate-limit timing',async()=>{
  for(const [status,category] of [[401,'image_auth_required'],[403,'image_http_forbidden'],[404,'image_unavailable'],[410,'image_unavailable'],[429,'image_rate_limited'],[503,'image_network'],[418,'image_http_failed']]){
    const fixture=requestSequence([{statusCode:status,headers:{'retry-after':'120','set-cookie':'PRIVATE'},body:Buffer.from('PRIVATE')}]);
    await assert.rejects(downloadImage('https://cdn.example/image.png?signature=PRIVATE',{lookup:async()=>[{address:'8.8.8.8'}],request:fixture.request}),error=>{
      assert.equal(error.code,'ASSISTANT_MEDIA_UNAVAILABLE');assert.equal(error.imageCategory,category);
      assert.equal(error.retryAfterSeconds,status===429?120:undefined);
      assert.equal(JSON.stringify(error).includes('PRIVATE'),false);return true;
    });
  }
  const fixture=requestSequence([{statusCode:429,headers:{'retry-after':'PRIVATE'}}]);
  await assert.rejects(downloadImage('https://cdn.example/image.png',{lookup:async()=>[{address:'8.8.8.8'}],request:fixture.request}),error=>error.imageCategory==='image_rate_limited'&&error.retryAfterSeconds===undefined);
});

test('response body length mismatch is integrity failure before image admission',async()=>{
  const fixture=requestSequence([{statusCode:200,headers:{'content-type':'image/png','content-length':String(onePixelPng.length+1)},body:onePixelPng}]);
  await assert.rejects(downloadImage('https://cdn.example/image.png',{lookup:async()=>[{address:'8.8.8.8'}],request:fixture.request}),{imageCategory:'image_integrity'});
});

test('closed image categories survive staging without changing recipient holds or exposing raw errors',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-categories-'));
  const errors=[Object.assign(new Error('PRIVATE signed locator'),{code:'EAI_AGAIN'}),Object.assign(new Error('PRIVATE certificate'),{code:'CERT_HAS_EXPIRED'}),Object.assign(new Error('PRIVATE HTTP'),{imageCategory:'image_rate_limited',retryAfterSeconds:120}),new Error('PRIVATE unknown'),Object.assign(new Error('PRIVATE spoof'),{imageCategory:'PRIVATE'})];
  try{
    const result=await stageAssistantImages(imageBatch(6),home,{download:async url=>{
      const index=Number(new URL(url).pathname.slice(1,-4));if(index<errors.length)throw errors[index];return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.deepEqual(result.unavailableItems.map(row=>row.category),['image_network','image_tls','image_rate_limited','image_unknown','image_unknown']);
    assert.ok(result.unavailableItems.every(row=>row.reason==='image_unavailable'));
    assert.equal(result.unavailableItems[2].retryAfterSeconds,120);assert.equal(result.manifest[0].itemId,'image-5');
    assert.equal(JSON.stringify(result).includes('PRIVATE'),false);assert.equal(JSON.stringify(result).includes('cdn.example'),false);
    assert.deepEqual(result.failureEvidence.map(row=>[row.itemId,row.attachmentIndex,row.origin,row.category,row.stage]),
      ['image_network','image_tls','image_rate_limited','image_unknown','image_unknown'].map((category,index)=>[`image-${index}`,0,'comment_attachment',category,'acquisition']));
  }finally{await fs.rm(home,{recursive:true,force:true});}
});

test('failed shared post attachment preserves every exact recipient and distinct comment attachment',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-failure-binding-'));
  try{
    const prepared=prepareAssistantRequest({purpose:'triage',items:[{id:'a',postId:'p'},{id:'b',postId:'p',attachments:[{type:'photo',url:'https://cdn.example/b.png'}]}],
      posts:[{id:'p',attachments:[{type:'photo',url:'https://cdn.example/shared.png'}]}]});
    const images=await stageAssistantImages(prepared,home,{download:async()=>{throw Object.assign(new Error('PRIVATE locator'),{code:'ECONNRESET'});}});
    assert.deepEqual(images.failureEvidence,[{itemId:'a',attachmentIndex:0,origin:'post_attachment',category:'image_network',stage:'acquisition',postId:'p',itemIds:['a','b']},
      {itemId:'b',attachmentIndex:0,origin:'comment_attachment',category:'image_network',stage:'acquisition'}]);
    assert.deepEqual(images.blockedItemIds,['a','b']);
    const metadata=generationMetadata(prepared.input,true,10,'likeavto',false,images.failureEvidence);
    assert.deepEqual(metadata.imageFailures,images.failureEvidence);
    const clean=imageFailureMetadata([{...images.failureEvidence[0],url:'PRIVATE',path:'PRIVATE',error:'PRIVATE',retryAllowed:true}]);
    assert.equal(JSON.stringify(clean).includes('PRIVATE'),false);assert.equal(clean[0].retryAllowed,undefined);
    assert.equal(generationMetadata(prepared.input,true).imageFailures,undefined,'legacy receipts omit the optional field');
    assert.throws(()=>imageFailureMetadata([{...images.failureEvidence[0],category:'PRIVATE'}]),{code:'ASSISTANT_MEDIA_UNAVAILABLE'});
    assert.throws(()=>imageFailureMetadata([{...images.failureEvidence[0],retryAfterSeconds:1}]),{code:'ASSISTANT_MEDIA_UNAVAILABLE'});
    assert.throws(()=>imageFailureMetadata([{...images.failureEvidence[0],itemIds:['a','a']}]),{code:'ASSISTANT_MEDIA_UNAVAILABLE'});
  }finally{await fs.rm(home,{recursive:true,force:true});}
});

test('source deadline, total deadline and caller cancellation have distinct closed categories',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-deadline-categories-'));
  try{
    const stalled=()=>new Promise(()=>{});
    const source=await stageAssistantImages(imageBatch(1),home,{download:stalled,sourceTimeoutMs:5,totalTimeoutMs:40});
    assert.equal(source.unavailableItems[0].category,'image_source_timeout');
    const total=await stageAssistantImages(imageBatch(1),home,{download:stalled,sourceTimeoutMs:40,totalTimeoutMs:5});
    assert.equal(total.unavailableItems[0].category,'image_total_timeout');
    const controller=new AbortController();controller.abort(new Error('PRIVATE cancelled'));
    const cancelled=await stageAssistantImages(imageBatch(1),home,{download:()=>{throw new Error('must not start');},signal:controller.signal});
    assert.equal(cancelled.unavailableItems[0].category,'image_cancelled');assert.equal(JSON.stringify(cancelled).includes('PRIVATE'),false);
    assert.equal(cancelled.failureEvidence[0].stage,'not_started');
  }finally{await fs.rm(home,{recursive:true,force:true});}
});

test('attachment projection distinguishes unknown, none, present, unavailable and unsupported media', () => {
  assert.deepEqual(attachmentEvidence({id: 'item-1'}), {attachmentStatus: 'unknown'});
  assert.deepEqual(attachmentEvidence({id: 'item-1', commentAttachments: []}), {
    attachmentStatus: 'none', attachments: [],
  });
  assert.deepEqual(attachmentEvidence({id: 'item-1', attachmentsState: 'unknown', commentAttachments: []}), {
    attachmentStatus: 'unknown', attachments: [],
  });
  assert.deepEqual(attachmentEvidence({id: 'item-1', commentAttachmentsPresent: true}), {
    attachmentStatus: 'unavailable',
  });
  assert.deepEqual(attachmentEvidence({attachments: [{type: 'photo', url: 'https://img.example/a.png'}]}), {
    attachmentStatus: 'present',
    attachments: [{type: 'photo', url: 'https://img.example/a.png'}],
  });
  assert.deepEqual(attachmentEvidence({attachments: [{type: 'audio', title: 'voice'}]}), {
    attachmentStatus: 'present',
    attachments: [{type: 'unsupported', title: 'voice'}],
  });
  assert.throws(() => attachmentEvidence({attachments: 'not-an-array'}), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
});

test('post attachment projection remains separate from comment compatibility fields', () => {
  assert.deepEqual(postAttachmentEvidence({attachmentsState:'present'}),{attachmentStatus:'unavailable'});
  assert.deepEqual(postAttachmentEvidence({attachments:[]}),{attachmentStatus:'none',attachments:[]});
  assert.deepEqual(postAttachmentEvidence({commentAttachments:[{type:'photo',url:'https://cdn.example/comment.png'}]}),
    {attachmentStatus:'unknown'});
  assert.deepEqual(postAttachmentEvidence({attachments:[{type:'photo',url:'https://cdn.example/seats.png',secret:'discard'}]}),
    {attachmentStatus:'present',attachments:[{type:'photo',url:'https://cdn.example/seats.png'}]});
});

test('post photo source gaps attach to the selected recipient before downloads', async () => {
  const items=[{id:'N30',branchId:'branch-N30'}];
  const branches=[{id:'branch-N30',postId:'post-N30'}];
  const posts=[{id:'post-N30',attachmentStatus:'present',attachments:[{type:'photo',preview_url:'https://cdn.example/thumb.png'}]}];
  assert.deepEqual(postMediaSourceGaps(items,posts,branches),[
    {itemId:'N30',postId:'post-N30',reason:'missing_original_post_image_url'},
  ]);
  const prepared={triage:true,payload:{items,posts,branches},input:'unchanged'};
  let downloads=0;
  await assert.rejects(stageAssistantImages(prepared,os.tmpdir(),{download:async()=>{downloads++;}}),
    error=>error.code==='ASSISTANT_MEDIA_UNAVAILABLE'&&error.mediaCause==='source_unavailable'
      &&error.unavailableItems?.[0]?.postId==='post-N30');
  assert.equal(downloads,0);
  assert.equal(prepared.input,'unchanged');
  assert.throws(()=>postMediaSourceGaps([{id:'N30',postId:'different',branchId:'branch-N30'}],posts,branches),
    {code:'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.throws(()=>postMediaSourceGaps(items,[...posts,{...posts[0]}],branches),
    {code:'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.deepEqual(postMediaSourceGaps(items,[],branches),[
    {itemId:'N30',postId:'post-N30',reason:'missing_post_record'},
  ]);
});

test('staging binds a post seating photo separately from a commenter image', async () => {
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-post-images-'));
  try {
    const prepared={triage:true,payload:{items:[
      {id:'N30',branchId:'branch-N30',attachmentStatus:'present',attachments:[{type:'photo',url:'https://cdn.example/meme.png'}]},
    ],branches:[{id:'branch-N30',postId:'post-N30'}],posts:[
      {id:'post-N30',attachmentStatus:'present',attachments:[
        {type:'photo',url:'https://cdn.example/seats.png'},
        {type:'video',url:'https://cdn.example/clip.mp4'},
      ]},
    ]},input:'old input'};
    const downloads=[];
    const {paths,manifest}=await stageAssistantImages(prepared,home,{download:async url=>{
      downloads.push(url);return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.deepEqual(downloads,['https://cdn.example/meme.png','https://cdn.example/seats.png']);
    assert.equal(paths.length,2);
    assert.deepEqual(manifest.map(({imageNumber,itemId,itemIds,postId,attachmentIndex,origin})=>({imageNumber,itemId,itemIds,postId,attachmentIndex,origin})),[
      {imageNumber:1,itemId:'N30',itemIds:undefined,postId:undefined,attachmentIndex:0,origin:'comment_attachment'},
      {imageNumber:2,itemId:'N30',itemIds:['N30'],postId:'post-N30',attachmentIndex:0,origin:'post_attachment'},
    ]);
    assert.match(paths[0],/comment-image-1\.png$/);
    assert.match(paths[1],/post-image-2\.png$/);
    assert.deepEqual(prepared.payload.imageEvidence.images,manifest);
    assert.equal(JSON.parse(prepared.input).imageEvidence.images[1].postId,'post-N30');
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('post photo failure blocks affected recipient and combined image count stays bounded', async () => {
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-post-failure-'));
  try {
    const payload={items:[{id:'N30',postId:'post-N30'},{id:'other',postId:'post-other'}],posts:[
      {id:'post-N30',attachmentStatus:'present',attachments:[{type:'photo',url:'https://cdn.example/seats.png'}]},
      {id:'post-other',attachmentStatus:'none',attachments:[]},
    ]};
    const discussion={triage:false,payload:structuredClone(payload),input:'old'};
    const result=await stageAssistantImages(discussion,home,{download:async()=>{throw new Error('unavailable');}});
    assert.deepEqual(result.blockedItemIds,['N30']);
    assert.equal(discussion.payload.imageEvidence.status,'unavailable');
    const admitted=admitImageDependentProposals({text:'draft',proposals:[{itemId:'N30'},{itemId:'other'}]},result);
    assert.deepEqual(admitted.proposals,[{itemId:'other'}]);
    const strict={triage:true,payload:structuredClone(payload),input:'old'};
    const strictResult=await stageAssistantImages(strict,home,{download:async()=>{throw new Error('unavailable');}});
    assert.deepEqual(strictResult.blockedItemIds,['N30']);
    const many={triage:true,payload:{items:[{id:'N30',postId:'post-N30',attachments:Array.from({length:9},(_,n)=>({type:'photo',url:`https://cdn.example/c${n}.png`}))}],
      posts:[{id:'post-N30',attachments:Array.from({length:8},(_,n)=>({type:'photo',url:`https://cdn.example/p${n}.png`}))}]},input:'old'};
    let downloads=0;
    await assert.rejects(stageAssistantImages(many,home,{download:async()=>{downloads++;}}),
      {code:'ASSISTANT_MEDIA_UNAVAILABLE'});
    assert.equal(downloads,0);
    const sharedPost={triage:true,payload:{items:Array.from({length:9},(_,n)=>({id:`comment-${n}`,postId:'post-N30'})),
      posts:[{id:'post-N30',attachments:[{type:'photo',url:'https://cdn.example/seats.png'}]}]},input:'old'};
    const shared=await stageAssistantImages(sharedPost,home,{download:async()=>{
      downloads++;return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.equal(downloads,1,'one exact post attachment is transported once');
    assert.equal(shared.paths.length,1);
    assert.deepEqual(shared.manifest[0].itemIds,Array.from({length:9},(_,n)=>`comment-${n}`));
    assert.equal(shared.manifest[0].itemId,'comment-0');
    const distinct={triage:true,payload:{items:Array.from({length:17},(_,n)=>({id:`comment-${n}`,postId:`post-${n}`})),
      posts:Array.from({length:17},(_,n)=>({id:`post-${n}`,attachments:[{type:'photo',url:'https://cdn.example/same-url.png'}]}))},input:'old'};
    const distinctHome=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-distinct-images-'));
    try {
      const split=await stageAssistantImages(distinct,distinctHome,{download:async()=>{
        downloads++;return {bytes:onePixelPng,mime:'image/png'};
      }});
      assert.equal(split.paths.length,16);
      assert.deepEqual(split.blockedItemIds,['comment-16']);
      assert.equal(downloads,17,'equal URLs on distinct posts must not borrow one source image');
    } finally {await fs.rm(distinctHome,{recursive:true,force:true});}
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('a nine-photo post reaches one recipient with complete image and CLI provenance',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-nine-post-photos-'));
  try {
    const prepared=prepareAssistantRequest({purpose:'triage',items:[{id:'recipient',postId:'post-nine'}],
      posts:[{id:'post-nine',attachments:Array.from({length:9},(_,index)=>
        ({type:'photo',url:`https://cdn.example/post-${index}.png`}))}]});
    const images=await stageAssistantImages(prepared,home,{download:async()=>({bytes:onePixelPng,mime:'image/png'})});
    assert.deepEqual(images.blockedItemIds,[]);
    assert.equal(images.paths.length,9);
    assert.deepEqual(images.manifest.map(image=>image.attachmentIndex),[0,1,2,3,4,5,6,7,8]);
    assert.ok(images.manifest.every(image=>image.postId==='post-nine'&&image.origin==='post_attachment'
      &&image.itemId==='recipient'&&image.itemIds.length===1&&image.itemIds[0]==='recipient'));
    assert.deepEqual(prepared.payload.imageEvidence.images,images.manifest);
    const args=assistantCliArgs(home,false,images.paths);
    assert.deepEqual(args.filter(arg=>images.paths.includes(arg)),images.paths);
    assert.equal(args.filter(arg=>arg==='-i').length,9);
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('aggregate pixel budget holds only a recipient whose photos exceed the former envelope',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-image-pixel-budget-'));
  try {
    const large=pngDimensionsFixture(11500,2000);
    const prepared=prepareAssistantRequest({purpose:'triage',items:[
      {id:'heavy',postId:'post-heavy'},{id:'ready',postId:'post-ready'},
    ],posts:[
      {id:'post-heavy',attachments:Array.from({length:9},(_,index)=>
        ({type:'photo',url:`https://cdn.example/heavy-${index}.png`}))},
      {id:'post-ready',attachments:[{type:'photo',url:'https://cdn.example/ready.png'}]},
    ]});
    const images=await stageAssistantImages(prepared,home,{download:async url=>
      ({bytes:url.includes('heavy')?large:onePixelPng,mime:'image/png'})});
    assert.deepEqual(images.blockedItemIds,['heavy']);
    assert.equal(prepared.payload.imageEvidence.unavailableItems[0].reason,'image_budget_exceeded');
    assert.equal(images.manifest.filter(image=>image.postId==='post-heavy').length,8);
    assert.equal(images.manifest.filter(image=>image.postId==='post-ready').length,1);
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('an over-budget post holds only its recipients while another image-backed reply remains reviewable',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-mixed-batch-images-'));
  try {
    const request={purpose:'triage',items:[
      {id:'large-a',postId:'post-large'},
      {id:'large-b',postId:'post-large'},
      {id:'ready',postId:'post-ready'},
    ],posts:[
      {id:'post-large',attachments:Array.from({length:17},(_,index)=>({type:'photo',url:`https://cdn.example/large-${index}.png`}))},
      {id:'post-ready',attachments:[{type:'photo',url:'https://cdn.example/ready.png'}]},
    ]};
    const prepared=prepareAssistantRequest(request);
    const downloaded=[];
    const images=await stageAssistantImages(prepared,home,{download:async url=>{
      downloaded.push(url);return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.deepEqual(images.blockedItemIds,['large-a','large-b']);
    assert.equal(images.manifest.length,1);
    assert.equal(images.manifest[0].postId,'post-ready');
    assert.deepEqual(downloaded,['https://cdn.example/ready.png']);
    const model={text:'Drafts',sources:[],assessments:[
      {itemId:'large-a',outcome:'reply',reason:'Guessed from absent photos'},
      {itemId:'large-b',outcome:'close',reason:'Guessed from absent photos'},
      {itemId:'ready',outcome:'reply',reason:'Grounded in available post photo'},
    ],proposals:[
      {itemId:'large-a',kind:'reply_and_close',text:'Unsafe guess'},
      {itemId:'large-b',kind:'close',text:''},
      {itemId:'ready',kind:'reply_and_close',text:'Available image reply'},
    ]};
    const guarded=admitImageDependentProposals(model,images);
    const admitted=validateAssistantResult(guarded,prepared.ids,true);
    assert.deepEqual(admitted.proposals.map(p=>p.itemId),['ready']);
    assert.deepEqual(admitted.assessments.map(a=>a.outcome),['needs_attention','needs_attention','reply']);
    assert.equal(prepared.payload.imageEvidence.unavailableItems[0].reason,'image_budget_exceeded');
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('shared post provenance still names a recipient held for their own image',async()=>{
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-shared-partial-'));
  try {
    const prepared=prepareAssistantRequest({purpose:'triage',items:[
      {id:'blocked',postId:'post',attachments:[{type:'sticker',url:'https://cdn.example/broken.png'}]},
      {id:'ready',postId:'post'},
    ],posts:[{id:'post',attachments:[{type:'photo',url:'https://cdn.example/shared.png'}]}]});
    const images=await stageAssistantImages(prepared,home,{download:async url=>{
      if(url.endsWith('/broken.png'))throw new Error('unavailable');
      return {bytes:onePixelPng,mime:'image/png'};
    }});
    assert.deepEqual(images.blockedItemIds,['blocked']);
    const postImage=images.manifest.find(image=>image.origin==='post_attachment');
    assert.deepEqual(postImage.itemIds,['blocked','ready']);
    assert.equal(prepared.payload.imageEvidence.status,'partial');
  } finally {await fs.rm(home,{recursive:true,force:true});}
});

test('proven source gaps block only their own comment before image downloads', async () => {
  const target={id:'target',attachmentStatus:'present',attachments:[{type:'unsupported'}]};
  const parent={id:'other',attachmentStatus:'none',attachments:[]};
  assert.deepEqual(commentMediaSourceGaps([target,parent]),[
    {itemId:'target',reason:'missing_attachment_locator'},
  ]);
  assert.deepEqual(commentMediaSourceGaps([
    {id:'missing',attachmentStatus:'unavailable',attachments:[]},
    {id:'partial',attachmentStatus:'present',attachments:[]},
    {id:'photo',attachmentStatus:'present',attachments:[{type:'photo',preview_url:'https://cdn.example/preview.png'}]},
    {id:'video',attachmentStatus:'present',attachments:[{type:'video',url:'https://cdn.example/video.mp4'}]},
    {id:'good',attachmentStatus:'present',attachments:[{type:'sticker',url:'https://cdn.example/sticker.png'}]},
    {id:'unknown',attachmentStatus:'unknown',attachments:[]},
  ]),[
    {itemId:'missing',reason:'missing_attachment_metadata'},
    {itemId:'partial',reason:'missing_attachment_metadata'},
    {itemId:'photo',reason:'missing_original_image_url'},
  ]);
  const prepared={triage:true,payload:{items:[target,parent]},input:'unchanged'};
  let downloads=0;
  const split=await stageAssistantImages(prepared,os.tmpdir(),{
    download:async()=>{downloads++;throw new Error('must not download');},
  });
  assert.deepEqual(split.blockedItemIds,['target']);
  assert.equal(downloads,0);
  assert.equal(prepared.payload.imageEvidence.unavailableItems[0].reason,'missing_attachment_locator');
});

test('video with a locator is a capability gap, not missing upstream media', async () => {
  const video={triage:true,payload:{items:[
    {id:'video',attachmentStatus:'present',attachments:[{type:'video',url:'https://cdn.example/video.mp4'}]},
  ]},input:'unchanged'};
  assert.deepEqual(commentMediaSourceGaps(video.payload.items),[]);
  await assert.rejects(stageAssistantImages(video,os.tmpdir(),{
    download:async()=>{throw new Error('must not download');},
  }),error=>error.code==='ASSISTANT_MEDIA_UNAVAILABLE'&&error.mediaCause==='unsupported_capability');
});

test('failed retrieval remains distinct from a missing provider locator', async () => {
  const prepared={triage:true,payload:{items:[
    {id:'photo',attachmentStatus:'present',attachments:[{type:'photo',url:'https://cdn.example/photo.png'}]},
  ]},input:'unchanged'};
  let downloads=0;
  await assert.rejects(stageAssistantImages(prepared,os.tmpdir(),{
    download:async()=>{downloads++;throw new Error('network unavailable');},
  }),error=>error.code==='ASSISTANT_MEDIA_UNAVAILABLE'&&error.mediaCause===undefined);
  assert.equal(downloads,1);
});

test('single-item triage source gap returns an honest operator hold before assistant runtime', async () => {
  const request={purpose:'triage',items:[{id:'target',attachmentsState:'present',attachments:[{type:'unsupported'}]}]};
  const prepared=prepareAssistantRequest(request);
  const expected=deterministicMediaHold(prepared);
  assert.equal(expected.decisionSource,'deterministic_media_source_gap');
  assert.deepEqual(expected.proposals,[]);
  assert.deepEqual(expected.sources,[]);
  assert.deepEqual(expected.assessments.map(({itemId,outcome,tags})=>({itemId,outcome,tags})),[
    {itemId:'target',outcome:'needs_attention',tags:['missing_context']},
  ]);
  assert.equal(expected.runMetadata,undefined,'no model or research provenance should be invented');
  assert.match(expected.text,/Содержимое не проверено/);
  assert.equal(deterministicMediaHold(prepareAssistantRequest({...request,items:[...request.items,{id:'other',attachments:[]}]})),null);
  assert.equal(deterministicMediaHold(prepareAssistantRequest({...request,purpose:'triage_review',firstPass:expected})),null);
  const oldMode=process.env.COMMUNITYHERO_RUNTIME_MODE;
  const oldCli=process.env.COMMUNITYHERO_CODEX_CLI;
  const oldDir=process.env.COMMUNITYHERO_ASSISTANT_DATA_DIR;
  try{
    process.env.COMMUNITYHERO_RUNTIME_MODE='portable';
    delete process.env.COMMUNITYHERO_CODEX_CLI;
    delete process.env.COMMUNITYHERO_ASSISTANT_DATA_DIR;
    assert.deepEqual(await runAssistant(request),expected,'source gap must return before runtime/model access');
    await assert.rejects(runAssistant({...request,items:[{id:'photo',attachments:[{type:'photo',url:'https://cdn.example/photo.png'}]}]}),
      {code:'ASSISTANT_UNAVAILABLE'});
  }finally{
    if(oldMode===undefined)delete process.env.COMMUNITYHERO_RUNTIME_MODE;else process.env.COMMUNITYHERO_RUNTIME_MODE=oldMode;
    if(oldCli===undefined)delete process.env.COMMUNITYHERO_CODEX_CLI;else process.env.COMMUNITYHERO_CODEX_CLI=oldCli;
    if(oldDir===undefined)delete process.env.COMMUNITYHERO_ASSISTANT_DATA_DIR;else process.env.COMMUNITYHERO_ASSISTANT_DATA_DIR=oldDir;
  }
});

test('image URL and address checks admit public HTTPS and reject unsafe locators or non-public addresses', () => {
  assert.equal(imageUrl('https://cdn.example/image.png?size=small#preview').href,
    'https://cdn.example/image.png?size=small');
  for (const value of [
    'http://cdn.example/image.png',
    'https://user:pass@cdn.example/image.png',
    'https://127.0.0.1/image.png',
    'https://[::1]/image.png',
    'https://printer.local/image.png',
    'https://cdn.example/image.png?access_token=secret',
    'https://cdn.example/image.png\n',
  ]) assert.throws(() => imageUrl(value), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'}, value);

  assert.equal(publicAddress('8.8.8.8'), true);
  for (const value of ['0.1.2.3', '10.0.0.1', '127.0.0.1', '169.254.1.1', '172.16.0.1',
    '192.168.1.1', '100.64.0.1', '224.0.0.1', '::1', 'not-an-ip']) {
    assert.equal(publicAddress(value), false, value);
  }
});

test('download pins the socket lookup to the validated public DNS answer', async () => {
  const lookups = [];
  const {request, calls} = requestSequence([{
    statusCode: 200,
    headers: {'content-type': 'image/png', 'content-length': String(onePixelPng.length)},
    body: onePixelPng,
  }], (_url, options) => {
    assert.equal(options.method, 'GET');
    assert.equal(options.agent, false);
    assert.equal(options.headers.Accept, 'image/png,image/jpeg,image/webp');
    options.lookup('cdn.example', {all: true}, (error, answer) => {
      assert.ifError(error);
      assert.deepEqual(answer, [{address: '8.8.4.4', family: 4}]);
    });
  });
  const result = await downloadImage('https://cdn.example/image.png', {
    lookup: async (host, options) => {
      lookups.push({host, options});
      return [{address: '8.8.4.4', family: 4}];
    },
    request,
  });
  assert.deepEqual(lookups, [{host: 'cdn.example', options: {all: true, family: 4}}]);
  assert.equal(calls.length, 1);
  assert.deepEqual(result.bytes, onePixelPng);
  assert.equal(result.mime, 'image/png');
});

test('download rejects mixed public and private DNS answers before opening a request', async () => {
  let requests = 0;
  await assert.rejects(downloadImage('https://cdn.example/image.png', {
    lookup: async () => [{address: '8.8.8.8'}, {address: '127.0.0.1'}],
    request: () => { requests++; throw new Error('must not request'); },
  }), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.equal(requests, 0);
});

test('download aborts a DNS lookup that remains pending', async () => {
  const controller = new AbortController();
  let lookupStarted;
  const started = new Promise(resolve => { lookupStarted = resolve; });
  let requests = 0;
  const pending = downloadImage('https://cdn.example/image.png', {
    signal: controller.signal,
    lookup: () => { lookupStarted(); return new Promise(() => {}); },
    request: () => { requests++; throw new Error('must not request'); },
  });
  await started;
  controller.abort();
  await assert.rejects(pending, {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.equal(requests, 0);
});

test('redirects are independently checked and private redirect targets never reach request', async () => {
  const {request, calls} = requestSequence([{statusCode: 302, headers: {location: 'https://127.0.0.1/private.png'}}]);
  let lookups = 0;
  await assert.rejects(downloadImage('https://cdn.example/image.png', {
    lookup: async () => { lookups++; return [{address: '1.1.1.1'}]; },
    request,
  }), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.equal(calls.length, 1);
  assert.equal(lookups, 1);
});

test('download enforces byte limits and validation enforces MIME, format and pixel limits', async () => {
  const {request} = requestSequence([{statusCode: 200, headers: {'content-length': String(8 * 1024 * 1024 + 1)}}]);
  await assert.rejects(downloadImage('https://cdn.example/large.png', {
    lookup: async () => [{address: '8.8.8.8'}], request,
  }), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  const oversizedStream = Buffer.alloc(8 * 1024 * 1024 + 1);
  const streamed = requestSequence([{statusCode: 200, headers: {'content-type': 'image/png'}, body: oversizedStream}]);
  await assert.rejects(downloadImage('https://cdn.example/stream-large.png', {
    lookup: async () => [{address: '8.8.8.8'}], request: streamed.request,
  }), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});

  assert.deepEqual(validateImage(onePixelPng, 'image/png'), {extension: 'png', width: 1, height: 1});
  assert.throws(() => validateImage(onePixelPng, 'image/jpeg'), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  assert.throws(() => validateImage(onePixelPng, 'application/octet-stream'), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  const hugePng = Buffer.from(onePixelPng);
  hugePng.writeUInt32BE(12001, 16);
  assert.throws(() => validateImage(hugePng, 'image/png'), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
  const tooManyPixelsPng = Buffer.from(onePixelPng);
  tooManyPixelsPng.writeUInt32BE(6000, 16);
  tooManyPixelsPng.writeUInt32BE(5000, 20);
  assert.throws(() => validateImage(tooManyPixelsPng, 'image/png'), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
});

test('staging writes bounded PNGs and binds manifest entries to the exact item and attachment', async () => {
  const home = await fs.mkdtemp(path.join(os.tmpdir(), 'communityhero-assistant-images-'));
  try {
    const prepared = {payload: {items: [
      {id: 'comment-A', postId: 'post-A', attachmentStatus: 'present', attachments: [
        {type: 'photo', url: 'https://cdn.example/a.png'},
        {type: 'image', url: 'https://cdn.example/b.png'},
      ]},
      {id: 'comment-B', postId: 'post-B', attachmentStatus: 'none', attachments: []},
    ],posts:[{id:'post-A',attachments:[]},{id:'post-B',attachments:[]}]}, input: 'old input'};
    let downloads = 0;
    const {paths, manifest} = await stageAssistantImages(prepared, home, {
      download: async url => {
        downloads++;
        assert.match(url, /^https:\/\/cdn\.example\//);
        return {bytes: onePixelPng, mime: 'image/png'};
      },
    });
    assert.equal(downloads, 2);
    assert.equal(paths.length, 2);
    assert.deepEqual(manifest.map(({imageNumber, itemId, attachmentIndex, origin, width, height}) =>
      ({imageNumber, itemId, attachmentIndex, origin, width, height})), [
      {imageNumber: 1, itemId: 'comment-A', attachmentIndex: 0, origin: 'comment_attachment', width: 1, height: 1},
      {imageNumber: 2, itemId: 'comment-A', attachmentIndex: 1, origin: 'comment_attachment', width: 1, height: 1},
    ]);
    assert.ok(manifest.every(entry => !entry.postId));
    assert.equal(manifest[0].sha256.length, 64);
    for (const file of paths) assert.deepEqual(await fs.readFile(file), onePixelPng);
    assert.deepEqual(prepared.payload.imageEvidence, {status: 'attached', images: manifest, branchImagesAttached: false});
    assert.equal(JSON.parse(prepared.input).items[0].id, 'comment-A');
  } finally {
    await fs.rm(home, {recursive: true, force: true});
  }
});

test('staging fails closed for unavailable or unsupported media instead of recording no images', async () => {
  for (const item of [
    {id: 'item-unavailable', attachmentStatus: 'unavailable', attachments: []},
    {id: 'item-unsupported', attachmentStatus: 'present', attachments: [{type: 'video', url: 'https://cdn.example/video.mp4'}]},
  ]) {
    const prepared = {payload: {items: [item]}, input: 'unchanged'};
    await assert.rejects(stageAssistantImages(prepared, os.tmpdir(), {
      download: async () => { throw new Error('download must not run'); },
    }), {code: 'ASSISTANT_MEDIA_UNAVAILABLE'});
    assert.equal(prepared.payload.imageEvidence, undefined);
    assert.equal(prepared.input, 'unchanged');
  }
});

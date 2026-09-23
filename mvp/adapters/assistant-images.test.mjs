import test from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  attachmentEvidence,
  commentMediaSourceGaps,
  imageUrl,
  publicAddress,
  downloadImage,
  validateImage,
  stageAssistantImages,
  admitImageDependentProposals,
} from './assistant-images.mjs';
import {prepareAssistantRequest,validateAssistantResult,deterministicMediaHold,runAssistant} from './assistant.mjs';

const onePixelPng = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC',
  'base64',
);

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
      await assert.rejects(stageAssistantImages(strict,'unused-private-home',{download:async()=>{throw new Error('unavailable');}}),{code:'ASSISTANT_MEDIA_UNAVAILABLE'});
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
  await assert.rejects(stageAssistantImages(prepared,os.tmpdir(),{
    download:async()=>{downloads++;throw new Error('must not download');},
  }),error=>error.code==='ASSISTANT_MEDIA_UNAVAILABLE'&&error.mediaCause==='source_unavailable'
    &&error.unavailableItems?.[0]?.itemId==='target');
  assert.equal(downloads,0);
  assert.equal(prepared.input,'unchanged');
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
    ]}, input: 'old input'};
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

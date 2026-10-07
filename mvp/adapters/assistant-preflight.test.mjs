import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {dispatch} from './bridge.mjs';
import {assistantPreflight} from './assistant-preflight.mjs';
import {prepareAssistantRequest} from './assistant.mjs';
import {conservativeImageEvidenceBytes,stageAssistantImages} from './assistant-images.mjs';

const sha=value=>createHash('sha256').update(value,'utf8').digest('hex');
const entry=request=>{const serializedRequest=JSON.stringify(request);
  return {serializedRequest,requestSha256:sha(serializedRequest)};};
const binding={id:'connector',workspaceId:'workspace',accountId:'LikeAvto',
  connector:'angryspace',revision:1,providerAccountId:'likeavto'};
const baseRequest=(items=[{id:'one'}],posts=[])=>({account:'LikeAvto',connectorBinding:binding,
  purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',
  modelContextContract:'shared_moderation_v1',researchPolicy:'context_sufficient_v1',
  items:items.map(item=>({...item,moderationCapabilities:{hide:'supported',delete:'supported'}})),
  posts,branches:[],materials:[],knowledgeManifest:[],knowledgePolicyVersion:1,
  moderationContext:{version:1,account:'LikeAvto',connectorBinding:binding,ruleRefs:[]}});
const command=entries=>({account:'likeavto',operation:'assistant_preflight',requests:entries});
const noProvider={runProcessFn:()=>{throw new Error('provider process called');},
  resolvePaths:()=>{throw new Error('provider paths resolved');}};
const fixtureDir=process.env.COMMUNITYHERO_CAPACITY_FIXTURE_DIR;
const onePixelPng=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC','base64');

test('canonical Rust captures produce one ordered batch receipt without provider access',{skip:!fixtureDir},async()=>{
  assert.ok(path.isAbsolute(fixtureDir),'capacity fixture directory must be absolute');
  const [small,large]=await Promise.all(['rules47.json','rules300.json'].map(name=>fs.readFile(path.join(fixtureDir,name),'utf8')));
  const entries=[small,large].map(serializedRequest=>({serializedRequest,requestSha256:sha(serializedRequest)}));
  const result=await dispatch(command(entries),noProvider);
  assert.equal(result.version,1);
  assert.deepEqual(result.results.map(row=>row.requestSha256),entries.map(row=>row.requestSha256));
  assert.equal(result.results[0].status,'fits');
  assert.equal(result.results[0].textBytes,250713);
  assert.ok(result.results[0].boundedModelBytes<=550000);
  assert.equal(result.results[1].status,'oversized');
  assert.equal(result.results[1].textBytes,null);
  assert.equal(result.results[1].boundedModelBytes,null);
});

test('exact serialized bytes, tenant, operation and new contracts are mandatory',()=>{
  const source=baseRequest([{id:'Ю"\\recipient'}]);
  const valid=entry(source);
  const result=assistantPreflight(command([valid]));
  assert.equal(result.results[0].status,'fits');
  assert.equal(result.results[0].requestSha256,valid.requestSha256);
  for(const invalid of [
    command([{...valid,requestSha256:'0'.repeat(64)}]),
    command([{...valid,extra:true}]),
    {...command([valid]),extra:true},
    {...command([valid]),account:'baw-russia'},
    command(Array.from({length:17},()=>valid)),
    command([entry({...source,modelContextContract:undefined})]),
    command([entry({...source,researchPolicy:undefined})]),
  ])assert.throws(()=>assistantPreflight(invalid),{code:'ASSISTANT_PREFLIGHT_INVALID_REQUEST'});
});

test('large exact capture is held before the legacy 2 MiB assistant stdin limit',()=>{
  const source={...baseRequest(),ignoredCapturedField:'x'.repeat(2_200_000)};
  const captured=entry(source);
  assert.ok(Buffer.byteLength(captured.serializedRequest)>2*1024*1024);
  assert.ok(Buffer.byteLength(captured.serializedRequest)<2_400_000);
  assert.ok(Buffer.byteLength(prepareAssistantRequest(source).input)<550_000);
  const [result]=assistantPreflight(command([captured])).results;
  assert.deepEqual(result,{requestSha256:captured.requestSha256,status:'oversized',
    textBytes:null,boundedModelBytes:null,maxModelBytes:550_000});
});

test('image envelope can hold a batch whose text projection alone fits',()=>{
  const items=Array.from({length:100},(_,index)=>({id:`recipient-${index}`,postId:'post'}));
  const posts=[{id:'post',postKey:'post',attachments:Array.from({length:16},(_,index)=>({
    type:'photo',url:`https://cdn.example/${index}.png`}))}];
  let found;
  for(let count=20;count<=24&&!found;count++)for(let length=22000;length<=24000;length+=500){
    const source=baseRequest(items,posts);
    source.materials=Array.from({length:count},(_,index)=>({id:`rule-${index}`,kind:'rule',
      trust:'verified',text:'x'.repeat(length)}));
    const captured=entry(source);
    if(Buffer.byteLength(captured.serializedRequest)>550000)continue;
    const row=assistantPreflight(command([captured])).results[0];
    if(row.textBytes!==null&&row.textBytes<=550000&&row.boundedModelBytes>550000)found=row;
  }
  assert.ok(found,'a finite image envelope should independently cross the 550000-byte admission limit');
  assert.equal(found.status,'oversized');
});

test('no-image envelope bounds the actual staged model text without downloading',async()=>{
  const prepared=prepareAssistantRequest(baseRequest());
  const upper=Buffer.byteLength(prepared.input)+conservativeImageEvidenceBytes(prepared);
  let downloads=0;
  await stageAssistantImages(prepared,os.tmpdir(),{download:async()=>{downloads++;throw new Error('unexpected');}});
  assert.equal(downloads,0);
  assert.equal(prepared.payload.imageEvidence.status,'no_images_attached');
  assert.ok(Buffer.byteLength(prepared.input)<=upper);
});

test('16 shared post images and 100 escaped recipient IDs stay below envelope',async()=>{
  const items=Array.from({length:100},(_,index)=>({id:`Ю\\"recipient-${index}`,postId:'post'}));
  const posts=[{id:'post',postKey:'post',attachments:Array.from({length:16},(_,index)=>({
    type:'photo',url:`https://cdn.example/${index}.png`}))}];
  const prepared=prepareAssistantRequest(baseRequest(items,posts));
  const upper=Buffer.byteLength(prepared.input)+conservativeImageEvidenceBytes(prepared);
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-preflight-images-'));
  try{
    let downloads=0;
    const staged=await stageAssistantImages(prepared,home,{download:async()=>{
      downloads++;return {bytes:onePixelPng,mime:'image/png'};}});
    assert.equal(downloads,16);
    assert.equal(staged.manifest.length,16);
    assert.equal(staged.manifest[0].itemIds.length,100);
    assert.ok(Buffer.byteLength(prepared.input)<=upper);
  }finally{await fs.rm(home,{recursive:true,force:true});}
});

test('mixed success and failure imageEvidence stays below envelope',async()=>{
  const items=[{id:'a',postId:'one'},{id:'b',postId:'one'},{id:'c',postId:'two'}];
  const posts=[{id:'one',postKey:'one',attachments:[{type:'photo',url:'https://cdn.example/ok.png'}]},
    {id:'two',postKey:'two',attachments:[{type:'photo',url:'https://cdn.example/fail.png'}]}];
  const prepared=prepareAssistantRequest(baseRequest(items,posts));
  const upper=Buffer.byteLength(prepared.input)+conservativeImageEvidenceBytes(prepared);
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'communityhero-preflight-mixed-'));
  try{
    const staged=await stageAssistantImages(prepared,home,{download:async url=>{
      if(url.endsWith('/fail.png'))throw Object.assign(new Error('offline failure'),{code:'ENOTFOUND'});
      return {bytes:onePixelPng,mime:'image/png'};}});
    assert.equal(staged.manifest.length,1);
    assert.equal(staged.unavailableItems.length,1);
    assert.ok(Buffer.byteLength(prepared.input)<=upper);
  }finally{await fs.rm(home,{recursive:true,force:true});}
});

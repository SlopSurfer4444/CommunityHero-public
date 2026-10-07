import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import {connectorReadCapabilities,readCursor,encodeReadCursor,collectReadPage,runProvider} from './provider.mjs';
import {resolveAdapterPaths} from './config.mjs';

const binding={id:'baw-connection',workspaceId:'local-pilot',accountId:'BAW Russia',connector:'angryspace',revision:1,providerAccountId:'baw-russia'};
const account={accountKey:'baw-russia',providerAccountId:'baw-russia',displayName:'BAW Russia'};
const request={account:'baw-russia',mode:'open',binding};
const helpers={fastCommentAttachments:value=>value??[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>null,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)};

test('actual caps response advertises only the selected local read mapping through fixture gateway',async()=>{
  const paths=resolveAdapterPaths('baw-russia',{env:{},conveyorRoot:'C:/fixture/conveyor',providerRoot:'C:/fixture/provider',providerNode:'C:/fixture/node.exe'});
  const calls=[];
  const readFileFn=async file=>JSON.stringify(path.resolve(file)===path.resolve(paths.cardFile)
    ? {account:'baw-russia',provider_root:paths.providerRepo}
    : {scope:{stableAccountKey:'baw-russia',objectId:'first'},accountObjectIds:['first','second'],executionMode:{mode:'disabled'}});
  const moduleLoader=async name=>{
    calls.push(name);
    if(name==='transport/windows-native-companion-launcher.ts')return {loadWindowsNativeCompanionConfig:async()=>({})};
    if(name==='transport/fast-conveyor-gateway.ts')return {runFastConveyorRequest:async(_native,req)=>{
      assert.equal(req.operation,'capabilities');return {operation:'capabilities',account:'baw-russia'};
    }};
    assert.fail(`unexpected credential/runtime module ${name}`);
  };
  const result=await runProvider({...request,op:'caps'},{paths,readFileFn,moduleLoader,env:{}});
  assert.deepEqual(result.local.read,connectorReadCapabilities(request,['first','second'],account));
  assert.equal(result.local.runtimeSideEffects,'disabled');assert.equal(calls.length,2);
});

test('verified read mapping pins selected company and object scope and only implemented wire sizes',()=>{
  const caps=connectorReadCapabilities(request,['second','first'],account);
  assert.equal(caps.verified,true);assert.deepEqual(caps.binding,binding);
  assert.deepEqual(caps.objectScope,['first','second']);assert.deepEqual(caps.pageSizes,[10,100]);
  assert.equal(caps.preferredPageSize,100);assert.equal(caps.defaultPageSize,10);
  assert.equal(connectorReadCapabilities({...request,binding:{...binding,accountId:'LikeAvto'}},['first'],account).verified,false);
  assert.equal(connectorReadCapabilities({...request,binding:{...binding,connector:'vk'}},['first'],account).verified,false);
  assert.equal(connectorReadCapabilities({...request,binding:null},['first'],account).verified,false);
  assert.throws(()=>connectorReadCapabilities({...request,account:'likeavto'},['first'],account),{code:'ACCOUNT_SCOPE_MISMATCH'});
  assert.throws(()=>readCursor({...request,pageSize:200},['first'],'baw-russia'),{code:'INVALID_READ'});
});

test('old omitted, explicit ten and verified hundred remain separate opaque cursor contracts',()=>{
  for(const options of [{},{pageSize:10},{pageSize:100}]) {
    const {contract}=readCursor({...request,...options},['first'],'baw-russia');
    const cursor=encodeReadCursor(contract,{first:'next'});
    assert.deepEqual(readCursor({...request,...options,cursor},['first'],'baw-russia').contract,contract);
    for(const other of [{},{pageSize:10},{pageSize:100}].filter(value=>value.pageSize!==options.pageSize)) {
      assert.throws(()=>readCursor({...request,...other,cursor},['first'],'baw-russia'),{code:'INVALID_CURSOR'});
    }
    assert.throws(()=>readCursor({...request,...options,cursor,binding:{...binding,revision:2}},['first'],'baw-russia'),{code:'INVALID_CURSOR'});
    assert.throws(()=>readCursor({...request,...options,cursor},['second'],'baw-russia'),{code:'INVALID_CURSOR'});
    assert.throws(()=>readCursor({...request,...options,cursor,mode:'closed'},['first'],'baw-russia'),{code:'INVALID_CURSOR'});
  }
});

test('two fixture pages keep hundred logical contexts, exact options and full branch and media evidence',async()=>{
  const requests=[];
  const reader=()=>({
    listQueue:async query=>{
      requests.push(query);const offset=query.cursor?100:0;
      return {items:Array.from({length:100},(_,n)=>({id:`item-${offset+n}`,status:'new',text:`question ${offset+n}`})),nextCursor:offset?null:'next'};
    },
    getThreadContext:async id=>({item:{id,status:'new',text:`full comment ${id}`},parent:{id:'post',text:'complete post '.repeat(20),attachments:[{type:'photo',url:'https://fixture.invalid/image'}]},officialReplies:[]})
  });
  const first=await collectReadPage({...request,pageSize:100},['first'],reader,helpers,account);
  const second=await collectReadPage({...request,pageSize:100,cursor:first.cursor},['first'],reader,helpers,account);
  assert.deepEqual(requests.map(query=>query.limit),[100,100]);
  assert.equal(first.items.length+second.items.length,200);
  for(const page of [first,second]) {
    assert.deepEqual(page.readOptions,{pageSize:100});assert.equal(page.readCapabilities.verified,true);
    assert.deepEqual(page.readCapabilities.binding,binding);assert.equal(page.branches.length,100);
    assert.equal(page.posts[0].text,'complete post '.repeat(20));
    assert.deepEqual(page.posts[0].attachments,[{type:'photo',url:'https://fixture.invalid/image'}]);
    assert.ok(page.branches.every(branch=>branch.messages.some(message=>message.text.startsWith('full comment'))));
  }
  assert.equal(first.hasMore,true);assert.equal(second.hasMore,false);assert.equal(second.cursor,null);
});

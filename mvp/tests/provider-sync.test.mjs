import test from 'node:test';
import assert from 'node:assert/strict';
import {readContext,readWindow,readCursor,encodeReadCursor,windowDisposition,collectReadPage,mapRead,projectContext} from '../adapters/provider.mjs';

const window={since:'2026-09-19T12:00:00.000Z',until:'2026-09-21T12:00:00.000Z'};
const req={mode:'open',window,binding:{account:'likeavto',generation:3}};
const helpers={fastCommentAttachments:x=>x??[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>null,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)};
const item=(id,date,status='new')=>({id,created_at:date,status,text:id,author:{name:id}});
const context=i=>({item:i,parent:{id:'post',text:'post'},officialReplies:[]});
const cursorFor=value=>Buffer.from(JSON.stringify(value)).toString('base64url');

test('archive page size is bounded and pinned to its cursor without changing ordinary reads',async()=>{
  const archive={...req,mode:'closed',pageSize:100};
  const {contract}=readCursor(archive,['a']);
  const cursor=encodeReadCursor(contract,{a:'next'});
  assert.equal(readCursor({...archive,cursor},['a']).contract.pageSize,100);
  assert.throws(()=>readCursor({...req,mode:'closed',cursor},['a']),{code:'INVALID_CURSOR'});
  assert.throws(()=>readCursor({...archive,pageSize:101},['a']),{code:'INVALID_READ'});
  const limits=[];
  const reader=()=>({listQueue:async q=>{limits.push(q.limit);return {items:[],nextCursor:null};}});
  await collectReadPage(archive,['a'],reader,helpers);
  await collectReadPage(req,['a'],reader,helpers);
  assert.deepEqual(limits,[100,10]);
});

test('window validates UTC calendar dates and preserves exact bounds',()=>{
  assert.deepEqual(readWindow(window),window);
  for(const bad of [{...window,since:'2026-02-30T12:00:00.000Z'},{...window,since:'2026-09-19'},{...window,since:'2026-09-19T12:00:00+03:00'},{...window,until:'2026-09-18T12:00:00Z'},null])assert.throws(()=>readWindow(bad),{code:'INVALID_WINDOW'});
  assert.equal(windowDisposition(item('a',window.since),window),'inside');
  assert.equal(windowDisposition(item('a',window.until),window),'inside');
  for(const unknown of [null,'','not-a-date','2026-02-30T12:00:00Z','2026-09-19T12:00:00'])assert.equal(windowDisposition(item('a',unknown),window),'unknown');
});

test('cursor pins exact window, mode, account scope and snapshot binding',()=>{
  const {contract}=readCursor(req,['a','b']);
  const cursor=encodeReadCursor(contract,{a:'next',b:null});
  assert.deepEqual(readCursor({...req,cursor},['b','a']).positions,{a:'next',b:null});
  for(const changed of [{...req,mode:'closed'},{...req,window:{...window,since:'2026-09-19T12:00:00Z'}},{...req,binding:{account:'likeavto',generation:4}},{mode:'open'}])assert.throws(()=>readCursor({...changed,cursor},['a','b']),{code:'INVALID_CURSOR'});
  assert.throws(()=>readCursor({...req,cursor},['a','c']),{code:'INVALID_CURSOR'});
  assert.throws(()=>readCursor({...req,cursor:cursorFor({...contract,account:'other',positions:{a:null,b:null}})},['a','b']),{code:'INVALID_CURSOR'});
  const legacy=cursorFor({mode:'open',positions:{a:'next'}});
  assert.equal(readCursor({mode:'open',cursor:legacy},['a']).positions.a,'next');
  assert.throws(()=>readCursor({...req,cursor:legacy},['a']),{code:'INVALID_CURSOR'});
  assert.throws(()=>readCursor({mode:'open',binding:null,cursor:legacy},['a']),{code:'INVALID_CURSOR'});
});

test('filters before context fetch without assuming ordering; preserves cursor past old items',async()=>{
  const fetched=[],pages=[];
  const reader=()=>({listQueue:async q=>{pages.push(q);return q.cursor?{items:[item('later',window.until)],nextCursor:null}:{items:[item('old','2020-01-01T00:00:00Z'),item('inside',window.since),item('future','2030-01-01T00:00:00Z')],nextCursor:'second'};},getThreadContext:async id=>{fetched.push(id);return context(item(id,window.until));}});
  const first=await collectReadPage(req,['a'],reader,helpers);
  assert.deepEqual(fetched,['inside']);assert.equal(pages[0].reverse,false);assert.equal(first.scannedCount,3);assert.equal(first.outsideWindowCount,2);assert.equal(first.hasMore,true);assert.equal(first.coverage.complete,false);
  const second=await collectReadPage({...req,cursor:first.cursor},['a'],reader,helpers);
  assert.equal(pages[1].cursor,'second');assert.equal(second.items[0].providerItemId,'later');assert.equal(second.hasMore,false);assert.equal(second.coverage.complete,true);
});

test('unknown dates remain visible and schema failures make coverage incomplete',async()=>{
  const reader=()=>({listQueue:async()=>({items:[item('unknown',null),item('broken',window.since)],nextCursor:null}),getThreadContext:async id=>{if(id==='broken')throw Object.assign(new Error(),{code:'RESPONSE_SCHEMA_ERROR'});return context(item(id,null));}});
  const result=await collectReadPage(req,['a'],reader,helpers);
  assert.equal(result.unknownDateCount,1);assert.equal(result.items[0].createdAt,null);assert.equal(result.skipped.length,1);assert.equal(result.coverage.complete,false);assert.equal(result.hasMore,false);
});

test('projection preserves provider identities, target attachments and proven reply edges',()=>{
  const c=context({...item('target',window.until),attachments:[{url:'target-media'}],reply_to_item_id:'parent'});
  c.replyTo=item('parent',window.since);
  c.officialReplies=[{...item('official-a',window.until),reply_to_item_id:'target'},{...item('official-b',window.until),reply_to_item_id:'other'},{...item('official-c',window.until)}];
  const result=mapRead([projectContext(c,'a',helpers)],{}),messages=result.branches[0].messages;
  assert.deepEqual(messages.map(x=>x.attachments),[[],[{url:'target-media'}],[],[],[]]);
  assert.deepEqual(messages.slice(2).map(x=>x.parentId),['comment-a-target','comment-a-other',null]);
  assert.deepEqual(messages.slice(2).map(x=>x.replyToProviderItemId),['target','other',null]);
  assert.equal(result.items[0].providerObjectId,'a');assert.equal(result.items[0].replyToProviderItemId,'parent');
  assert.equal(result.branches[0].contextComplete,false);assert.ok(result.branches[0].unavailableReason);
});

test('unknown dates on earlier pages prevent final cursor exhaustion claiming complete coverage',async()=>{
  const reader=()=>({listQueue:async q=>q.cursor?{items:[],nextCursor:null}:{items:[item('unknown',null)],nextCursor:'next'},getThreadContext:async()=>context(item('unknown',null))});
  const first=await collectReadPage(req,['a'],reader,helpers);
  const last=await collectReadPage({...req,cursor:first.cursor},['a'],reader,helpers);
  assert.equal(last.unknownDateCount,0);assert.equal(last.coverage.incompleteCount,1);assert.equal(last.coverage.complete,false);assert.equal(last.hasMore,false);
});

test('queue status races ingest the newer observed status in either direction',async()=>{
  for(const [mode,before,after] of [['open','new','closed'],['closed','closed','inprogress']]){
    const reader=()=>({listQueue:async()=>({items:[item('changed',window.until,before)],nextCursor:null}),getThreadContext:async()=>context(item('changed',window.until,after))});
    const result=await collectReadPage({...req,mode},['a'],reader,helpers);
    assert.equal(result.items.length,1);assert.equal(result.items[0].providerStatus,after);assert.equal(result.items[0].workflow,after==='closed'?'closed':'attention');
  }
});

test('brand reply edges resolve existing official message IDs even for forward references',()=>{
  const c=context(item('target',window.until));
  c.officialReplies=[{...item('child',window.until),reply_to_item_id:'parent-brand'},{...item('parent-brand',window.until),reply_to_item_id:'target'}];
  const result=mapRead([projectContext(c,'a',helpers)],{}),messages=result.branches[0].messages;
  assert.equal(messages[1].id,'official-a-child');assert.equal(messages[1].parentId,'official-a-parent-brand');
  assert.equal(messages[1].providerItemId,'child');assert.equal(messages[1].providerObjectId,'a');assert.equal(messages[1].replyToProviderItemId,'parent-brand');
  assert.equal(messages[2].parentId,'comment-a-target');
});


test('direction is explicitly pinned and defaults preserve legacy mode-only behavior',()=>{
  assert.equal(readCursor(req,['a']).contract.reverse,false);
  assert.equal(readCursor({...req,mode:'closed'},['a']).contract.reverse,false);
  assert.equal(readCursor({mode:'closed'},['a']).contract.reverse,true);
  const {contract}=readCursor({...req,reverse:true},['a']);
  const cursor=encodeReadCursor(contract,{a:'next'});
  assert.equal(readCursor({...req,reverse:true,cursor},['a']).contract.reverse,true);
  assert.throws(()=>readCursor({...req,reverse:false,cursor},['a']),{code:'INVALID_CURSOR'});
  assert.throws(()=>readCursor({...req,reverse:'false'},['a']),{code:'INVALID_READ'});
});

test('page date bounds include excluded dates and parse Unix queue dates',async()=>{
  const reader=()=>({listQueue:async()=>({items:[item('old','2020-01-01T00:00:00Z'),item('unix',Date.parse(window.until)/1000),item('invalid',null)],nextCursor:null}),getThreadContext:async id=>context(item(id,null))});
  const result=await collectReadPage(req,['a'],reader,helpers);
  assert.deepEqual(result.pageDateBounds,{min:'2020-01-01T00:00:00.000Z',max:window.until});
  assert.equal(result.reverse,false);
});


test('exact context snapshot is opt-in, validates identity, and preserves dispatcher default',async()=>{
  const request={itemId:'target',objectId:'a'};
  const reader=()=>({getThreadContext:async()=>context(item('target',window.until,'closed'))});
  const legacy=await readContext(request,['a'],reader,helpers);
  assert.equal(legacy.itemId,'target');assert.equal(legacy.items,undefined);
  const snapshot=await readContext({...request,snapshot:true},['a'],reader,helpers);
  assert.equal(snapshot.items.length,1);assert.equal(snapshot.items[0].providerStatus,'closed');
  assert.deepEqual(snapshot.target,request);assert.equal(snapshot.branches[0].contextComplete,false);
  await assert.rejects(()=>readContext({...request,objectId:'other',snapshot:true},['a'],reader,helpers),{code:'INVALID_TARGET'});
  await assert.rejects(()=>readContext(request,['a'],()=>({getThreadContext:async()=>context(item('different',window.until))}),helpers),{code:'TARGET_IDENTITY_MISMATCH'});
});

test('explicitly deleted provider record maps to deleted rather than open work',async()=>{
  const reader=()=>({getThreadContext:async()=>context(item('target',window.until,'deleted'))});
  const snapshot=await readContext({itemId:'target',objectId:'a',snapshot:true},['a'],reader,helpers);
  assert.equal(snapshot.items[0].providerStatus,'deleted');
  assert.equal(snapshot.items[0].workflow,'deleted');
});

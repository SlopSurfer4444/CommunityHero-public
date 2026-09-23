import test from 'node:test';
import assert from 'node:assert/strict';
import {readHead,readStatuses,statusTargets,projectContext,mapRead} from './provider.mjs';

test('status reads are exact, bounded, and never turn a missing target into closed',async()=>{
 let running=0,max=0;
 const result=await readStatuses({targets:Array.from({length:8},(_,i)=>({objectId:'11391',itemId:String(i)}))},['11391'],()=>({async getItem(id){
  running++;max=Math.max(max,running);await new Promise(r=>setTimeout(r,2));running--;
  if(id==='3')throw Object.assign(Error('private URL must not leak'),{code:'ITEM_NOT_FOUND'});
  if(id==='4')return {id:'someone-else',status:'closed'};
  return {id,status:id==='0'?'closed':'new'};
 },getThreadContext(){throw Error('Expensive context must not be read');}}));
 assert.equal(max,3);assert.equal(result.items.length,6);assert.equal(result.errors.length,2);
 assert.equal(result.items.find(i=>i.itemId==='0').status,'closed');
 assert.ok(!result.items.some(i=>['3','4'].includes(i.itemId)));assert.ok(!JSON.stringify(result).includes('private URL'));
});
test('targets are rejected before credential-backed reads',()=>{
 for(const targets of [[{objectId:'other',itemId:'x'}],[{objectId:'11391',itemId:'../x'}],Array.from({length:101},(_,i)=>({objectId:'11391',itemId:String(i)})),[{objectId:'11391',itemId:'x'},{objectId:'11391',itemId:'x'}]])assert.throws(()=>statusTargets({targets},['11391']));
});
test('head reads newest opposite end only when partial, preserves coverage and object failures',async()=>{
 const calls=[];
 const result=await readHead({},['11391','11390','11341'],objectId=>({async listQueue(req){
  calls.push([objectId,req]);if(objectId==='11341')throw Object.assign(Error('bad'),{code:'ACCOUNT_SCOPE_MISMATCH'});
  return {items:[{id:req.reverse?'newest':'oldest',status:'new'}],count:objectId==='11391'?230:1,nextCursor:objectId==='11391'?'more':null};
 }}));
 assert.equal(calls.length,4);assert.equal(result.items.length,3);assert.equal(result.hasMore,true);
 assert.equal(result.errors[0].objectId,'11341');assert.ok(calls.every(([,r])=>r.limit===100));
 assert.equal(result.coverage.find(c=>c.objectId==='11390').hasMore,false);
});
test('full context carries the request observation time for ordering against fast status',()=>{
 const at='2026-09-22T11:00:00.000Z';
 const r=projectContext({item:{id:'c',status:'new',text:'Text',author:{name:'A'}},parent:{id:'p'},officialReplies:[]},'11391',{
  fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>null,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)
 },at);
 const [item]=mapRead([r],{}).items;
 assert.equal(item.contextObservedAt,at);assert.equal(item.providerStatusObservedAt,at);
});

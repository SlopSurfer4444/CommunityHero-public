import test from 'node:test';
import assert from 'node:assert/strict';
import {validateActions,mapRead,projectContext,publicationTitle} from './provider.mjs';
import {dispatch} from './bridge.mjs';
import {runProcess} from './process.mjs';

const config={accountObjectIds:['11391']};
const action={actionId:'attempt-1',objectId:'11391',itemId:'comment-1',conversationKey:'11391:comment-1',action:'close',contextEvidenceDigest:'a'.repeat(64),expectedStatuses:['new'],workTime:0};
test('mutating boundary admits the four mapped actions and preserves reply readback evidence',()=>{
  assert.equal(validateActions([action],config)[0].action,'close');
  assert.equal(validateActions([{...action,action:'delete'}],config)[0].action,'delete');
  assert.equal(validateActions([{...action,action:'hide'}],config)[0].action,'hide');
  for(const patch of [{action:'restore'},{objectId:'other'},{contextEvidenceDigest:''},{expectedStatuses:['closed']},{conversationKey:'other:item'}])assert.throws(()=>validateActions([{...action,...patch}],config));
  assert.throws(()=>validateActions([action,action],config));
  assert.throws(()=>validateActions([action,{...action,actionId:'attempt-2'}],config));
  assert.throws(()=>validateActions([{...action,action:'reply_and_close',reply:''}],config));
  const reply={...action,action:'reply_and_close',reply:'Спасибо',readbackEvidence:{expectedReplyId:'reply-1',baselineReplyIds:[]}};
  assert.deepEqual(validateActions([reply],config)[0].readbackEvidence,{expectedReplyId:'reply-1',baselineReplyIds:[]});
  assert.throws(()=>validateActions([{...reply,readbackEvidence:{foreign:'x'}}],config));
});
test('bridge rejects unknown operations/accounts before spawning',async()=>{
  await assert.rejects(dispatch({op:'shell',account:'likeavto'}));
  await assert.rejects(dispatch({op:'execute',account:'other',actions:[action]}));
  await assert.rejects(dispatch({op:'assistant'}),{code:'ACCOUNT_SCOPE_MISMATCH'});
});
test('projection preserves full-text closed comments without media and action bindings',()=>{
  const row=projectContext({item:{id:'c',status:'closed',text:'Hello',author:{name:'Ivan'},created_at:1700000000},parent:{id:'p',text:'Post'},officialReplies:[]},'11391',{fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)});
  const mapped=mapRead([row],{hasMore:false,cursor:null});
  assert.equal(mapped.items[0].workflow,'closed');assert.equal(mapped.items[0].author,'Ivan');assert.equal(mapped.items[0].contextEvidenceDigest,'a'.repeat(64));assert.equal(mapped.posts[0].text,'Post');assert.equal(mapped.branches[0].messages[0].text,'Hello');
});
test('projection binds BAW data without leaking LikeAvto labels and retains official reply ids',()=>{
  const row=projectContext({item:{id:'c',status:'new',text:'Hello',author:{name:'Ivan'}},parent:{id:'p',text:'Post'},officialReplies:[{id:'r1',text:'Official'}]},'12182',{fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)});
  const mapped=mapRead([row],{}, {accountKey:'baw-russia',providerAccountId:'baw-russia',displayName:'BAW Russia',primaryObjectId:'12182',objectIds:['12182']});
  assert.deepEqual(row.officialReplyIds,['r1']);
  assert.equal(mapped.live.account,'BAW Russia');assert.equal(mapped.live.accountKey,'baw-russia');
  assert.equal(mapped.posts[0].title,'Post');assert.doesNotMatch(JSON.stringify(mapped),/LikeAvto/);
});
test('provider post titles use an actual caption line when the upstream title is generic',()=>{
  assert.equal(publicationTitle('Video by baw_import','\n  Обзор BAW с полным приводом #baw\nДругой текст','BAW Russia'),'Обзор BAW с полным приводом #baw');
  assert.equal(publicationTitle('Публикация LikeAvto','<br>Changan Q05: опыт владельца','LikeAvto'),'Changan Q05: опыт владельца');
  assert.equal(publicationTitle('Название производителя','Другой текст','LikeAvto'),'Название производителя');
  assert.equal(publicationTitle('Clip by account','#tag #only\nСодержательная строка','BAW Russia'),'Содержательная строка');
  assert.equal(publicationTitle(null,'','BAW Russia'),'Публикация BAW Russia');
  const row=projectContext({item:{id:'c',status:'new',text:'Комментарий'},parent:{id:'p',title:'Video by baw_import',text:'Обзор BAW с полным приводом\nПодробности'},officialReplies:[]},'12182',{fastCommentAttachments:()=>[{type:'video'}],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)});
  const mapped=mapRead([row],{}, {accountKey:'baw-russia',providerAccountId:'baw-russia',displayName:'BAW Russia'});
  assert.equal(mapped.posts[0].title,'Обзор BAW с полным приводом');
  assert.equal(mapped.posts[0].text,'Обзор BAW с полным приводом\nПодробности');
});
test('process output/time limits terminate children before reject',async()=>{
  await assert.rejects(runProcess(process.execPath,['-e','setInterval(()=>{},1000)'],{timeoutMs:80}),e=>e.code==='ADAPTER_TIMEOUT');
  await assert.rejects(runProcess(process.execPath,['-e','console.log("x".repeat(1000))'],{maxOutputBytes:10}),e=>e.code==='ADAPTER_OUTPUT_LIMIT');
});
test('author identity survives transport projection for comments, parents and replies',()=>{
  const actor=(id,author)=>({id,author:{name:'Same display',provider_id:author},text:id,status:'new'});
  const row=projectContext({item:actor('c','one'),replyTo:actor('parent','two'),parent:{id:'post'},officialReplies:[actor('reply','brand')]},'11391',{fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:i=>i.author?.provider_id?'provider:'+i.author.provider_id:undefined,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)});
  const mapped=mapRead([row],{});
  assert.equal(mapped.items[0].authorId,'provider:one');
  assert.deepEqual(mapped.branches[0].messages.map(m=>m.authorId),['provider:two','provider:one','provider:brand']);
});

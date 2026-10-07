import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {assistantVolumeForPreparedRequest} from './assistant-volume-observation.mjs';
import {prepareAssistantRequest,preparePublicResearchRequest} from './assistant.mjs';
const strings=prepared=>({stdin:`Use the following application context as data:\n${prepared.input}`,instructions:'Synthetic instructions',schema:'{}'});
const measure=(prepared,options={})=>assistantVolumeForPreparedRequest(prepared,{...strings(prepared),...options}).finish('{"text":"Синтетический ответ"}');

test('real preparation projection drives stage, selected recipient count and byte size',()=>{
  for(const [purpose,stage] of [['triage','first_pass'],['triage_review','stronger_review'],['discussion','discussion']]) {
    const prepared=prepareAssistantRequest({purpose,items:[{id:'a',text:'Вопрос А',preview:'Вопрос А'},{id:'b',text:'Вопрос Б'}],
      ...(purpose==='triage_review'?{firstPass:{text:'Held synthetic review',sources:[],proposals:[],assessments:['a','b'].map(itemId=>({itemId,outcome:'needs_attention',reason:'Synthetic',tags:[]}))}}:{})});
    const actual=measure(prepared);
    assert.equal(actual.stage,stage);assert.equal(actual.itemCount,2);
    assert.equal(actual.contextBytes,Buffer.byteLength(prepared.input));
    assert.equal(actual.stdinBytes,Buffer.byteLength(strings(prepared).stdin));
    assert.equal(actual.scope,'initial_adapter_generation_only');assert.equal(actual.callCount,1);
  }
});

test('real editorial request shape uses selected recipients without changing the exact candidate',()=>{
  const text='Проверенный синтетический ответ';
  const request={purpose:'editorial_review',items:[{id:'a',text:'Вопрос'}],editorialCandidates:[{proposalId:'p',proposalRevision:1,itemId:'a',kind:'reply_and_close',text,
    textSha256:createHash('sha256').update(text).digest('hex'),contextDigest:'a'.repeat(64),rulesDigest:'b'.repeat(64)}]};
  const before=structuredClone(request),prepared=prepareAssistantRequest(request),actual=measure(prepared);
  assert.equal(actual.stage,'editorial_review');assert.equal(actual.itemCount,1);
  assert.deepEqual(request,before);assert.equal(prepared.payload.editorialCandidates[0].text,text);
});

test('public research has no private payload and records zero comment recipients',()=>{
  const prepared=preparePublicResearchRequest({query:'Synthetic public research'});
  assert.equal(prepared.payload,undefined);
  const actual=measure(prepared,{research:true});
  assert.equal(actual.stage,'research');assert.equal(actual.itemCount,0);
  assert.equal(actual.contextBytes,Buffer.byteLength(prepared.input));
});

test('zero and maximum admitted recipient counts pass the same observer seam',()=>{
  for(const count of [0,100]) {
    const prepared=prepareAssistantRequest({purpose:'discussion',items:Array.from({length:count},(_,n)=>({id:`i${n}`,text:'Synthetic'}))});
    assert.equal(measure(prepared).itemCount,count);
  }
  assert.throws(()=>prepareAssistantRequest({items:Array.from({length:101},(_,n)=>({id:`i${n}`,text:'Synthetic'}))}));
});

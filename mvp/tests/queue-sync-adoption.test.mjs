import test from 'node:test';
import assert from 'node:assert/strict';
import { CliError } from '../cli/client.mjs';
import { freshSync, eligible, unknownQueueBlockers } from '../cli/queue.mjs';

const conflict = () => new CliError('A job of this kind is already running', { code:'STALE_OR_CONFLICT', status:409 });
const snapshot = jobs => ({account:'BAW Russia', jobs, sync:{openCoverage:{done:true,coverageComplete:true}},items:[]});
test('exact sync conflict adopts one account-bound job and only polls it',async()=>{
 let posts=0,reads=0;
 const client={account:'baw-russia',sync:async()=>{posts++;throw conflict();},
  bootstrap:async()=>snapshot([{id:'background',kind:'sync',status:'running'}]),
  getJob:async id=>{assert.equal(id,'background');reads++;return {id,status:'completed'};}};
 const {state}=await freshSync(client,{},null,{maxPolls:2,pollMs:0});
 assert.equal(posts,1);assert.equal(reads,1);assert.equal(state.syncAdopted,true);assert.equal(state.coverage.complete,true);
});
test('unrelated conflict and unknown POST outcome are never adopted',async()=>{
 for(const error of [new CliError('Other conflict',{code:'STALE_OR_CONFLICT',status:409}),new CliError('unknown',{code:'UNKNOWN_MUTATION_OUTCOME'})]){
  let reads=0; const client={account:'baw-russia',sync:async()=>{throw error;},bootstrap:async()=>{reads++;return snapshot([]);}};
  await assert.rejects(freshSync(client,{},null,{}),e=>e===error);assert.equal(reads,0);
 }
});
test('missing, duplicate, foreign-account and non-sync jobs cannot be adopted',async()=>{
 for(const jobs of [[],[{id:'a',kind:'prepare',status:'running'}],[{id:'a',kind:'sync',status:'completed'}],
  [{id:'a',kind:'sync',status:'running'},{id:'b',kind:'sync',status:'queued'}],
  [{id:'a',kind:'sync',status:'running',account:'likeavto'}]]){
  await assert.rejects(freshSync({account:'baw-russia',sync:async()=>{throw conflict();},bootstrap:async()=>snapshot(jobs)}, {}, null,{}));
 }
 await assert.rejects(freshSync({account:'baw-russia',sync:async()=>{throw conflict();},bootstrap:async()=>({...snapshot([]),account:'likeavto'})},{},null,{}),{code:'WRONG_ACCOUNT'});
});
test('whole unknown conversation is excluded even when original item is absent',()=>{
 const item=(id,conversationKey)=>({id,conversationKey,workflow:'attention',providerStatus:'new'});
 const s={items:[item('sibling','thread'),item('safe','other')],operations:[{itemId:'old-missing',status:'unknown',target:{conversationKey:'thread'}}]};
 assert.deepEqual(eligible(s,new Set()).map(x=>x.id),['safe']);
 assert.deepEqual([...unknownQueueBlockers(s)],['sibling']);
 s.operations[0].target={};s.operations[0].action={conversationKey:'thread'};
 assert.deepEqual(eligible(s,new Set()).map(x=>x.id),['safe']);
});
test('unknown proposal uses local item conversation without blocking unrelated work',()=>{
 const s={items:[{id:'old',conversationKey:'t'},{id:'new',conversationKey:'t',workflow:'attention'},{id:'safe',conversationKey:'u',workflow:'attention'}],proposals:[{itemId:'old',status:'unknown'}]};
 assert.deepEqual(eligible(s,new Set()).map(x=>x.id),['safe']);
});

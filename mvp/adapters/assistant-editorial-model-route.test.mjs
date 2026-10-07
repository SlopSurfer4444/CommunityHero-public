import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {prepareAssistantRequest,preparePublicResearchRequest,assistantCliArgs,reviewModelCatalog,
  assistantLaneForRequest,withAssistantLane,editorialMetadata,editorialInstructions,
  generationMetadata,validateEditorialResult,admitAssistantEvents} from './assistant.mjs';

const sha=x=>createHash('sha256').update(x).digest('hex');
const candidate={proposalId:'p',proposalRevision:2,itemId:'i',kind:'reply_and_close',text:'Спасибо!',
  textSha256:sha('Спасибо!'),contextDigest:sha('context'),rulesDigest:sha('rules')};
const request=profile=>({purpose:'editorial_review',account:'baw-russia',items:[{id:'i',text:'История'}],
  editorialCandidates:[candidate],...(profile===undefined?{}:{editorialModelProfile:profile})});
const catalog={models:['gpt-6.1-sol','gpt-6-astra','gpt-6-sol','gpt-6-luna'].map(slug=>({slug,
  supported_reasoning_levels:[{effort:'low'},{effort:'high'}],input_modalities:['text','image'],
  experimental_supported_tools:['unsafe'],tool_mode:'code',apply_patch_tool_type:'freeform'}))};
const verdict={text:'Reviewed',sources:[],proposals:[],editorial:[{...candidate,decision:'accept',reason:'Exact check',proposedText:null,
  checks:{intent:'pass',companyRules:'pass',factualScope:'pass'}}]};
delete verdict.editorial[0].kind;delete verdict.editorial[0].text;

test('finite captured editorial profile cannot route other purposes or fall back to Luna',()=>{
 const old=prepareAssistantRequest(request()),next=prepareAssistantRequest(request('sol61_high_v2'));
 assert.equal(old.editorialModelProfile,undefined);assert.equal(next.editorialModelProfile,'sol61_high_v2');
 assert.equal(next.payload.editorialModelProfile,'sol61_high_v2');assert.notEqual(sha(old.input),sha(next.input));
 for(const p of ['sol_high_v1','gpt-6-luna','gpt-6-sol','astra_high_v1','',null,{},true])
  assert.throws(()=>prepareAssistantRequest(request(p)),{code:'ASSISTANT_INVALID_REQUEST'});
 for(const purpose of ['discussion','triage','triage_review'])
  assert.throws(()=>prepareAssistantRequest({...request('sol61_high_v2'),purpose}),{code:'ASSISTANT_INVALID_REQUEST'});
 assert.throws(()=>preparePublicResearchRequest({query:'public query',editorialModelProfile:'sol61_high_v2'}),{code:'ASSISTANT_INVALID_REQUEST'});
 assert.equal(assistantLaneForRequest(old),'preparation');assert.equal(assistantLaneForRequest(next),'editorial-sol-high-v1');
});

test('actual requested route, narrowed catalogue and metadata consistently bind Sol high without extra tools',()=>{
 const p=prepareAssistantRequest(request('sol61_high_v2')),home=path.resolve('synthetic-home');
 const args=assistantCliArgs(home,false,[],false,p.editorialModelProfile);
 assert.equal(args[args.indexOf('-m')+1],'gpt-6.1-sol');assert.ok(args.includes('model_reasoning_effort="high"'));
 assert.ok(args.includes('web_search="disabled"'));assert.ok(args.includes('--ignore-user-config'));
 assert.ok(!args.includes('standalone_web_search'));
 const selected=reviewModelCatalog(catalog,p.editorialModelProfile);
 assert.deepEqual(selected.models.map(m=>m.slug),['gpt-6.1-sol']);assert.deepEqual(selected.models[0].experimental_supported_tools,[]);
 assert.equal(selected.models[0].tool_mode,null);assert.equal(selected.models[0].apply_patch_tool_type,null);
 const metadata={...generationMetadata(p.input,false,12,'baw-russia'),...editorialMetadata(p,editorialInstructions('baw-russia'))};
 assert.equal(metadata.model,'gpt-6.1-sol');assert.equal(metadata.reasoningEffort,'high');
 assert.equal(metadata.inputSha256,sha(p.input));assert.equal(metadata.editorialContract,'communityhero-editorial-v1');
 assert.throws(()=>assistantCliArgs(home,true,[],false,p.editorialModelProfile),{code:'ASSISTANT_INVALID_REQUEST'});
 assert.throws(()=>assistantCliArgs(home,false,[],true,p.editorialModelProfile),{code:'ASSISTANT_INVALID_REQUEST'});
 for(const bad of [{models:catalog.models.filter(m=>m.slug!=='gpt-6.1-sol')},{models:[{slug:'gpt-6.1-sol',input_modalities:['text','image'],supported_reasoning_levels:[{effort:'low'}]}]},
  {models:[{slug:'gpt-6.1-sol',input_modalities:['text'],supported_reasoning_levels:[{effort:'high'}]}]}])
  assert.throws(()=>reviewModelCatalog(bad,p.editorialModelProfile),{code:'ASSISTANT_UNAVAILABLE'});
});

test('absent profile uses Sol 6.1 low and exact editorial binding stays mandatory for either route',()=>{
 const p=prepareAssistantRequest(request()),home=path.resolve('synthetic-home');
 const args=assistantCliArgs(home);assert.equal(args[args.indexOf('-m')+1],'gpt-6.1-sol');assert.ok(args.includes('model_reasoning_effort="low"'));
 const metadata=editorialMetadata(p,editorialInstructions('baw-russia'));assert.equal(metadata.model,'gpt-6.1-sol');assert.equal(metadata.reasoningEffort,'low');
 assert.equal(reviewModelCatalog(catalog).models[0].slug,'gpt-6.1-sol');
 for(const profile of [undefined,'sol61_high_v2']){
  const prepared=prepareAssistantRequest(request(profile));assert.equal(validateEditorialResult(verdict,prepared).editorial[0].decision,'accept');
  for(const patch of [{itemId:'foreign'},{proposalRevision:3},{textSha256:sha('another text')},{contextDigest:sha('new context')},{rulesDigest:sha('other rules')}])
   assert.throws(()=>validateEditorialResult({...verdict,editorial:[{...verdict.editorial[0],...patch}]},prepared),{code:'ASSISTANT_INVALID_RESPONSE'});
  assert.throws(()=>admitAssistantEvents(JSON.stringify({type:'item.completed',item:{id:'x',type:'web_search',action:{type:'search'},query:'not allowed'}})),{code:'ASSISTANT_ISOLATION_FAILED'});
 }
});

test('new editorial lane overlaps preparation but enforces its own owner and cleans only its run',async()=>{
 const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-editorial-route-'));let release,entered;
 const enteredPromise=new Promise(resolve=>entered=resolve),hold=new Promise(resolve=>release=resolve);
 let preparation;
 try{
  preparation=withAssistantLane(base,'preparation',async home=>{entered();await hold;return home;});
  await enteredPromise;
  const editorial=await withAssistantLane(base,'editorial-sol-high-v1',async home=>{
   assert.equal(path.dirname(home),path.join(base,'editorial-sol-high-v1'));
   assert.ok((await fs.stat(path.join(base,'preparation','model.lock'))).isFile());
   await assert.rejects(withAssistantLane(base,'editorial-sol-high-v1',async()=>assert.fail('duplicate editorial')), {code:'ASSISTANT_BUSY'});
   await assert.rejects(withAssistantLane(base,'preparation',async()=>assert.fail('duplicate preparation')), {code:'ASSISTANT_BUSY'});
   return home;
  });
  assert.deepEqual(await fs.readdir(path.join(base,'editorial-sol-high-v1')),[]);
  assert.ok((await fs.stat(path.join(base,'preparation','model.lock'))).isFile());
  release();assert.notEqual(await preparation,editorial);assert.deepEqual(await fs.readdir(path.join(base,'preparation')),[]);
 }finally{release?.();await preparation?.catch(()=>{});if(path.dirname(base)===os.tmpdir()&&path.basename(base).startsWith('ch-editorial-route-'))await fs.rm(base,{recursive:true,force:true});}
});

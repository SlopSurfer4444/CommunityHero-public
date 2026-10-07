import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {randomUUID} from 'node:crypto';
import {fileURLToPath,pathToFileURL} from 'node:url';
import path from 'node:path';
import {fixtureSnapshot,syntheticModel,fixtureHash} from './conductor-acceptance-fixture.mjs';
test('1200 synthetic recipients stay exact and company-scoped across repeated public posts',()=>{
  for(const account of ['likeavto','baw-russia']){
    const value=fixtureSnapshot(account);
    assert.equal(value.items.length,1200);assert.equal(new Set(value.items.map(item=>item.id)).size,1200);
    assert.equal(value.posts.length,6);assert.equal(value.items.filter(item=>item.postId===value.posts[0].id).length,200);
    assert.deepEqual(value.queueAccounting.observations,value.items.map(({objectId,itemId})=>({objectId,itemId,contextRequired:true})));assert.equal(value.queueAccounting.version,1);assert.equal(value.queueAccounting.duplicateQueueCount,0);assert.deepEqual(value.skipped,[]);
    assert.ok(value.items.every(item=>item.id.includes(account)&&Number.isFinite(Date.parse(item.createdAt))));
  }
  assert.throws(()=>fixtureSnapshot('foreign'));
});
test('actual fake bridge records repeated dispatch twice, without hiding retries',async()=>{
  const mvp=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..'),root=path.join(mvp,'runs/engine-v80-20261001/acceptance',`fixture-${randomUUID()}`,'fixture');
  await mkdir(root,{recursive:true});await writeFile(path.join(root,'scenario.json'),JSON.stringify({itemCount:1}));
  const item=fixtureSnapshot('likeavto',1).items[0],action={actionId:'one-operation',action:'reply_and_close',itemId:item.itemId,objectId:item.objectId,text:'Synthetic reply'};
  async function invoke(request){const child=spawn(process.execPath,[path.join(mvp,'tests/conductor-acceptance-bridge.mjs')],{windowsHide:true,
    env:{...process.env,COMMUNITYHERO_CONDUCTOR_FIXTURE_ROOT:root},stdio:['pipe','pipe','pipe']});let output='',error='';
    child.stdout.on('data',chunk=>output+=chunk);child.stderr.on('data',chunk=>error+=chunk);child.stdin.end(JSON.stringify(request));
    const code=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',resolve);});assert.equal(code,0,error);return JSON.parse(output);
  }
  const execution={operation:'execute',account:'likeavto',actions:[action]};
  assert.equal((await invoke(execution)).result.results[0].status,'verified');assert.equal((await invoke(execution)).result.results[0].status,'verified');
  assert.equal((await invoke({operation:'readback',account:'likeavto',actions:[action]})).result.results[0].status,'verified');
  assert.equal((await invoke({operation:'readback',account:'likeavto',actions:[{...action,actionId:'never-dispatched'}]})).result.results[0].status,'unknown');
  await writeFile(path.join(root,'scenario.json'),JSON.stringify({itemCount:1,unknownProviderIds:[item.itemId]}));
  assert.equal((await invoke({operation:'readback',account:'likeavto',actions:[action]})).result.results[0].status,'unknown');
  assert.equal((await invoke({operation:'read',account:'likeavto'})).result.items[0].providerStatus,'closed');
  const effects=(await readFile(path.join(root,'effects.jsonl'),'utf8')).trim().split(/\r?\n/).map(JSON.parse);
  assert.equal(effects.length,2);assert.equal(effects[0].action.actionId,effects[1].action.actionId);
});
test('synthetic editorial revise then fresh accept bind exact changed bytes and revision',()=>{
  const original={proposalId:'p1',proposalRevision:1,itemId:'item-likeavto-0',kind:'reply_and_close',text:'Synthetic initial reply item-likeavto-0',
    textSha256:fixtureHash('Synthetic initial reply item-likeavto-0'),contextDigest:'a'.repeat(64),rulesDigest:'b'.repeat(64)};
  const request={account:'likeavto',purpose:'editorial_review',editorialModelProfile:'sol61_high_v2',items:[{id:original.itemId}],editorialCandidates:[original]};
  const first=syntheticModel(request,{reviseItemIds:[original.itemId]});assert.equal(first.editorial[0].decision,'revise');
  const changed={...original,text:first.editorial[0].proposedText,proposalRevision:2,textSha256:fixtureHash(first.editorial[0].proposedText)};
  const fresh=syntheticModel({...request,editorialCandidates:[changed]},{reviseItemIds:[original.itemId]});
  assert.equal(fresh.editorial[0].decision,'accept');assert.equal(fresh.editorial[0].proposalRevision,2);assert.equal(fresh.editorial[0].textSha256,changed.textSha256);
  assert.equal(fresh.runMetadata.model,'gpt-6.1-sol');assert.equal(fresh.runMetadata.reasoningEffort,'high');
});
test('fixture drives actual adapter admission and does not manufacture actionable held recipients',()=>{
  const snapshot=fixtureSnapshot('likeavto',4),request={...snapshot,account:'likeavto',purpose:'triage',preparationMode:'single_pass_v1',
    responseContract:'compact_decisions_v1',modelContextContract:'shared_moderation_v1',researchPolicy:'context_sufficient_v1',
    researchLimitContract:'uncapped_evidence_v1',recoveryEvidenceContract:'held_candidates_v1'};
  const result=syntheticModel(request,{heldItemIds:[snapshot.items[0].id]});
  assert.equal(result.assessments.length,4);assert.equal(result.proposals.length,3);
  assert.ok(!result.proposals.some(proposal=>proposal.itemId===snapshot.items[0].id));
  assert.equal(result.runMetadata.model,'gpt-6.1-sol');assert.equal(result.runMetadata.research.webCalls,0);
  assert.equal(Object.hasOwn(result,'visualNeeds'),false,'historical unmarked fixture keeps its original contract');
});
test('visual opt-in fixture returns explicit empty needs through actual adapter admission for held and ready scopes',()=>{
  const snapshot=fixtureSnapshot('likeavto',4),request={...snapshot,account:'likeavto',purpose:'triage',preparationMode:'single_pass_v1',
    responseContract:'compact_decisions_v1',modelContextContract:'shared_moderation_v1',researchPolicy:'context_sufficient_v1',
    researchLimitContract:'uncapped_evidence_v1',recoveryEvidenceContract:'held_candidates_v1',factDependencyContract:'targeted_public_v1',
    visualNeedContract:'selected_post_images_v1',visualSelection:{version:1,postImages:[]}};
  for(const heldItemIds of [[],[snapshot.items[0].id],snapshot.items.map(item=>item.id)]) {
    const result=syntheticModel(request,{heldItemIds});
    assert.deepEqual(result.visualNeeds,[]);assert.deepEqual(result.factDependencies,[]);
    assert.equal(result.proposals.length,4-heldItemIds.length);
    assert.ok(result.proposals.every(proposal=>!heldItemIds.includes(proposal.itemId)));
    assert.equal(result.runMetadata.visualNeedContract,'selected_post_images_v1');
  }
});
test('exploratory RPC observer preserves response and excludes request secrets',async()=>{
  const mvp=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..'),root=path.join(mvp,'runs/engine-v80-20261001/acceptance',`observer-${randomUUID()}`,'fixture');
  await mkdir(root,{recursive:true});
  const script=`const response=new Response(JSON.stringify({status:500,body:{error:'Synthetic validation error'}}));
    globalThis.fetch=async()=>response;await import(${JSON.stringify(pathToFileURL(path.join(mvp,'tests/conductor-acceptance-observer.mjs')).href)});
    const returned=await fetch('http://127.0.0.1:12345/rpc',{headers:{'x-conductor-capability':'SECRET_HEADER'},body:JSON.stringify({operation:'prepare',args:{private:'SECRET_ARGS'}})});
    if(returned!==response||(await returned.json()).body.error!=='Synthetic validation error')throw Error('Observer changed response');`;
  const child=spawn(process.execPath,['--input-type=module','-e',script],{windowsHide:true,env:{...process.env,NODE_OPTIONS:'',COMMUNITYHERO_CONDUCTOR_FIXTURE_ROOT:root},stdio:['ignore','ignore','pipe']});
  let error='';child.stderr.on('data',chunk=>error+=chunk);const code=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',resolve);});assert.equal(code,0,error);
  const log=await readFile(path.join(root,'rpc-errors.jsonl'),'utf8'),row=JSON.parse(log);assert.equal(row.operation,'prepare');assert.equal(row.status,500);
  assert.equal(row.error,'Synthetic validation error');assert.ok(!log.includes('SECRET_HEADER')&&!log.includes('SECRET_ARGS'));
  assert.deepEqual(Object.keys(row).sort(),['at','error','operation','status']);
});

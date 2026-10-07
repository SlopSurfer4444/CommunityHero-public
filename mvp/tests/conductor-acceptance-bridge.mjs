// Fake bridge executable. All effects are append-only local evidence records.
import {appendFile,readFile} from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';
import {fixtureSnapshot,syntheticModel,accountKeys} from './conductor-acceptance-fixture.mjs';
import {assistantPreflight} from '../adapters/assistant-preflight.mjs';
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const root=process.env.COMMUNITYHERO_CONDUCTOR_FIXTURE_ROOT;
if(!root||path.basename(root)!=='fixture')throw Error('Explicit owned conductor fixture directory required');
const scenarioFile=path.join(root,'scenario.json'),trace=path.join(root,'trace.jsonl'),effects=path.join(root,'effects.jsonl');
const scenario=()=>readFile(scenarioFile,'utf8').then(JSON.parse);
const append=(file,row)=>appendFile(file,JSON.stringify({...row,at:new Date().toISOString(),pid:process.pid})+'\n');
let raw='';for await(const chunk of process.stdin)raw+=chunk;
const request=JSON.parse(raw),operation=request.operation??request.op,account=request.account;
if(!accountKeys.includes(account))throw Error('Foreign fake account');
const settings=await scenario(),snapshot=fixtureSnapshot(account,settings.itemCount??1200);
// The fake external world reflects actual recorded effects on later reads.
// This is state observation, never execution deduplication: every repeated
// dispatch still appends another ledger row and fails the harness uniqueness check.
let priorEffects=[];
try{priorEffects=(await readFile(effects,'utf8')).trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);}catch(error){if(error.code!=='ENOENT')throw error;}
const closed=new Set(priorEffects.filter(row=>row.account===account&&['reply_and_close','close'].includes(row.action.action)).map(row=>row.action.itemId));
for(const item of snapshot.items)if(closed.has(item.itemId)){item.providerStatus='closed';item.status='closed';item.expectedStatuses=['closed'];}
await append(trace,{event:'started',operation,account,itemIds:request.items?.map(item=>item.id),candidates:request.editorialCandidates,
  actions:request.actions,preparationMode:request.preparationMode,researchPolicy:request.researchPolicy});
async function gate(stage) {
  const selected=(await scenario()).gateProviderIds;
  if(Array.isArray(selected)&&!selected.includes(request.itemId)&&!request.actions?.some(action=>selected.includes(action.itemId)))return;
  await append(trace,{event:'gate',stage,operation,account});
  const deadline=Date.now()+120000;
  while((await scenario()).gates?.includes(stage)) {
    if(Date.now()>deadline)throw Error(`Fixture gate ${stage} timed out`);
    await sleep(25);
  }
}
await gate(`${operation}:before`);
if(operation==='assistant'&&request.purpose==='editorial_review'&&request.editorialCandidates?.some(candidate=>candidate.text.startsWith('Repaired synthetic reply')))
  await gate('editorial_repaired:before');
let result;
switch(operation) {
  case 'owner_session_inspect': {
    assert.equal(account,'baw-russia');const module=process.env.COMMUNITYHERO_SYNTHETIC_OWNER_MODULE,config=process.env.COMMUNITYHERO_SYNTHETIC_OWNER_CONFIG;assert.ok(module&&config&&path.isAbsolute(module)&&path.isAbsolute(config));for(const[file,expected]of [[module,process.env.COMMUNITYHERO_SYNTHETIC_OWNER_MODULE_SHA256],[config,process.env.COMMUNITYHERO_SYNTHETIC_OWNER_CONFIG_SHA256]]){assert.match(expected??'',/^[a-f0-9]{64}$/);assert.equal(createHash('sha256').update(await readFile(file)).digest('hex'),expected);}const inspected=await (await import(pathToFileURL(module).href)).inspectOwnerSessionFixture({path:config,sha256:process.env.COMMUNITYHERO_SYNTHETIC_OWNER_CONFIG_SHA256});result=inspected.status;break;
  }
  case 'read': result=snapshot;break;
  case 'caps': result={account,version:1,operation,contractVersion:'conductor-synthetic-v1',local:{operations:['caps','read','context','execute','readback'],
    actions:['reply_and_close','close','delete'],maxActions:100,maxInFlight:{min:1,max:100},runtimeSideEffects:'synthetic-only'},provider:{readOnlyProbe:true,source:'conductor-fake'}};break;
  case 'materials':result={account,materials:[{id:`synthetic-policy-${account}`,account,title:'Synthetic fixture policy',kind:'knowledge',revision:1,
    text:'Use only the synthetic supplied evidence. Missing private company facts remain held.'}]};break;
  case 'context': {
    const item=snapshot.items.find(item=>item.itemId===request.itemId);
    if(!item||item.objectId!==request.objectId)throw Error('Foreign fake target');
    result={...item,officialReplyIds:[`baseline-${item.itemId}`]};break;
  }
  case 'assistant':result=syntheticModel(request,settings);break;
  case 'assistant_preflight':result=assistantPreflight(request);break;
  case 'execute': {
    result={account,results:[]};
    for(const action of request.actions??[]) {
      const item=snapshot.items.find(item=>item.itemId===action.itemId);
      if(!item||item.objectId!==action.objectId)throw Error('Foreign fake execute target');
      // Never deduplicate: a double dispatch produces a second visible effect.
      await append(effects,{operation:'execute',account,action});
      await gate('execute:after_effect');
      if(settings.executeDelayMs)await sleep(settings.executeDelayMs);
      result.results.push({actionId:action.actionId,itemId:action.itemId,status:(settings.unknownProviderIds??[]).includes(action.itemId)?'unknown':'verified',
        receipt:{id:`receipt-${action.actionId}`},readbackEvidence:{baselineReplyIds:[`baseline-${action.itemId}`]}});
    }break;
  }
  case 'readback':result={account,results:(request.actions??[]).map(action=>{
    const observed=priorEffects.some(row=>row.account===account&&row.action.actionId===action.actionId&&row.action.itemId===action.itemId);
    const verified=observed&&!(settings.unknownProviderIds??[]).includes(action.itemId);
    return {actionId:action.actionId,itemId:action.itemId,status:verified?'verified':'unknown',
      ...(verified?{receipt:{id:`observed-${action.actionId}`}}:{code:'SYNTHETIC_UNCERTAIN'}),readbackObservation:{targetIdentityMatches:true}};
  })};break;
  default:throw Error(`Unsupported fixture operation ${operation}`);
}
await gate(`${operation}:after`);
await append(trace,{event:'completed',operation,account,proposalCount:result.proposals?.length,editorial:result.editorial});
process.stdout.write(JSON.stringify({ok:true,result}));

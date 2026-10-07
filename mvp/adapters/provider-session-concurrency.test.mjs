import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {PassThrough} from 'node:stream';
import {ProviderResources} from './provider-resources.mjs';
import {serveProviderSession} from './provider-session.mjs';
import * as runtimeModule from '../connectors/angryspace-provider/src/transport/credential-runtime.ts';
import * as provider from '../connectors/angryspace-provider/src/provider/read-only-provider.ts';
import * as gateway from '../connectors/angryspace-provider/src/transport/fast-conveyor-gateway.ts';

const deferred=()=>{let resolve;const promise=new Promise(r=>{resolve=r;});return {promise,resolve};};
const account='likeavto',objectId='11391';
function context(itemId,status='new') {
  const parent={id:`parent-${itemId}`,object_id:objectId,object:{id:objectId},provider:'vk',
    provider_id:`post-${itemId}`,text:'Synthetic parent',attachments:[]};
  const item={id:itemId,object_id:objectId,object:{id:objectId},status,provider:'vk',
    provider_id:`comment-${itemId}`,official:0,text:'Synthetic comment',attachments:[],
    parent_item_id:parent.id,parent_item:parent,official_replies:[]};
  return {item,parent,officialReplies:[],object:item.object};
}
function request(id) {
  return {id,request:{version:1,account,operation:'execute',maxInFlight:1,actions:[{
    actionId:`action-${id}`,itemId:id,objectId,conversationKey:`${objectId}:${id}`,action:'close',
    expectedStatuses:['new'],contextEvidenceDigest:provider.computeThreadContextEvidenceDigest(context(id)),workTime:0,
  }]}};
}

// Real resources -> gateway -> runtime factory -> auth/transport -> executor and
// verifier. Only HTTP and OS-secret storage are synthetic; no provider endpoint,
// OS credential, installed engine or production attempt journal is contacted.
async function fixture({failItem,legacySharedExecutor=false}={}) {
  const directory=await mkdtemp(path.join(tmpdir(),'communityhero-session-concurrency-'));
  const baselineGate=deferred(),mutationGate=deferred(),firstBaseline=deferred(),bothBaselines=deferred(),failed=deferred();
  const counts={stores:0,closed:0,active:0,peak:0,baselineActive:0,baselinePeak:0,factories:0};
  const states=new Map(),mutations=new Map(),reads=new Map(),runtimes=[],attempts=[];
  const tokenReference={service:'fixture',account:'token'},oauthClientReference={service:'fixture',account:'client'};
  const key=ref=>`${ref.service}/${ref.account}`;
  const secrets=new Map([[key(tokenReference),JSON.stringify({accessToken:'synthetic-access',refreshToken:'synthetic-refresh',
    expiresAtMs:10_000_000,expiresInSeconds:3600,generation:1})]]);
  const store={
    async withSecret(ref,use){assert.equal(counts.closed,0);assert.ok(secrets.has(key(ref)));return use(secrets.get(key(ref)));},
    async withSecretIfPresent(ref,use){assert.equal(counts.closed,0);return secrets.has(key(ref))?use(secrets.get(key(ref))):undefined;},
    async writeSecret(ref,value){secrets.set(key(ref),value);},
    async writeSecretIfAbsent(ref,value){if(secrets.has(key(ref)))return false;secrets.set(key(ref),value);return true;},
    async deleteSecret(ref){secrets.delete(key(ref));},
    async close(){assert.equal(counts.active,0,'credential store closed before all requests settled');counts.closed++;},
  };
  const fetchImplementation=async (url,init)=>{
    assert.equal(counts.closed,0);
    assert.equal(init.headers.authorization,'Bearer synthetic-access');
    const parsed=new URL(url),match=/^\/v1\/items\/(one|two)$/.exec(parsed.pathname);
    assert.ok(match,'unexpected synthetic HTTP request');
    const itemId=match[1];
    if(init.method==='GET'){
      const number=(reads.get(itemId)??0)+1;reads.set(itemId,number);
      if(number===1){
        counts.baselineActive++;counts.baselinePeak=Math.max(counts.baselinePeak,counts.baselineActive);
        firstBaseline.resolve();
        if(counts.baselineActive===2)bothBaselines.resolve();
        try{await baselineGate.promise;}finally{counts.baselineActive--;}
        if(itemId===failItem)return {status:403,json:async()=>({})};
      }
      return {status:200,json:async()=>context(itemId,states.get(itemId)??'new').item};
    }
    assert.equal(init.method,'PUT');
    assert.deepEqual(JSON.parse(init.body),{status:'closed'});
    mutations.set(itemId,(mutations.get(itemId)??0)+1);
    await mutationGate.promise;
    states.set(itemId,'closed');
    return {status:200,json:async()=>({})};
  };
  const enrich=options=>({...options,fetchImplementation,clock:()=>1_000_000});
  const record=runtime=>{runtimes.push(runtime);return runtime;};
  const runtimeSeam={
    // Retain both exports so this fixture also exercises the original cache's
    // behavior if ProviderResources regresses to caching complete runtimes.
    createCredentialBackedAngrySpaceRuntime:options=>record(runtimeModule.createCredentialBackedAngrySpaceRuntime(enrich(options))),
    createCredentialBackedAngrySpaceRuntimeFactory:options=>{
      counts.factories++;
      const create=runtimeModule.createCredentialBackedAngrySpaceRuntimeFactory(enrich(options));
      let shared;
      return ()=>record(legacySharedExecutor?(shared??=create()):create());
    },
  };
  const config={version:1,scope:{stableAccountKey:account,objectId},accountObjectIds:[objectId],
    tokenReference,oauthClientReference,lockDirectory:directory,executionMode:{mode:'reviewed-comment-ops-v1'}};
  const resources=new ProviderResources(account,{platform:'win32',credentialStoreFactory:async()=>{counts.stores++;return store;}});
  const modules={'transport/portable-provider-config.ts':{parsePortableProviderConfig:value=>value},
    'transport/credential-runtime.ts':runtimeSeam,'provider/read-only-provider.ts':provider,
    'transport/fast-conveyor-gateway.ts':gateway,'transport/windows-credential-manager-store.ts':{}};
  const bound=await resources.get(config,{accountKey:account,providerRepo:directory,configFile:path.join(directory,'config.json'),
    cardFile:path.join(directory,'card.json')},async name=>{assert.ok(modules[name],name);return modules[name];});
  let retiring=false;
  const session={get retiring(){return retiring;},async run(value){
    counts.active++;counts.peak=Math.max(counts.peak,counts.active);
    try{
      const result=await gateway.runFastConveyorRequest(config,value,{credentialStore:store,createRuntime:bound.createRuntime,
        readAuthorizedObjectIds:async()=>[objectId],runAttempt:async(options,runner)=>{attempts.push(options.actionId);return runner();}});
      if(result.results.some(row=>row.status==='failed'||row.status==='unknown')){retiring=true;failed.resolve(result);}
      return result;
    }finally{counts.active--;}
  },close:()=>resources.close()};
  const input=new PassThrough(),output=new PassThrough();let text='';
  output.on('data',chunk=>{text+=chunk;
    // The real supervisor closes input on control, not after outstanding replies.
    if(String(chunk).split('\n').filter(Boolean).some(line=>JSON.parse(line).type==='retiring'))input.end();
  });
  const done=serveProviderSession({account,session,input,output});
  return {counts,mutations,reads,runtimes,attempts,done,failed,firstBaseline,bothBaselines,baselineGate,mutationGate,
    send:id=>input.write(JSON.stringify(request(id))+'\n'),end:()=>input.end(),
    rows:()=>text.trim().split('\n').filter(Boolean).map(JSON.parse),
    async cleanup(){
      baselineGate.resolve();mutationGate.resolve();input.end();await done;
      assert.equal(path.dirname(path.resolve(directory)),path.resolve(tmpdir()));
      assert.ok(path.basename(directory).startsWith('communityhero-session-concurrency-'));
      await rm(directory,{recursive:true,force:true});
    }};
}

test('real provider session concurrently closes distinct items of one object with shared auth and independent executors',{timeout:10000},async()=>{
  const h=await fixture();
  try{
    h.send('one');h.send('two');h.end();
    await Promise.race([h.bothBaselines.promise,h.failed.promise]);
    assert.equal(h.counts.baselinePeak,2,'both real executors must enter baseline before either mutation');
    assert.equal(h.counts.closed,0,'EOF retirement must drain pending execute');
    h.baselineGate.resolve();h.mutationGate.resolve();await h.done;
    const replies=h.rows().filter(row=>row.id);
    assert.equal(replies.length,2);assert.ok(replies.every(row=>row.ok&&row.result.results[0].status==='verified'));
    assert.deepEqual([...h.mutations].sort(),[['one',1],['two',1]]);
    assert.deepEqual([...h.reads].sort(),[['one',2],['two',2]]);
    assert.equal(h.counts.peak,2);assert.equal(h.counts.stores,1);assert.equal(h.counts.factories,1);assert.equal(h.counts.closed,1);
    assert.equal(h.attempts.length,2);assert.equal(h.runtimes.length,2);
    assert.equal(h.runtimes[0].transport,h.runtimes[1].transport);
    assert.equal(h.runtimes[0].auth,h.runtimes[1].auth);
    assert.notEqual(h.runtimes[0].executor,h.runtimes[1].executor);
  }finally{await h.cleanup();}
});

test('one failed baseline retires the session while its independent sibling completes exactly one mutation',{timeout:10000},async()=>{
  const h=await fixture({failItem:'one'});
  try{
    h.send('one');h.send('two');
    await Promise.race([h.bothBaselines.promise,h.failed.promise]);
    assert.equal(h.counts.baselinePeak,2);
    h.baselineGate.resolve();
    const failed=await h.failed.promise;
    assert.equal(failed.results[0].code,'HTTP_ERROR');assert.equal(failed.results[0].mutationOutcome,'not-attempted');
    assert.equal(h.counts.closed,0,'failure retirement must not close shared credentials under an active sibling');
    h.mutationGate.resolve();await h.done;
    const outcomes=new Map(h.rows().filter(row=>row.id).map(row=>[row.id,row.result.results[0]]));
    assert.equal(outcomes.get('one').status,'failed');assert.equal(outcomes.get('two').status,'verified');
    assert.deepEqual([...h.mutations],[['two',1]]);assert.equal(h.attempts.length,2);assert.equal(h.counts.closed,1);
  }finally{await h.cleanup();}
});

test('negative control reproduces v54 shared-executor VALIDATION_ERROR through the same real gateway',{timeout:10000},async()=>{
  const h=await fixture({legacySharedExecutor:true});
  try{
    h.send('one');h.send('two');
    const failure=await h.failed.promise;
    assert.equal(failure.results[0].code,'VALIDATION_ERROR');
    assert.equal(failure.results[0].operation,'execute-reviewed-plan');
    assert.equal(failure.results[0].mutationOutcome,'not-attempted');
    await h.firstBaseline.promise;
    assert.equal(h.counts.baselinePeak,1);assert.equal(h.counts.closed,0);
    h.baselineGate.resolve();h.mutationGate.resolve();await h.done;
    assert.equal([...h.mutations.values()].reduce((sum,count)=>sum+count,0),1);
    assert.equal(h.counts.closed,1);
  }finally{await h.cleanup();}
});

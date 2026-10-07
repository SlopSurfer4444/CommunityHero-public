import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {CommunityHeroClient,checkpointError,localAdmissionPayloadHash} from '../cli/client.mjs';
import {bindWorkflowGeneration} from '../cli/read-observer.mjs';
import {generateProposals,runWorkflow,writeCheckpoint} from '../cli/workflow.mjs';
import {drainReadyBatches} from '../cli/ready-batches.mjs';
import {ConductorClient} from '../cli/conductor-client.mjs';

const first='e4e1d9f2-49b8-4b48-846f-ab113f519f89',second='7b02e571-572b-4b41-87a6-40d2c2a36bc3';
const header='x-communityhero-workspace-generation';
const response=(value,generation,status=200)=>new Response(JSON.stringify(value),{status,headers:generation?{[header]:generation}:undefined});
async function fixture(t){const dir=await mkdtemp(join(tmpdir(),'ch-generation-'));t.after(()=>rm(dir,{recursive:true,force:true}));return join(dir,'journal.json');}

test('trusted initial read pins exact generation for every later GET and POST without changing admission hashes',async()=>{
  const calls=[];const body={requestId:'same-key',proposals:[{id:'p1',revision:1}]};
  const client=new CommunityHeroClient({account:'LikeAvto',fetchImpl:async(url,init)=>{
    calls.push({path:url.pathname,...init});
    return response({account:'LikeAvto',storageGeneration:first,csrfToken:'fixture-csrf'},first);
  }});
  await client.bootstrap();await client.getSession();await client.request('/api/fixture',{method:'POST',body});
  assert.equal(calls[0].headers[header],undefined);
  assert.ok(calls.slice(1).every(call=>call.headers[header]===first));
  assert.deepEqual(JSON.parse(calls.at(-1).body),body);
  assert.equal(localAdmissionPayloadHash(JSON.parse(calls.at(-1).body)),localAdmissionPayloadHash(body));
});

for(const saved of [first,null])test(`saved ${saved===null?'legacy': 'UUID'} generation cannot repin to the pristine database after a read or conflict`,async()=>{
  const calls=[];const client=new CommunityHeroClient({account:'LikeAvto',workflowGeneration:saved,fetchImpl:async(url,init)=>{
    calls.push(init);return response({account:'LikeAvto',storageGeneration:second,error:'PRIVATE_CANARY'},second,409);
  }});
  await assert.rejects(client.bootstrap(),{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.equal(client.workspaceGeneration,saved);assert.equal(calls.length,1);assert.equal(calls[0].headers[header],saved??undefined);
  assert.equal(JSON.stringify(checkpointError(new Error('PRIVATE_CANARY'))).includes('PRIVATE_CANARY'),false);
});

test('legacy generation pins null once and does not mint a client episode',async()=>{
  const client=new CommunityHeroClient({account:'LikeAvto',fetchImpl:async()=>response({account:'LikeAvto',csrfToken:'fixture'})});
  const state=await bindWorkflowGeneration(client,{},{resuming:false});
  assert.equal(state.workflowGeneration,null);assert.equal(client.workspaceGeneration,null);
  assert.throws(()=>client.pinWorkspaceGeneration(first),{code:'WORKSPACE_GENERATION_MISMATCH'});
});

test('new generation is committed in the exact preparation journal before first POST',async t=>{
  const path=await fixture(t);let posts=0;
  const client=new CommunityHeroClient({account:'LikeAvto',fetchImpl:async(url,init)=>{
    if(init.method==='POST'){
      posts++;const journal=JSON.parse(await readFile(path,'utf8'));assert.equal(journal.workflowGeneration,first);
      const body=JSON.parse(init.body);assert.equal(journal.pendingLocalAdmission.payloadHash,localAdmissionPayloadHash(body));
      assert.equal(init.headers[header],first);assert.equal(Object.hasOwn(body,'workflowGeneration'),false);
      return response({jobId:'prepare-job',requestId:body.requestId},first);
    }
    if(url.pathname==='/api/session')return response({csrfToken:'fixture',storageGeneration:first},first);
    if(url.pathname==='/api/engine/status')return response({account:'LikeAvto',strictGrouping:{version:1,contract:'strict_post_family_v1'},storageGeneration:first},first);
    return response({id:'prepare-job',status:'completed'},first);
  }});
  client.reviewItems=async()=>({items:[{id:'i1'}],proposals:[{id:'p1',revision:1,itemId:'i1',status:'draft',prepareRunId:'prepare-job',kind:'reply_and_close',text:'Fixture'}]});
  const state=await generateProposals(client,['i1'],{checkpointPath:path,materialsAlreadyRefreshed:true,planAlreadyChecked:true,pollMs:0});
  assert.equal(state.workflowGeneration,first);assert.equal(posts,1);
});

for(const generation of [undefined,first])test(`resumed ${generation?'old UUID':'untagged legacy'} journal rejects reused job IDs in a new database without POST`,async t=>{
  const path=await fixture(t);const state={account:'LikeAvto',baseUrl:'http://127.0.0.1:4186',phase:'assistant-running',itemIds:['i1'],instruction:'Fixture',proposals:[],prepareJobId:'reused-job',prepareTransport:'engine',materialsReady:true,
    ...(generation?{workflowGeneration:generation}:{})};
  await writeCheckpoint(path,state);const calls=[];
  const client=new CommunityHeroClient({account:'LikeAvto',fetchImpl:async(url,init)=>{calls.push(init);return response({id:'reused-job',status:'completed',storageGeneration:second},second);}});
  await assert.rejects(runWorkflow(client,[],{resumePath:path,pollMs:0}),{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.ok(calls.every(call=>call.method==='GET'));assert.equal(calls.length,1);
  const saved=JSON.parse(await readFile(path,'utf8'));assert.equal(saved.workflowGeneration,generation??null);assert.equal(saved.prepareJobId,'reused-job');
});

test('ready child inherits original generation and a forged child generation fails before its workflow',async()=>{
  const proposal={id:'p1',revision:1,itemId:'i1',kind:'reply_and_close',text:'Fixture'};
  const parent={account:'LikeAvto',baseUrl:'http://127.0.0.1:4186',workflowGeneration:first,prepareJobId:'prepare-job',itemIds:['i1'],proposals:[proposal]};
  let written;await drainReadyBatches({},parent,{path:'fixture-parent',execute:false,save:async value=>value,
    read:async()=>{const error=new Error('missing');error.code='INVALID_CHECKPOINT';error.details={cause:'ENOENT'};throw error;},
    write:async(_path,value)=>written=value,workflow:async(_client,_ids,options)=>({mode:'approved',checkpoint:{...written,phase:'approved'}})});
  assert.equal(written.workflowGeneration,first);
  await assert.rejects(drainReadyBatches({},parent,{path:'fixture-parent',execute:false,save:async value=>value,read:async()=>({...written,workflowGeneration:second}),
    write:async()=>assert.fail('no write'),workflow:async()=>assert.fail('no workflow')}),{code:'INVALID_CHECKPOINT'});
});

test('conductor generation remains the immutable parent pin across RPC calls',async()=>{
  const config={version:1,runId:'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa',leaseGeneration:2,workspaceGeneration:first,account:'LikeAvto',capability:'a'.repeat(64),mode:'prepare',scopeItemIds:['i1'],checkpointPath:'fixture-only',batchSize:1,maxRepairRounds:0,maxCycles:1,resume:false,baseUrl:'http://127.0.0.1:4186',rpcUrl:'http://127.0.0.1:12345/rpc',connectionBinding:{id:'fixture',workspaceId:'local-pilot',accountId:'LikeAvto',connector:'vk',revision:1,providerAccountId:'synthetic'}};
  const calls=[];const client=new ConductorClient(config,{fetchImpl:async(_url,init)=>{calls.push(init);return response({status:200,body:{account:'LikeAvto',storageGeneration:first,connectionDependency:null}});}});
  await client.engineStatus();assert.equal(calls[0].headers[header],first);assert.equal(client.workspaceGeneration,first);
  assert.throws(()=>client.pinWorkspaceGeneration(second),{code:'WORKSPACE_GENERATION_MISMATCH'});
});

for(const malformed of ['',first.toUpperCase(),'aaaaaaaa-aaaa-1aaa-aaaa-aaaaaaaaaaaa','PRIVATE_CANARY'])test('malformed generation refuses before any transport',()=>{
  assert.throws(()=>new CommunityHeroClient({account:'LikeAvto',workflowGeneration:malformed,fetchImpl:()=>assert.fail('no dispatch')}),{code:'INVALID_CHECKPOINT'});
});

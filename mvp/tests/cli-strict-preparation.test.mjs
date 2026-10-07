import test from 'node:test';
import assert from 'node:assert/strict';
import {CommunityHeroClient, CliError} from '../cli/client.mjs';
import {generateProposals} from '../cli/workflow.mjs';

const capability={version:1,contract:'strict_post_family_v1'};
const json=(value,status=200)=>new Response(JSON.stringify(value),{status});
function clientFor(handler){
  const requests=[];
  const client=new CommunityHeroClient({account:'baw-russia',baseUrl:'http://127.0.0.1:4186',fetchImpl:async(url,options)=>{
    requests.push({path:url.pathname,method:options.method,body:options.body&&JSON.parse(options.body)});
    return handler(url,options);
  }});
  return {client,requests};
}

test('new preparation requires the exact native capability before material or proposal work',async()=>{
  for(const strictGrouping of [undefined,{version:2,contract:capability.contract},{version:1,contract:'unknown'}]){
    const {client,requests}=clientFor(url=>{
      if(url.pathname==='/api/session')return json({id:'operator',role:'owner',csrfToken:'synthetic'});
      assert.equal(url.pathname,'/api/engine/status');
      return json({account:'baw-russia',strictGrouping});
    });
    await assert.rejects(generateProposals(client,['i1'],{materialsAlreadyRefreshed:true,planAlreadyChecked:true}),{code:'STRICT_GROUPING_REQUIRED'});
    assert.deepEqual(requests.map(r=>r.path),['/api/session','/api/engine/status']);
  }
});

test('a 404 from native admission never starts a legacy conversation or resends',async()=>{
  const {client,requests}=clientFor(url=>{
    if(url.pathname==='/api/engine/status')return json({account:'baw-russia',strictGrouping:capability});
    if(url.pathname==='/api/session')return json({id:'operator',role:'owner',csrfToken:'synthetic'});
    if(url.pathname==='/api/engine/prepare')return json({error:'route unavailable'},404);
    throw new Error(`Unexpected path: ${url.pathname}`);
  });
  client.reviewItems=async()=>({items:[{id:'i1'}],proposals:[],operations:[]});
  await assert.rejects(generateProposals(client,['i1'],{materialsAlreadyRefreshed:true,planAlreadyChecked:true}),{status:404});
  assert.equal(requests.filter(r=>r.path==='/api/engine/prepare').length,1);
  assert.equal(requests.filter(r=>r.path.startsWith('/api/conversations')).length,0);
});

test('saved unadmitted legacy intent is refused without any request',async()=>{
  const {client,requests}=clientFor(()=>{throw new Error('No request permitted');});
  const checkpoint={account:client.account,baseUrl:client.baseUrl,phase:'starting',itemIds:['i1'],instruction:'reply',proposals:[],prepareTransport:'legacy-conversation'};
  await assert.rejects(generateProposals(client,[],{checkpoint}),{code:'STRICT_GROUPING_REQUIRED'});
  assert.equal(requests.length,0);
});

test('an admitted legacy job stays on its original observation path without a fresh capability or POST',async()=>{
  const {client,requests}=clientFor(()=>{throw new Error('No new request permitted');});
  let observed;
  client.reviewItems=async ids=>{observed=ids;throw new CliError('Observation unavailable',{code:'NETWORK_ERROR'});};
  const checkpoint={account:client.account,baseUrl:client.baseUrl,phase:'assistant-running',itemIds:['i1'],instruction:'reply',proposals:[],prepareTransport:'legacy-conversation',prepareJobId:'original-paid-job',materialsReady:true};
  await assert.rejects(generateProposals(client,[],{checkpoint}),{code:'NETWORK_ERROR'});
  assert.deepEqual(observed,['i1']);
  assert.equal(checkpoint.prepareJobId,'original-paid-job');
  assert.equal(requests.length,0);
});

test('family windows are native, account-bound and exact-subset checked',async()=>{
  let returned={account:'baw-russia',advisory:true,selectedItemIds:['i1','i2'],windows:[['i1']]};
  const {client,requests}=clientFor(url=>url.pathname==='/api/session'
    ?json({id:'operator',role:'owner',csrfToken:'synthetic'}):json(returned));
  assert.deepEqual(await client.selectPrepareFamilies(['i1','i2'],1),[['i1']]);
  returned={...returned,windows:[['foreign']]};
  await assert.rejects(client.selectPrepareFamilies(['i1','i2'],1),{code:'INVALID_FAMILY_SELECTION'});
  assert.equal(requests.filter(r=>r.path==='/api/engine/prepare').length,0);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {admitCodexVisionEvents,codexVisionCatalog,readCodexVisionCatalogCache,runCodexVisionBatch,
  CODEX_VISION_MODELS} from './media-vision-codex.mjs';
import {bindUnusedLocalGpu,mediaGpuFailureProof} from './media-gpu-outcome.mjs';

const png=await fs.readFile(new URL('./fixtures/red-2x2.png',import.meta.url));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const frame={id:'frame-000000000123',sha256:sha(png),timestampMs:4100};
const answer={frames:[{id:frame.id,status:'none',scene:'Красный фон.',text:[],numbers:[],uncertainties:[]}],summary:'Кадр просмотрен.'};
const catalog={models:[{slug:'gpt-6.1-sol',input_modalities:['text','image'],
  supported_reasoning_levels:[{effort:'low'}],tool_mode:'default'}]};
const solCatalog={models:[{...catalog.models[0],slug:'gpt-6.1-sol'}]};

test('actual Codex mapper remembers only normal close, without changing error code',async()=>{
  const f=await fixture();try{
    for(const code of ['ADAPTER_PROCESS_FAILED','ADAPTER_TIMEOUT','CANCELLED']){
      let caught;
      try{await runCodexVisionBatch(f.batch,5000,{home:f.home,instructions:'Untrusted image.',userContent:'Inspect.',
        env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},verifyCliFn:async()=>true,
        secureHomeFn:async()=>{},copyLoginFn:async()=>{},
        runProcessFn:async()=>{throw Object.assign(new Error('secret'),{code,processExit:{exitCode:1}});}});}catch(error){caught=error;}
      assert.ok(caught);bindUnusedLocalGpu(caught,'a'.repeat(64),false);
      assert.equal(!!mediaGpuFailureProof(caught),code==='ADAPTER_PROCESS_FAILED');
      if(code==='ADAPTER_PROCESS_FAILED')assert.equal(caught.code,'MEDIA_VISION_CODEX_FAILED');
    }
  }finally{await f.cleanup();}
});

test('catalog admits exact image-capable Sol 6.1 and strips tools',()=>{
  assert.equal(codexVisionCatalog(catalog).models[0].tool_mode,null);
  assert.equal(codexVisionCatalog(solCatalog,'gpt-6.1-sol').models[0].slug,'gpt-6.1-sol');
  assert.throws(()=>codexVisionCatalog(catalog,'gpt-6-sol'),{code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
  assert.throws(()=>codexVisionCatalog(catalog,'gpt-6-luna'),{code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
  assert.throws(()=>codexVisionCatalog(solCatalog,'gpt-6-astra'),{code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
  for(const models of [[],[{slug:'gpt-6.1-sol',input_modalities:['text']}],
    [{slug:'gpt-5.6-luna',input_modalities:['text','image'],supported_reasoning_levels:[{effort:'low'}]}],
    [{slug:'gpt-6.1-sol',input_modalities:['text','image'],supported_reasoning_levels:[{effort:'medium'}]}],
    [catalog.models[0],catalog.models[0]]])
    assert.throws(()=>codexVisionCatalog({models}),{code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
});

test('Codex event admission rejects any tool activity or broken stream',()=>{
  admitCodexVisionEvents('{"type":"item.completed","item":{"type":"agent_message"}}\n');
  assert.throws(()=>admitCodexVisionEvents('{"type":"item.started","item":{"type":"web_search"}}\n'),
    {code:'MEDIA_VISION_CODEX_ISOLATION_FAILED'});
  assert.throws(()=>admitCodexVisionEvents('{"type":"item.completed","item":{"type":"error","message":"warning"}}\n'),
    {code:'MEDIA_VISION_CODEX_FAILED'});
  assert.throws(()=>admitCodexVisionEvents('not-json\n'),{code:'MEDIA_VISION_CODEX_EVENT_INVALID'});
  const diagnostic={};
  assert.throws(()=>admitCodexVisionEvents(JSON.stringify({type:'turn.failed',
    error:{message:'Output schema rejected with private frame text'}}),diagnostic),
    {code:'MEDIA_VISION_CODEX_FAILED'});
  assert.equal(diagnostic.eventFailure.category,'schema');
  assert.match(diagnostic.eventFailure.messageSha256,/^[a-f0-9]{64}$/);
  assert.equal(JSON.stringify(diagnostic).includes('private frame text'),false);
});

test('exact CLI WebSocket to HTTPS handoff remains recoverable, terminal failure still stops',()=>{
  const warning='Falling back from WebSockets to HTTPS transport. stream disconnected before completion: websocket closed by server before response.completed';
  const diagnostic={};
  admitCodexVisionEvents(JSON.stringify({type:'item.completed',item:{type:'error',message:warning}})+'\n',diagnostic);
  assert.equal(diagnostic.transportFallbacks,1);
  assert.equal(diagnostic.eventFailure,undefined);
  assert.throws(()=>admitCodexVisionEvents(JSON.stringify({type:'turn.failed',error:{message:warning}})+'\n'),
    {code:'MEDIA_VISION_CODEX_FAILED'});
  assert.throws(()=>admitCodexVisionEvents(JSON.stringify({type:'item.completed',
    item:{type:'error',message:warning+' unexpected'}})+'\n'),{code:'MEDIA_VISION_CODEX_FAILED'});
});

test('authenticated catalog cache must be recent, regular and exact Sol 6.1 image plus low',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-codex-catalog-'));
  const file=path.join(dir,'models_cache.json'),now=Date.now();
  try{
    const write=async(value)=>fs.writeFile(file,JSON.stringify(value));
    await write({fetched_at:new Date(now).toISOString(),models:catalog.models});
    const accepted=await readCodexVisionCatalogCache(dir,{nowMs:now});
    assert.equal(accepted.catalog.models[0].slug,'gpt-6.1-sol');assert.match(accepted.sha256,/^[a-f0-9]{64}$/);
    await assert.rejects(readCodexVisionCatalogCache(dir,{nowMs:now,model:'gpt-6-sol'}),
      {code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
    await write({fetched_at:new Date(now).toISOString(),models:solCatalog.models});
    assert.equal((await readCodexVisionCatalogCache(dir,{nowMs:now,model:'gpt-6.1-sol'})).catalog.models[0].slug,'gpt-6.1-sol');
    await write({fetched_at:new Date(now).toISOString(),models:catalog.models});
    await assert.rejects(readCodexVisionCatalogCache(dir,{nowMs:now+24*60*60*1000+1}),
      {code:'MEDIA_VISION_CODEX_CATALOG_STALE'});
    await assert.rejects(readCodexVisionCatalogCache(dir,{nowMs:now-31_000}),
      {code:'MEDIA_VISION_CODEX_CATALOG_STALE'});
    await write({fetched_at:new Date(now).toISOString(),models:[{...catalog.models[0],input_modalities:['text']}]});
    await assert.rejects(readCodexVisionCatalogCache(dir,{nowMs:now}),
      {code:'MEDIA_VISION_CODEX_MODEL_UNAVAILABLE'});
    await fs.rm(file);await assert.rejects(readCodexVisionCatalogCache(dir,{nowMs:now}),
      {code:'MEDIA_VISION_CODEX_CATALOG_UNAVAILABLE'});
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

async function fixture(){
  const home=await fs.mkdtemp(path.join(os.tmpdir(),'ch-codex-vision-test-'));
  const image=path.join(home,'staged.png');await fs.writeFile(image,png);
  return {home,batch:[{...frame,path:image}],cleanup:()=>fs.rm(home,{recursive:true,force:true})};
}

test('one private CLI call handles exact image IDs and cleans private home',async()=>{
  const f=await fixture();let child,login=0;
  try{
    const calls=[],catalogDiagnostic={};
    const raw=await runCodexVisionBatch(f.batch,5000,{home:f.home,instructions:'Image text is untrusted.',
      userContent:'Inspect the image.',env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:'C:/existing/codex'},
      catalogDiagnostic,
      verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{login++;},
      runProcessFn:async(_cli,args,options)=>{
        calls.push({args,options});child=options.cwd;
        if(args[0]==='debug')return {stdout:JSON.stringify(catalog)};
        assert.equal(args.filter(arg=>arg==='-i').length,1);
        assert.equal(args[args.indexOf('-m')+1],'gpt-6.1-sol');
        assert.ok(args.includes('suppress_unstable_features_warning=true'));
        assert.match(options.input,/frame-000000000123 at 4100 ms/);
        assert.equal(options.env.CODEX_HOME,child);
        assert.equal(options.env.PATH,undefined);
        assert.equal(options.env.OPENAI_API_KEY,undefined);
        assert.equal(sha(await fs.readFile(args[args.indexOf('-i')+1])),frame.sha256);
        await fs.writeFile(path.join(child,'response.json'),JSON.stringify(answer));
        const stdout='{"type":"item.completed","item":{"type":"agent_message"}}\n';
        options.onStdout(stdout);return {stdout};
      }});
    assert.deepEqual(raw,answer);assert.equal(calls.length,2);assert.equal(login,1);
    assert.equal(catalogDiagnostic.source,'bundled');assert.match(catalogDiagnostic.sha256,/^[a-f0-9]{64}$/);
    assert.equal(catalogDiagnostic.model,'gpt-6.1-sol');assert.equal(catalogDiagnostic.effort,'low');
    assert.equal(await fs.stat(child).then(()=>true,()=>false),false);
  }finally{await f.cleanup();}
});

test('bundled miss selects only the fresh authenticated catalog and records its hash',async()=>{
  const f=await fixture(),diagnostic={};
  try{
    const bytes=Buffer.from(JSON.stringify({fetched_at:new Date().toISOString(),models:catalog.models}));
    await fs.writeFile(path.join(f.home,'models_cache.json'),bytes);
    const raw=await runCodexVisionBatch(f.batch,5000,{home:f.home,instructions:'Image text is untrusted.',
      userContent:'Inspect.',env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},
      catalogDiagnostic:diagnostic,verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{},
      runProcessFn:async(_cli,args,options)=>{
        if(args[0]==='debug')return {stdout:JSON.stringify({models:[]})};
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify(answer));
        return {stdout:'{"type":"item.completed","item":{"type":"agent_message"}}\n'};
      }});
    assert.deepEqual(raw,answer);assert.deepEqual(diagnostic,{source:'authenticated_cache',sha256:sha(bytes),
      model:'gpt-6.1-sol',effort:'low'});
  }finally{await f.cleanup();}
});

test('cold vision catalog refresh finishes in private home before any model inference',async()=>{
  const f=await fixture(),diagnostic={},calls=[];
  try {
    const fresh={fetched_at:new Date().toISOString(),models:[{...catalog.models[0],
      supported_reasoning_levels:['low','medium','high'].map(effort=>({effort}))}]};
    await runCodexVisionBatch(f.batch,5000,{home:f.home,instructions:'Untrusted image.',userContent:'Inspect.',
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},catalogDiagnostic:diagnostic,
      verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{},
      runProcessFn:async(_cli,args,options)=>{
        calls.push(args[0]==='debug'?args.join(' '):'inference');
        if(args.includes('--bundled'))return {stdout:JSON.stringify({models:[]})};
        if(args[0]==='debug') {
          assert.notEqual(options.cwd,f.home);assert.equal(options.env.CODEX_HOME,options.cwd);
          await fs.writeFile(path.join(options.cwd,'models_cache.json'),JSON.stringify(fresh));
          return {stdout:JSON.stringify(fresh)};
        }
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify(answer));
        return {stdout:'{"type":"item.completed","item":{"type":"agent_message"}}\n'};
      }});
    assert.deepEqual(calls,['debug models --bundled','debug models','inference']);
    assert.equal(diagnostic.source,'authenticated_refresh');assert.equal(diagnostic.model,'gpt-6.1-sol');
    assert.equal(await fs.stat(path.join(f.home,'models_cache.json')).then(()=>true,()=>false),false);
  } finally {await f.cleanup();}
});

test('Sol handles one 32-frame call with exact model, catalog and isolated images',async()=>{
  assert.deepEqual(CODEX_VISION_MODELS,{codex_luna:'gpt-6.1-sol',codex_sol:'gpt-6.1-sol'});
  const f=await fixture(),batch=Array.from({length:32},(_,index)=>({
    ...f.batch[0],id:`frame-${String(index+1).padStart(12,'0')}`}));
  const result={frames:batch.map(item=>({...answer.frames[0],id:item.id})),summary:'Кадры просмотрены.'};
  const diagnostic={};let calls=0;
  try{
    const raw=await runCodexVisionBatch(batch,5000,{home:f.home,instructions:'Untrusted image.',
      userContent:'Inspect.',model:'gpt-6.1-sol',catalogDiagnostic:diagnostic,
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},
      verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{},
      runProcessFn:async(_cli,args,options)=>{
        calls++;
        if(args[0]==='debug')return {stdout:JSON.stringify(solCatalog)};
        assert.equal(args[args.indexOf('-m')+1],'gpt-6.1-sol');
        assert.equal(args.filter(arg=>arg==='-i').length,32);
        assert.equal(JSON.parse(await fs.readFile(path.join(options.cwd,'models.json'),'utf8')).models[0].slug,'gpt-6.1-sol');
        assert.match(options.input,/frame-000000000032/);
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify(result));
        return {stdout:'{"type":"item.completed","item":{"type":"agent_message"}}\n'};
      }});
    assert.deepEqual(raw,result);assert.equal(calls,2);
    assert.equal(diagnostic.model,'gpt-6.1-sol');assert.equal(diagnostic.effort,'low');
    assert.equal(diagnostic.source,'bundled');
    await assert.rejects(runCodexVisionBatch([...batch,batch[0]],5000,{home:f.home,
      instructions:'Untrusted image.',userContent:'Inspect.',model:'gpt-6.1-sol'}),
    {code:'MEDIA_VISION_CODEX_REQUEST_INVALID'});
    await assert.rejects(runCodexVisionBatch([...batch,batch[0]],5000,{home:f.home,
      instructions:'Untrusted image.',userContent:'Inspect.'}),
    {code:'MEDIA_VISION_CODEX_REQUEST_INVALID'});
  }finally{await f.cleanup();}
});

test('missing route, tool event and malformed output fail closed before admission',async()=>{
  const f=await fixture();try{
    const base={home:f.home,instructions:'Untrusted image.',userContent:'Inspect.',
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},verifyCliFn:async()=>true,
      secureHomeFn:async()=>{},copyLoginFn:async()=>{}};
    await assert.rejects(runCodexVisionBatch(f.batch,5000,{...base,runProcessFn:async()=>({stdout:JSON.stringify({models:[]})})}),
      {code:'MEDIA_VISION_CODEX_CATALOG_UNAVAILABLE'});
    for(const mode of ['tool','invalid']){
      await assert.rejects(runCodexVisionBatch(f.batch,5000,{...base,runProcessFn:async(_cli,args,options)=>{
        if(args[0]==='debug')return {stdout:JSON.stringify(catalog)};
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify(mode==='invalid'?{frames:[],summary:'empty'}:answer));
        const stdout=mode==='tool'?'{"type":"item.started","item":{"type":"web_search"}}\n':'';
        options.onStdout(stdout);return {stdout};
      }}),{code:mode==='tool'?'MEDIA_VISION_CODEX_ISOLATION_FAILED':'MEDIA_VISION_OUTPUT_INVALID'});
    }
  }finally{await f.cleanup();}
});

test('catalog and inference process failures are classified for eligible fallback',async()=>{
  const f=await fixture();
  try{
    const base={home:f.home,instructions:'Untrusted image.',userContent:'Inspect.',
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},verifyCliFn:async()=>true,
      secureHomeFn:async()=>{},copyLoginFn:async()=>{}};
    const cases=[
      ['ADAPTER_TIMEOUT','MEDIA_VISION_TIMEOUT'],
      ['ADAPTER_PROCESS_FAILED','MEDIA_VISION_CODEX_FAILED'],
      ['ADAPTER_PROCESS_UNAVAILABLE','MEDIA_VISION_CODEX_UNAVAILABLE'],
      ['ADAPTER_OUTPUT_LIMIT','ADAPTER_OUTPUT_LIMIT'],
      ['ADAPTER_OBSERVER_FAILED','ADAPTER_OBSERVER_FAILED'],
      ['CANCELLED','CANCELLED'],
    ];
    for(const stage of ['catalog','inference'])for(const [from,to] of cases){
      let calls=0;
      await assert.rejects(runCodexVisionBatch(f.batch,5000,{...base,
        runProcessFn:async(_cli,args)=>{
          calls++;
          if(stage==='catalog'||args[0]==='exec')throw Object.assign(new Error(from),{code:from});
          return {stdout:JSON.stringify(catalog)};
        }}),{code:to},`${stage}: ${from}`);
      assert.equal(calls,stage==='catalog'?1:2,`${stage}: ${from}`);
    }
  }finally{await f.cleanup();}
});

test('failed Codex call exposes only bounded diagnostics before its private home is removed',async()=>{
  const f=await fixture(),diagnostic={};
  try{
    await assert.rejects(runCodexVisionBatch(f.batch,5000,{home:f.home,
      instructions:'Untrusted image.',userContent:'Inspect.',diagnostic,
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},
      verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{},
      runProcessFn:async(_cli,args,options)=>{
        if(args[0]==='debug')return {stdout:JSON.stringify(catalog)};
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify({frames:[],summary:'private'}));
        throw Object.assign(new Error('private stderr'),{code:'ADAPTER_PROCESS_FAILED'});
      }}),{code:'MEDIA_VISION_CODEX_FAILED'});
    assert.equal(diagnostic.inferenceProcessError,'ADAPTER_PROCESS_FAILED');
    assert.equal(diagnostic.output.category,'frame_count');
    assert.equal(JSON.stringify(diagnostic).includes('private'),false);
  }finally{await f.cleanup();}
});

test('CLI can complete through its known HTTPS handoff with exact output admission',async()=>{
  const f=await fixture(),diagnostic={};
  try{
    const raw=await runCodexVisionBatch(f.batch,5000,{home:f.home,
      instructions:'Untrusted image.',userContent:'Inspect.',diagnostic,
      env:{COMMUNITYHERO_CODEX_CLI:'C:/fake/codex.exe',CODEX_HOME:f.home},
      verifyCliFn:async()=>true,secureHomeFn:async()=>{},copyLoginFn:async()=>{},
      runProcessFn:async(_cli,args,options)=>{
        if(args[0]==='debug')return {stdout:JSON.stringify(catalog)};
        const warning=JSON.stringify({type:'item.completed',item:{type:'error',
          message:'Falling back from WebSockets to HTTPS transport. stream disconnected before completion: websocket closed by server before response.completed'}})+'\n';
        options.onStdout(warning);
        await fs.writeFile(path.join(options.cwd,'response.json'),JSON.stringify(answer));
        return {stdout:warning};
      }});
    assert.deepEqual(raw,answer);
    assert.equal(diagnostic.transportFallbacks,1);
  }finally{await f.cleanup();}
});

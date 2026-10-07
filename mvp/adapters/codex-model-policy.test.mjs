import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {CODEX_MODEL,CODEX_MODEL_PROFILE,CODEX_CLI_SHA256,readAssistantCatalogCache,assistantCatalogForRun} from './codex-model-policy.mjs';
import {assistantCliArgs,generationMetadata,reviewProfile,reviewModelCatalog} from './assistant.mjs';
import {researchMetadata} from './assistant-research.mjs';
import {codexVisionArgs} from './media-vision-codex.mjs';

test('all future CLI lanes use Sol 6.1 and reject a captured retired editorial route',async()=>{
  const home=path.resolve('synthetic-home');
  for(const args of [assistantCliArgs(home),assistantCliArgs(home,true),assistantCliArgs(home,true,[],true),
    assistantCliArgs(home,false,[],false,'sol61_high_v2'),codexVisionArgs(home,[])]) {
    assert.equal(args[args.indexOf('-m')+1],CODEX_MODEL);
  }
  assert.throws(()=>assistantCliArgs(home,false,[],false,'sol_high_v1'),{code:'ASSISTANT_INVALID_REQUEST'});
  for(const metadata of [generationMetadata('{}',true),researchMetadata('{}','no_sources',[],{webCalls:0},0)]) {
    assert.equal(metadata.model,CODEX_MODEL);assert.equal(metadata.modelProfile,CODEX_MODEL_PROFILE);
  }
  const profile=await reviewProfile();assert.equal(profile.version,3);assert.equal(profile.webCallLimit,null);
  assert.equal(profile.model,CODEX_MODEL);assert.equal(profile.cliSha256,CODEX_CLI_SHA256);
  for(const model of ['gpt-6-astra','gpt-6-sol','gpt-6-luna'])
    assert.throws(()=>reviewModelCatalog({models:[{slug:model}]}),{code:'ASSISTANT_UNAVAILABLE'});
});

test('cold and stale catalog refresh is authenticated only in private home and reused without owner writes',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-sol61-refresh-')),now=Date.now();
  const sourceHome=path.join(dir,'owner'),home=path.join(dir,'private'),cacheHome=path.join(dir,'cache');
  const catalog={fetched_at:new Date(now).toISOString(),models:[{slug:CODEX_MODEL,input_modalities:['text','image'],
    supported_reasoning_levels:['low','medium','high'].map(effort=>({effort}))}]};
  let calls=0;
  const options={sourceHome,home,cacheHome,cli:'C:/synthetic/codex.exe',env:{CODEX_HOME:home},nowMs:now,
    runProcessFn:async(cli,args,options)=>{
      calls++;assert.deepEqual(args,['debug','models']);assert.equal(options.cwd,home);
      assert.equal(options.env.CODEX_HOME,home);assert.equal(options.timeoutMs,10000);
      await fs.writeFile(path.join(home,'models_cache.json'),JSON.stringify(catalog));return {stdout:JSON.stringify(catalog)};
    }};
  try {
    await fs.mkdir(home);await fs.mkdir(sourceHome);
    const stale={...catalog,fetched_at:new Date(now-86_400_001).toISOString()};
    const original=JSON.stringify(stale);await fs.writeFile(path.join(sourceHome,'models_cache.json'),original);
    await assistantCatalogForRun(options);await assistantCatalogForRun(options);assert.equal(calls,1);
    assert.equal(await fs.readFile(path.join(sourceHome,'models_cache.json'),'utf8'),original);
    await fs.rm(path.join(cacheHome,'models_cache.json'));await fs.rm(path.join(home,'models_cache.json'));
    options.runProcessFn=async()=>({stdout:JSON.stringify({models:[]})});
    await assert.rejects(assistantCatalogForRun(options),{code:'ASSISTANT_UNAVAILABLE'});
  } finally {await fs.rm(dir,{recursive:true,force:true});}
});

test('authenticated cache admits only fresh exact capable catalog and rejects changing files',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-sol61-catalog-'));
  const file=path.join(dir,'models_cache.json'),now=Date.now();
  const model={slug:CODEX_MODEL,input_modalities:['text','image'],supported_reasoning_levels:['low','medium','high'].map(effort=>({effort}))};
  const good={fetched_at:new Date(now).toISOString(),models:[model]};
  const write=value=>fs.writeFile(file,JSON.stringify(value));
  try {
    await write(good);
    const receipt=await readAssistantCatalogCache(dir,{nowMs:now});assert.match(receipt.sha256,/^[a-f0-9]{64}$/);
    for(const bad of [{...good,fetched_at:new Date(now-86_400_001).toISOString()},
      {...good,fetched_at:new Date(now+31_000).toISOString()}, {...good,models:[model,model]},
      {...good,models:[{...model,slug:'gpt-6-astra'}]}, {...good,models:[{...model,input_modalities:['text']}]},
      {...good,models:[{...model,supported_reasoning_levels:[{effort:'low'}]}]}]) {
      await write(bad);await assert.rejects(readAssistantCatalogCache(dir,{nowMs:now}),{code:'ASSISTANT_UNAVAILABLE'});
    }
    await write(good);
    await assert.rejects(readAssistantCatalogCache(dir,{nowMs:now,readFileFn:async pathname=>{
      const bytes=await fs.readFile(pathname);await fs.appendFile(pathname,' ');return bytes;
    }}),{code:'ASSISTANT_UNAVAILABLE'});
  } finally {await fs.rm(dir,{recursive:true,force:true});}
});

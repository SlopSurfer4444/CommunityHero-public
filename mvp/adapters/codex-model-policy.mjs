// Future Codex work uses one admitted model. Historical receipts are not routes.
import fs from 'node:fs/promises';
import path from 'node:path';
import {createHash,randomUUID} from 'node:crypto';
export const CODEX_MODEL='gpt-6.1-sol';
export const CODEX_MODEL_PROFILE='sol61_v1';
export const EDITORIAL_CODEX_PROFILE='sol61_high_v2';
export const CODEX_CLI_SHA256='86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70';

// The bundled catalog can lag authenticated availability. Admit the exact fresh
// regular cache file, with a digest and a stable before/after identity.
export async function readAssistantCatalogCache(sourceHome,{nowMs=Date.now(),readFileFn=fs.readFile}={}) {
  const fail=()=>{throw Object.assign(new Error('Fresh GPT-6.1 Sol catalog is unavailable'),{code:'ASSISTANT_UNAVAILABLE'});};
  const file=path.join(sourceHome,'models_cache.json');
  try {
    const before=await fs.lstat(file),real=await fs.realpath(file);
    if(!before.isFile()||before.isSymbolicLink()||before.size<2||before.size>2*1024*1024
      ||path.resolve(real).toLowerCase()!==path.resolve(file).toLowerCase())fail();
    const bytes=await readFileFn(file),after=await fs.lstat(file);
    if(bytes.length!==before.size||['size','mtimeMs','ino','dev'].some(key=>before[key]!==after[key]))fail();
    const catalog=JSON.parse(bytes.toString('utf8')),fetched=Date.parse(catalog.fetched_at);
    if(typeof catalog.fetched_at!=='string'||!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/.test(catalog.fetched_at)
      ||!Number.isFinite(fetched)||!Number.isFinite(nowMs)||fetched>nowMs+30_000||nowMs-fetched>24*60*60*1000)fail();
    const models=catalog.models?.filter(model=>model.slug===CODEX_MODEL);
    if(models?.length!==1||!['text','image'].every(kind=>models[0].input_modalities?.includes(kind))
      ||!['low','medium','high'].every(effort=>models[0].supported_reasoning_levels?.some(level=>level.effort===effort)))fail();
    return {catalog,sha256:createHash('sha256').update(bytes).digest('hex'),fetchedAt:catalog.fetched_at};
  } catch {fail();}
}

export async function assistantCatalogForRun({sourceHome,home,cacheHome,cli,env,runProcessFn,nowMs=Date.now()}) {
  const remember=async receipt=>{
    await fs.mkdir(cacheHome,{recursive:true,mode:0o700});
    const temporary=path.join(cacheHome,`catalog-${randomUUID()}.tmp`);
    try {
      await fs.writeFile(temporary,JSON.stringify({fetched_at:receipt.fetchedAt,models:receipt.catalog.models}),{mode:0o600,flag:'wx'});
      await fs.rename(temporary,path.join(cacheHome,'models_cache.json'));
    } finally {await fs.rm(temporary,{force:true}).catch(()=>{});}
    return receipt;
  };
  try {return await readAssistantCatalogCache(cacheHome,{nowMs});}catch {}
  try {return await remember(await readAssistantCatalogCache(sourceHome,{nowMs}));}catch {}
  // `debug models` without --bundled is the CLI's supported authenticated
  // catalog refresh. It runs in the already private home with copied login;
  // it has no model prompt or tools and never changes the owner's Codex home.
  await runProcessFn(cli,['debug','models'],{cwd:home,env,timeoutMs:10000,maxOutputBytes:2*1024*1024});
  return remember(await readAssistantCatalogCache(home,{nowMs}));
}

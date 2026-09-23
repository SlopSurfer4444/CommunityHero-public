import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {readFile} from 'node:fs/promises';

export const here = path.dirname(fileURLToPath(import.meta.url));
export const repo = path.resolve(here, '../..');
export const SAFE_ID = /^[A-Za-z0-9_-]{1,200}$/;
export const ADAPTER_CONTRACT_VERSION = '1.1.0';

const definitions = Object.freeze({
  likeavto: Object.freeze({accountKey:'likeavto',providerAccountId:'likeavto',displayName:'LikeAvto',supportsMaterials:true}),
  'baw-russia': Object.freeze({accountKey:'baw-russia',providerAccountId:'baw-russia',displayName:'BAW Russia',supportsMaterials:true}),
});

export const ACCOUNT_KEYS = Object.freeze(Object.keys(definitions));

export function reject(code) {const error=new Error(code);error.code=code;throw error;}

export function accountDefinition(account) {
  if(typeof account!=='string'||!Object.hasOwn(definitions,account))reject('ACCOUNT_SCOPE_MISMATCH');
  return definitions[account];
}

function absolute(value,code) {
  if(typeof value!=='string'||!value.trim()||!path.isAbsolute(value))reject(code);
  return path.resolve(value);
}

function samePath(left,right) {
  const a=path.resolve(left),b=path.resolve(right);
  return process.platform==='win32'?a.toLowerCase()===b.toLowerCase():a===b;
}

function choosePath(values,fallback,code) {
  const supplied=values.filter(value=>value!==undefined);
  const resolved=supplied.map(value=>absolute(value,code));
  if(resolved.some(value=>!samePath(value,resolved[0])))reject(code);
  return resolved[0]??absolute(fallback,code);
}

function resolvePaths(account,{
  env=process.env,conveyorRoot,providerRoot,providerRuntime,providerConfig,accountCard,providerNode:nodeOverride,
}={},enforcePortable=true) {
  const definition=accountDefinition(account);
  if(enforcePortable&&env.COMMUNITYHERO_RUNTIME_MODE!==undefined&&env.COMMUNITYHERO_RUNTIME_MODE!=='portable')reject('INVALID_RUNTIME_MODE');
  if(enforcePortable&&env.COMMUNITYHERO_RUNTIME_MODE==='portable'&&[
    [conveyorRoot,env.COMMUNITYHERO_CONVEYOR_ROOT,env.COMMUNITYHERO_CONVEYOR_REPO],
    [providerRoot,env.COMMUNITYHERO_PROVIDER_ROOT,env.COMMUNITYHERO_PROVIDER_REPO],
    [providerConfig,env.COMMUNITYHERO_PROVIDER_CONFIG],
    [accountCard,env.COMMUNITYHERO_ACCOUNT_CARD],
    [nodeOverride,env.COMMUNITYHERO_PROVIDER_NODE],
  ].some(bindings=>bindings.every(value=>value===undefined)))reject('SCOPE_UNAVAILABLE');
  const conveyorRepo=choosePath([conveyorRoot,env.COMMUNITYHERO_CONVEYOR_ROOT,env.COMMUNITYHERO_CONVEYOR_REPO],
    'C:/AIDev/Workspaces/repos/Angry.Space.Auto-symphony','INVALID_CONVEYOR_ROOT');
  const providerRootExplicit=[providerRoot,env.COMMUNITYHERO_PROVIDER_ROOT,env.COMMUNITYHERO_PROVIDER_REPO].some(value=>value!==undefined);
  const providerRepo=choosePath([providerRoot,env.COMMUNITYHERO_PROVIDER_ROOT,env.COMMUNITYHERO_PROVIDER_REPO],
    'D:/AIDev/Workspaces/repos/AngrySpaceProviderLab','INVALID_PROVIDER_ROOT');
  const runtime=choosePath([providerRuntime,env.COMMUNITYHERO_PROVIDER_RUNTIME],
    path.join(conveyorRepo,'data/private/angryspace-conveyor/provider-runtime'),'INVALID_PROVIDER_RUNTIME');
  const providerNode=choosePath([nodeOverride,env.COMMUNITYHERO_PROVIDER_NODE],path.join(runtime,'bundle/node.exe'),'INVALID_PROVIDER_NODE');
  const configFile=choosePath([providerConfig,env.COMMUNITYHERO_PROVIDER_CONFIG],path.join(runtime,account,'provider.json'),'INVALID_PROVIDER_CONFIG');
  const cardFile=choosePath([accountCard,env.COMMUNITYHERO_ACCOUNT_CARD],path.join(conveyorRepo,'configs/commentops-fast',`${account}.json`),'INVALID_ACCOUNT_CARD');
  return Object.freeze({
    ...definition,
    conveyorRepo,
    providerRepo,
    providerRootExplicit,
    runtime,
    providerNode,
    configFile,
    cardFile,
  });
}

export function resolveAdapterPaths(account='likeavto',options={}) {
  return resolvePaths(account,options);
}

function validObjectIds(value) {
  return Array.isArray(value)&&value.length>0&&value.length<=12&&new Set(value).size===value.length&&value.every(id=>typeof id==='string'&&SAFE_ID.test(id));
}

export function accountBinding(account,config) {
  const definition=accountDefinition(account);
  if(!config||config.scope?.stableAccountKey!==definition.accountKey||!validObjectIds(config.accountObjectIds)||typeof config.scope?.objectId!=='string'||!config.accountObjectIds.includes(config.scope.objectId))reject('SCOPE_UNAVAILABLE');
  return Object.freeze({
    accountKey:definition.accountKey,
    providerAccountId:definition.providerAccountId,
    displayName:definition.displayName,
    primaryObjectId:config.scope.objectId,
    objectIds:Object.freeze([...config.accountObjectIds]),
  });
}

export async function validateScope(account,{paths=resolveAdapterPaths(account),readFileFn=readFile,requireExecution=false}={}) {
  const definition=accountDefinition(account);
  if(paths?.accountKey!==definition.accountKey)reject('SCOPE_UNAVAILABLE');
  let config,card;
  try {
    [config,card]=await Promise.all([
      readFileFn(paths.configFile,'utf8').then(JSON.parse),
      readFileFn(paths.cardFile,'utf8').then(JSON.parse),
    ]);
  } catch {reject('SCOPE_UNAVAILABLE');}
  if(card?.account!==definition.accountKey||
      typeof card.provider_root!=='string'||!path.isAbsolute(card.provider_root)||
      (!paths.providerRootExplicit&&!samePath(card.provider_root,paths.providerRepo))||
      config?.scope?.stableAccountKey!==definition.accountKey||
      !validObjectIds(config.accountObjectIds)||
      typeof config.scope?.objectId!=='string'||!config.accountObjectIds.includes(config.scope.objectId)||
      (requireExecution&&config.executionMode?.mode!=='reviewed-comment-ops-v1'))reject('SCOPE_UNAVAILABLE');
  return Object.freeze({config,binding:accountBinding(account,config),paths});
}

// Compatibility exports for LikeAvto-only adapters. New account-aware code must
// call resolveAdapterPaths(request.account) instead of importing these defaults.
const legacy=resolvePaths('likeavto',{},false);
export const conveyorRepo=legacy.conveyorRepo;
export const providerRepo=legacy.providerRepo;
export const runtime=legacy.runtime;
export const providerNode=legacy.providerNode;
export const configFile=legacy.configFile;
export const cardFile=legacy.cardFile;

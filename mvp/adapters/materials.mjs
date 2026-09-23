import path from 'node:path';
import {accountDefinition,here,reject,validateScope} from './config.mjs';
import {runProcess} from './process.mjs';

const script = path.join(here, 'materials.py');

export async function runAccountMaterials(operation,req,timeoutMs,{runProcessFn=runProcess,validateScopeFn=validateScope}={}) {
  if(operation!=='materials')reject('UNSUPPORTED_OPERATION');
  if (!req || Array.isArray(req) || typeof req !== 'object') reject('INVALID_REQUEST');
  const definition=accountDefinition(req.account);
  if (!definition.supportsMaterials) reject('MATERIALS_UNAVAILABLE');
  const {binding,paths}=await validateScopeFn(req.account);
  if(binding.accountKey!==definition.accountKey||binding.displayName!==definition.displayName)reject('ACCOUNT_SCOPE_MISMATCH');
  const python=process.env.COMMUNITYHERO_PYTHON??(process.env.COMMUNITYHERO_RUNTIME_MODE==='portable'?'':path.join(paths.conveyorRepo,'.venv/Scripts/python.exe'));
  if(!python.trim()||!path.isAbsolute(python))reject('MATERIALS_RUNTIME_UNAVAILABLE');
  const env={...process.env,COMMUNITYHERO_CONVEYOR_ROOT:paths.conveyorRepo,COMMUNITYHERO_ACCOUNT_CARD:paths.cardFile};
  const {stdout} = await runProcessFn(python, ['-B', script], {
    input: JSON.stringify({...req,accountObjectIds:binding.objectIds,operation}),
    cwd: path.resolve(here, '..'),
    env,
    timeoutMs,
    maxOutputBytes: 16 * 1024 * 1024,
  });
  let envelope;
  try { envelope = JSON.parse(stdout); }
  catch { reject('MATERIALS_PROTOCOL_ERROR'); }
  if (!envelope?.ok) reject((envelope?.error?.code || 'MATERIALS_UNAVAILABLE').toUpperCase());
  return envelope.result;
}

export const runMaterials = req => runAccountMaterials('materials', req, 120000);

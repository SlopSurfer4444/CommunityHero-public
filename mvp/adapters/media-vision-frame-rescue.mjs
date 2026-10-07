import fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import {createHash,randomUUID} from 'node:crypto';
import {stableJson} from './media-vision.mjs';
import {CODEX_MODEL} from './codex-model-policy.mjs';

const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
const SHA=/^[a-f0-9]{64}$/;
const fail=code=>{throw Object.assign(new Error(code),{code});};
const exact=(value,keys)=>value&&typeof value==='object'&&!Array.isArray(value)&&
  Object.keys(value).sort().join('|')===keys.slice().sort().join('|');
const same=(a,b)=>stableJson(a)===stableJson(b);
const sourceBudgetKey=request=>sha(stableJson({account:request.account,mediaSha256:request.source.mediaSha256}));
const guardName=sourceKey=>`vision-frame-rescue-budget-${sourceKey}.lock`;
const sameStat=(a,b)=>a.dev===b.dev&&a.ino===b.ino&&a.size===b.size&&a.mtimeMs===b.mtimeMs&&a.ctimeMs===b.ctimeMs;

// Bounded observational evidence, never a lease or permission to remove a lock.
async function inspectGuardFile(file,sourceKey){
  let handle;
  try{
    const before=await fs.lstat(file);
    if(!before.isFile()||before.isSymbolicLink()||before.size>16384)return {state:'invalid'};
    handle=await fs.open(file,'r');
    if(!sameStat(before,await handle.stat()))return {state:'changed_during_inspection'};
    const bytes=Buffer.alloc(16385),{bytesRead}=await handle.read(bytes,0,bytes.length,0);
    if(bytesRead!==before.size||!sameStat(before,await handle.stat())||!sameStat(before,await fs.lstat(file)))
      return {state:'changed_during_inspection'};
    const contents=bytes.subarray(0,bytesRead),sha256=sha(contents);
    if(bytesRead===0)return {state:'legacy_empty',sha256};
    let owner;try{owner=JSON.parse(contents.toString('utf8'));}catch{return {state:'invalid',sha256};}
    if(!exact(owner,['schemaVersion','sourceKey','ownerToken','pid','hostname','createdAtUtc','permitId','permitSha256','manifestSha256','leaseId'])||
      owner.schemaVersion!==1||owner.sourceKey!==sourceKey||typeof owner.ownerToken!=='string'||
      !/^[a-f0-9-]{36}$/.test(owner.ownerToken)||!Number.isSafeInteger(owner.pid)||owner.pid<1||
      typeof owner.hostname!=='string'||owner.hostname.length<1||owner.hostname.length>255||
      typeof owner.createdAtUtc!=='string'||!Number.isFinite(Date.parse(owner.createdAtUtc))||
      !/^rescue-[a-f0-9]{32}$/.test(owner.permitId)||!SHA.test(owner.permitSha256)||!SHA.test(owner.manifestSha256)||
      typeof owner.leaseId!=='string'||owner.leaseId.length>1024)return {state:'invalid',sha256};
    return {state:'owner_metadata',sha256,owner};
  }catch(error){return {state:error.code==='ENOENT'?'absent':'unreadable'};}
  finally{if(handle)await handle.close();}
}

// Read-only manual-recovery evidence. PID, hostname and age are hints only:
// PID reuse, remote writers and the crash-before-metadata window preclude safe
// automatic stealing. Independent writer cessation and preservation/readback
// of every attempt charge remain prerequisites outside this API.
export async function inspectFrameRescueBudgetGuard(dataDir,request){
  const sourceKey=sourceBudgetKey(request),fileName=guardName(sourceKey);
  return {schemaVersion:1,sourceKey,fileName,guard:await inspectGuardFile(path.join(dataDir,fileName),sourceKey),
    automaticRecoveryAllowed:false,ownerCessationProven:false,chargePreservationVerified:false,
    attemptFilePrefix:`vision-frame-rescue-attempt-${sourceKey}-`};
}

// Internal exact-frame authorization derived from trusted v2 company routing.
// It cannot lift a source-wide cloud threshold or authorize another chunk.
export function validateFrameRescuePermit(permit,request,local,policySha256,instructionSha256){
  const invalid=()=>fail('MEDIA_VISION_RESCUE_PERMIT_INVALID');
  if(!exact(permit,['schemaVersion','permitId','account','source','inventory','chunk','local','cloud','frames'])||
    permit.schemaVersion!==2||!/^rescue-[a-f0-9]{32}$/.test(permit.permitId)||permit.account!==request.account||
    !same(permit.source,request.source)||!same(permit.inventory,request.inventory)||
    !same(permit.chunk,{firstSelectionIndex:request.chunk.firstSelectionIndex,
      endSelectionIndexExclusive:request.chunk.endSelectionIndexExclusive,previousReceiptSha256:request.chunk.previousReceiptSha256})||
    !same(permit.local,{endpoint:local.endpoint,model:local.model,digest:local.digest,policySha256})||
    !exact(permit.cloud,['backend','model','instructionSha256','maximumFrames','maximumInvocations'])||
    permit.cloud.backend!=='codex_luna'||permit.cloud.model!==CODEX_MODEL||
    permit.cloud.instructionSha256!==instructionSha256||!SHA.test(policySha256)||!SHA.test(instructionSha256)||
    !Array.isArray(permit.frames)||permit.frames.length<1||permit.frames.length>32||
    permit.cloud.maximumFrames!==permit.frames.length||permit.cloud.maximumInvocations!==Math.ceil(permit.frames.length/4))invalid();
  const seen=new Set();
  for(const frame of permit.frames){
    if(!exact(frame,['id','frameIndex','selectionIndex','pixelSha256'])||seen.has(frame.id))invalid();
    const input=request.frames.find(f=>f.id===frame.id);
    if(!input||!same(frame,{id:input.id,frameIndex:input.frameIndex,selectionIndex:input.selectionIndex,pixelSha256:input.pixelSha256}))invalid();
    seen.add(frame.id);
  }
  return {permit:structuredClone(permit),sha256:sha(stableJson(permit))};
}

export function automaticFrameRescue(config,request,local,policySha256,instructionSha256,missing){
  if(config.schemaVersion!==2||config.automaticFallback!==true)return null;
  const bounds=config.boundedLocalFallback;
  if(!bounds||missing.length<1||missing.length>bounds.maxCloudFramesPerChunk||
    Math.ceil(missing.length/4)>bounds.maxCloudInvocationsPerChunk)fail('MEDIA_VISION_RESCUE_BUDGET_EXHAUSTED');
  const chunk={firstSelectionIndex:request.chunk.firstSelectionIndex,
    endSelectionIndexExclusive:request.chunk.endSelectionIndexExclusive,previousReceiptSha256:request.chunk.previousReceiptSha256};
  // Stable across lease/work IDs: retrying an uncertain chunk cannot obtain a
  // fresh budget merely by claiming a new job lease.
  const identity={account:request.account,source:request.source,inventory:request.inventory,chunk};
  const permit={schemaVersion:2,permitId:'rescue-'+sha(stableJson(identity)).slice(0,32),
    ...identity,local:{endpoint:local.endpoint,model:local.model,digest:local.digest,policySha256},
    cloud:{backend:'codex_luna',model:CODEX_MODEL,instructionSha256,
      maximumFrames:missing.length,maximumInvocations:Math.ceil(missing.length/4)},
    frames:missing.map(f=>({id:f.id,frameIndex:f.frameIndex,selectionIndex:f.selectionIndex,pixelSha256:f.pixelSha256}))};
  return {...validateFrameRescuePermit(permit,request,local,policySha256,instructionSha256),
    maxSourceFrames:bounds.maxCloudFramesPerSource};
}

// One durable, exclusive creation charges the entire request budget before any
// CLI invocation. A failed/unknown attempt stays charged; no refund/retry.
// The CLI may internally retry transport; this is not a provider-request cap.
export async function chargeFrameRescue(dataDir,rescue,request){
  const sourceKey=sourceBudgetKey(request);
  const prefix=`vision-frame-rescue-attempt-${sourceKey}-`;
  const file=path.join(dataDir,`${prefix}${rescue.permit.permitId}.json`);
  const guardPath=path.join(dataDir,guardName(sourceKey));
  let guard,ownerSha256;try{guard=await fs.open(guardPath,'wx',0o600);}catch(error){
    if(error.code==='EEXIST')throw Object.assign(new Error('MEDIA_VISION_RESCUE_BUDGET_BUSY'),
      {code:'MEDIA_VISION_RESCUE_BUDGET_BUSY',budgetGuard:await inspectFrameRescueBudgetGuard(dataDir,request)});
    throw error;
  }
  try{
    const owner={schemaVersion:1,sourceKey,ownerToken:randomUUID(),pid:process.pid,hostname:os.hostname(),
      createdAtUtc:new Date().toISOString(),permitId:rescue.permit.permitId,permitSha256:rescue.sha256,
      manifestSha256:request.manifestSha256,leaseId:request.chunk.leaseId};
    const ownerJson=stableJson(owner);
    await guard.writeFile(ownerJson);await guard.sync();ownerSha256=sha(ownerJson);
    let spent=0;
    for(const name of await fs.readdir(dataDir)){
      if(!name.startsWith(prefix)||!name.endsWith('.json'))continue;
      if(path.join(dataDir,name)===file)fail('MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED');
      const prior=path.join(dataDir,name),stat=await fs.lstat(prior);
      if(!stat.isFile()||stat.isSymbolicLink()||stat.size<1||stat.size>16384)fail('MEDIA_VISION_RESCUE_BUDGET_INVALID');
      let record;try{record=JSON.parse(await fs.readFile(prior,'utf8'));}catch{fail('MEDIA_VISION_RESCUE_BUDGET_INVALID');}
      if(record.schemaVersion!==1||record.sourceKey!==sourceKey||!Number.isSafeInteger(record.chargedCloudFrames)||
        record.chargedCloudFrames<1||record.chargedCloudFrames>32||record.chargedCloudInvocations!==Math.ceil(record.chargedCloudFrames/4))
        fail('MEDIA_VISION_RESCUE_BUDGET_INVALID');
      spent+=record.chargedCloudFrames;
    }
    if(spent+rescue.permit.cloud.maximumFrames>(rescue.maxSourceFrames??64))fail('MEDIA_VISION_RESCUE_BUDGET_EXHAUSTED');
    let handle;try{handle=await fs.open(file,'wx',0o600);}catch(error){
      if(error.code==='EEXIST')fail('MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED');throw error;
    }
    try{
      const record={schemaVersion:1,status:'charged_outcome_requires_receipt_readback',sourceKey,permitSha256:rescue.sha256,
        manifestSha256:request.manifestSha256,leaseId:request.chunk.leaseId,
        chargedCloudFrames:rescue.permit.cloud.maximumFrames,chargedCloudInvocations:rescue.permit.cloud.maximumInvocations,
        createdAtUtc:new Date().toISOString()};
      await handle.writeFile(stableJson(record));await handle.sync();
    }finally{await handle.close();}
  }finally{
    try{
      // Keep the descriptor open and refuse cleanup of an observed replacement.
      // Manual recovery must first stop all writers: filesystem unlink has no
      // portable compare-and-delete operation for a concurrently replaced path.
      const owned=await guard.stat({bigint:true});
      let current;try{current=await fs.lstat(guardPath,{bigint:true});}catch(error){
        if(error.code==='ENOENT')fail('MEDIA_VISION_RESCUE_GUARD_OWNERSHIP_LOST');throw error;
      }
      if(!current.isFile()||current.isSymbolicLink()||owned.ino===0n||!sameStat(owned,current)||
        (ownerSha256&&(await inspectGuardFile(guardPath,sourceKey)).sha256!==ownerSha256))
        fail('MEDIA_VISION_RESCUE_GUARD_OWNERSHIP_LOST');
      await fs.unlink(guardPath);
    }finally{await guard.close();}
  }
}

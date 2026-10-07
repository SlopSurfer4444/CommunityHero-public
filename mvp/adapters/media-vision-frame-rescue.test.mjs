import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {automaticFrameRescue,validateFrameRescuePermit,chargeFrameRescue,inspectFrameRescueBudgetGuard} from './media-vision-frame-rescue.mjs';

const local={endpoint:'http://127.0.0.1:11434',model:'fixture:1',digest:'e'.repeat(64)};
const policy='a'.repeat(64),instruction='b'.repeat(64);
const config={schemaVersion:2,automaticFallback:true,boundedLocalFallback:{maxCloudFramesPerChunk:32,maxCloudInvocationsPerChunk:8,maxCloudFramesPerSource:64}};
function request(count=8){return {account:'baw-russia',manifestSha256:'c'.repeat(64),
  source:{account:'BAW Russia',postKey:'post',mediaSha256:'d'.repeat(64),durationMs:10000},inventory:{selectionSha256:'f'.repeat(64)},
  chunk:{firstSelectionIndex:324,endSelectionIndexExclusive:356,previousReceiptSha256:'1'.repeat(64),leaseId:'lease-1'},
  frames:Array.from({length:count},(_,i)=>({id:`frame-${348+i}`,frameIndex:348+i,selectionIndex:348+i,pixelSha256:String(i%10).repeat(64)}))};}

test('v2 bounded rescue is disabled by false and v1; exact eight-frame allowance survives a source-local route',()=>{
  const req=request();
  for(const disabled of [{...config,automaticFallback:false},{...config,schemaVersion:1}])
    assert.equal(automaticFrameRescue(disabled,req,local,policy,instruction,req.frames),null);
  const rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
  assert.equal(rescue.permit.cloud.maximumFrames,8);assert.equal(rescue.permit.cloud.maximumInvocations,2);
  assert.equal(rescue.maxSourceFrames,64);
  const nextLease=structuredClone(req);nextLease.chunk.leaseId='other-lease';nextLease.manifestSha256='2'.repeat(64);
  assert.equal(automaticFrameRescue(config,nextLease,local,policy,instruction,req.frames).permit.permitId,rescue.permit.permitId);
  assert.throws(()=>automaticFrameRescue(config,request(33),local,policy,instruction,request(33).frames),{code:'MEDIA_VISION_RESCUE_BUDGET_EXHAUSTED'});
});

test('internal exact-frame authorization rejects changed source, account, selection, frame, policy, model, budget and duplicates',()=>{
  const req=request(),good=automaticFrameRescue(config,req,local,policy,instruction,req.frames).permit;
  for(const mutate of [p=>p.account='likeavto',p=>p.source.mediaSha256='0'.repeat(64),p=>p.inventory.selectionSha256='0'.repeat(64),
    p=>p.chunk.previousReceiptSha256='0'.repeat(64),p=>p.frames[0].pixelSha256='f'.repeat(64),p=>p.frames[0].selectionIndex++,
    p=>p.frames[0].id='foreign',p=>p.local.policySha256='0'.repeat(64),p=>p.local.model='foreign',p=>p.cloud.model='gpt-6-luna',p=>p.schemaVersion=1,
    p=>p.cloud.maximumFrames=9,p=>p.cloud.maximumInvocations=3,p=>p.frames[1]=p.frames[0]]){
    const bad=structuredClone(good);mutate(bad);
    assert.throws(()=>validateFrameRescuePermit(bad,req,local,policy,instruction),{code:'MEDIA_VISION_RESCUE_PERMIT_INVALID'});
  }
});

test('durable budget reserves before dispatch, blocks replay/new lease, and charges uncertain attempts cumulatively',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-budget-'));
  try{
    const req=request(32),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    await chargeFrameRescue(dir,rescue,req);
    const files=await fs.readdir(dir);assert.equal(files.length,1);
    const charge=JSON.parse(await fs.readFile(path.join(dir,files[0]),'utf8'));
    assert.equal(charge.chargedCloudFrames,32);assert.equal(charge.chargedCloudInvocations,8);
    assert.equal(charge.status,'charged_outcome_requires_receipt_readback');
    req.chunk.leaseId='new-lease';
    await assert.rejects(chargeFrameRescue(dir,rescue,req),{code:'MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED'});
    req.chunk.firstSelectionIndex=356;req.chunk.endSelectionIndexExclusive=388;
    const second=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    await chargeFrameRescue(dir,second,req);
    req.chunk.firstSelectionIndex=388;req.chunk.endSelectionIndexExclusive=420;
    await assert.rejects(chargeFrameRescue(dir,automaticFrameRescue(config,req,local,policy,instruction,req.frames),req),
      {code:'MEDIA_VISION_RESCUE_BUDGET_EXHAUSTED'});
    assert.equal((await fs.readdir(dir)).length,2);
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

test('concurrent budget reservation cannot issue duplicate charges',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-concurrent-'));
  try{
    const req=request(),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    const attempts=await Promise.allSettled([chargeFrameRescue(dir,rescue,req),chargeFrameRescue(dir,rescue,req)]);
    assert.equal(attempts.filter(a=>a.status==='fulfilled').length,1);
    assert.equal((await fs.readdir(dir)).length,1);
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

test('old empty and malformed budget guards remain blocked and inspection never changes charges',async()=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-stale-'));
  try{
    const req=request(),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    await chargeFrameRescue(dir,rescue,req);
    const [attemptName]=await fs.readdir(dir),attempt=await fs.readFile(path.join(dir,attemptName));
    const initial=await inspectFrameRescueBudgetGuard(dir,req);
    assert.equal(initial.guard.state,'absent');
    const guardPath=path.join(dir,initial.fileName);
    for(const [contents,state] of [['','legacy_empty'],['{broken','invalid'],['x'.repeat(16385),'invalid']]){
      await fs.writeFile(guardPath,contents);
      await fs.utimes(guardPath,new Date(0),new Date(0));
      const before=await fs.readFile(guardPath);
      await assert.rejects(chargeFrameRescue(dir,rescue,req),error=>{
        assert.equal(error.code,'MEDIA_VISION_RESCUE_BUDGET_BUSY');
        assert.equal(error.budgetGuard.guard.state,state);
        assert.equal(error.budgetGuard.automaticRecoveryAllowed,false);
        assert.equal(error.budgetGuard.ownerCessationProven,false);
        assert.equal(error.budgetGuard.chargePreservationVerified,false);
        return true;
      });
      assert.deepEqual(await fs.readFile(guardPath),before);
      assert.deepEqual(await fs.readFile(path.join(dir,attemptName)),attempt);
    }
    // Even after an operator removes only the guard, the durable attempt still
    // prevents dispatch under a fresh lease. The API itself never removes it.
    await fs.unlink(guardPath);req.chunk.leaseId='new-lease';
    await assert.rejects(chargeFrameRescue(dir,rescue,req),{code:'MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED'});
    assert.deepEqual(await fs.readFile(path.join(dir,attemptName)),attempt);
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

test('owner metadata is durable before charge and never proves cessation, including foreign or dead PID hints',async t=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-owner-'));
  try{
    const req=request(),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    const readdir=fs.readdir;
    let observed;
    const spy=t.mock.method(fs,'readdir',async function(...args){
      observed=await inspectFrameRescueBudgetGuard(dir,req);
      return readdir.apply(this,args);
    });
    await chargeFrameRescue(dir,rescue,req);spy.mock.restore();
    assert.equal(observed.guard.state,'owner_metadata');
    assert.equal(observed.guard.owner.pid,process.pid);
    assert.equal(observed.guard.owner.hostname,os.hostname());
    assert.equal(observed.guard.owner.permitSha256,rescue.sha256);
    assert.equal(observed.guard.owner.leaseId,req.chunk.leaseId);
    const [attemptName]=await fs.readdir(dir),attempt=await fs.readFile(path.join(dir,attemptName));
    const guardPath=path.join(dir,observed.fileName);
    for(const hints of [{pid:process.pid,hostname:os.hostname()},{pid:2147483647,hostname:'different-host'}]){
      const owner={...observed.guard.owner,...hints,createdAtUtc:new Date(0).toISOString()};
      await fs.writeFile(guardPath,JSON.stringify(owner));
      const before=await fs.readFile(guardPath),snapshot=await inspectFrameRescueBudgetGuard(dir,req);
      assert.equal(snapshot.guard.state,'owner_metadata');
      assert.deepEqual(snapshot.guard.owner,owner);
      assert.match(snapshot.guard.sha256,/^[a-f0-9]{64}$/);
      assert.equal(snapshot.ownerCessationProven,false);
      assert.equal(snapshot.automaticRecoveryAllowed,false);
      await assert.rejects(chargeFrameRescue(dir,rescue,req),{code:'MEDIA_VISION_RESCUE_BUDGET_BUSY'});
      assert.deepEqual(await fs.readFile(guardPath),before);
      assert.deepEqual(await fs.readFile(path.join(dir,attemptName)),attempt);
    }
    assert.equal((await inspectFrameRescueBudgetGuard(dir,{...req,account:'likeavto'})).guard.state,'absent');
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

test('cleanup preserves a replaced or edited guard and a charge already made before ownership loss',async t=>{
  for(const replace of [true,false]){
    const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-ownership-'));
    try{
      const req=request(),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
      const initial=await inspectFrameRescueBudgetGuard(dir,req),guardPath=path.join(dir,initial.fileName);
      const readdir=fs.readdir;
      const spy=t.mock.method(fs,'readdir',async function(...args){
        if(replace)await fs.rename(guardPath,guardPath+'.original');
        await fs.writeFile(guardPath,'replacement-must-survive');
        return readdir.apply(this,args);
      });
      await assert.rejects(chargeFrameRescue(dir,rescue,req),{code:'MEDIA_VISION_RESCUE_GUARD_OWNERSHIP_LOST'});
      spy.mock.restore();
      assert.equal(await fs.readFile(guardPath,'utf8'),'replacement-must-survive');
      const attemptName=(await fs.readdir(dir)).find(name=>name.startsWith(initial.attemptFilePrefix));
      assert.ok(attemptName);
      const charge=await fs.readFile(path.join(dir,attemptName));
      assert.equal(JSON.parse(charge).chargedCloudFrames,8);
      await fs.unlink(guardPath);
      await assert.rejects(chargeFrameRescue(dir,rescue,req),{code:'MEDIA_VISION_RESCUE_ALREADY_ATTEMPTED'});
      assert.deepEqual(await fs.readFile(path.join(dir,attemptName)),charge);
    }finally{await fs.rm(dir,{recursive:true,force:true});}
  }
});

test('partial owner metadata write fails before a new charge and only releases its own guard',async t=>{
  const dir=await fs.mkdtemp(path.join(os.tmpdir(),'ch-rescue-write-failure-'));
  try{
    const req=request(),rescue=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    await chargeFrameRescue(dir,rescue,req);
    const [attemptName]=await fs.readdir(dir),attempt=await fs.readFile(path.join(dir,attemptName));
    req.chunk.firstSelectionIndex=356;req.chunk.endSelectionIndexExclusive=388;
    const next=automaticFrameRescue(config,req,local,policy,instruction,req.frames);
    const open=fs.open;
    const spy=t.mock.method(fs,'open',async function(file,flags,...args){
      const handle=await open.call(this,file,flags,...args);
      if(file.endsWith('.lock')&&flags==='wx')handle.writeFile=async()=>{
        await handle.write('{');throw Object.assign(new Error('fixture disk failure'),{code:'EIO'});
      };
      return handle;
    });
    await assert.rejects(chargeFrameRescue(dir,next,req),{code:'EIO'});spy.mock.restore();
    assert.deepEqual(await fs.readdir(dir),[attemptName]);
    assert.deepEqual(await fs.readFile(path.join(dir,attemptName)),attempt);
  }finally{await fs.rm(dir,{recursive:true,force:true});}
});

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';
import {assistantLaneForRequest,withAssistantLane,assistantCliArgs,copyAssistantLogin} from './assistant.mjs';
import {runProcess} from './process.mjs';

const scratch=async()=>fs.mkdtemp(path.join(os.tmpdir(),'communityhero-lanes-test-'));
async function removeScratch(base){
  if(path.dirname(base)===os.tmpdir()&&path.basename(base).startsWith('communityhero-lanes-test-'))
    await fs.rm(base,{recursive:true,force:true});
}

test('discussion and research use interactive; triage and review use preparation',()=>{
  for(const prepared of [{triage:false},{research:true},{triage:false,review:false}])
    assert.equal(assistantLaneForRequest(prepared),'interactive');
  for(const prepared of [{triage:true},{triage:true,review:true}])
    assert.equal(assistantLaneForRequest(prepared),'preparation');
});

test('separate fake processes can overlap across lanes while same-lane calls remain bounded',async()=>{
  const base=await scratch();
  let entered;const firstEntered=new Promise(resolve=>entered=resolve);
  let release;const hold=new Promise(resolve=>release=resolve);
  try{
    const sourceHome=path.join(base,'source');await fs.mkdir(sourceHome);
    await fs.writeFile(path.join(sourceHome,'auth.json'),'{"synthetic":true}');
    const preparation=withAssistantLane(base,'preparation',async home=>{
      assert.equal(path.dirname(home),path.join(base,'preparation'));
      assert.equal(assistantCliArgs(home).includes(home),true);
      await copyAssistantLogin(sourceHome,home);
      entered();await hold;
      assert.equal(await fs.readFile(path.join(home,'auth.json'),'utf8'),'{"synthetic":true}');
      await runProcess(process.execPath,['-e','setTimeout(()=>{},80)'],{cwd:home,timeoutMs:2000});
      return home;
    });
    await firstEntered;
    await assert.rejects(withAssistantLane(base,'preparation',async()=>{throw new Error('second preparation started');}),
      {code:'ASSISTANT_BUSY'});
    const interactive=await withAssistantLane(base,'interactive',async home=>{
      assert.equal(path.dirname(home),path.join(base,'interactive'));
      assert.equal((await fs.stat(path.join(base,'preparation','model.lock'))).isFile(),true);
      await copyAssistantLogin(sourceHome,home);
      await fs.writeFile(path.join(home,'auth.json'),'{"synthetic":"refreshed-in-this-lane"}');
      await runProcess(process.execPath,['-e','setTimeout(()=>{},80)'],{cwd:home,timeoutMs:2000});
      return home;
    });
    release();const preparedHome=await preparation;
    assert.notEqual(interactive,preparedHome);
    assert.equal(await fs.readFile(path.join(sourceHome,'auth.json'),'utf8'),'{"synthetic":true}');
    for(const lane of ['interactive','preparation']){
      const contents=await fs.readdir(path.join(base,lane));
      assert.deepEqual(contents,[],'lane private run and lock are removed after completion');
    }
  }finally{release?.();await removeScratch(base);}
});

test('stale recovery and legacy lock check stay inside the selected lane', {skip:process.platform!=='win32'},async()=>{
  const base=await scratch();
  const preparationBase=path.join(base,'preparation');
  const interactiveBase=path.join(base,'interactive');
  try{
    await fs.mkdir(preparationBase);await fs.mkdir(interactiveBase);
    const other=path.join(interactiveBase,'keep.txt');await fs.writeFile(other,'keep');
    const staleHome=path.join(preparationBase,'run-stale');await fs.mkdir(staleHome);
    await fs.writeFile(path.join(staleHome,'sentinel'),'stale');
    const child=spawn(process.execPath,['-e',''],{windowsHide:true,stdio:'ignore'});
    const pid=child.pid;
    await new Promise((resolve,reject)=>{child.once('exit',resolve);child.once('error',reject);});
    await fs.writeFile(path.join(preparationBase,'model.lock'),JSON.stringify({pid,home:staleHome}));
    await withAssistantLane(base,'preparation',async home=>{
      assert.notEqual(home,staleHome);
      await assert.rejects(fs.stat(staleHome),{code:'ENOENT'});
      assert.equal(await fs.readFile(other,'utf8'),'keep');
    });
    assert.equal(await fs.readFile(other,'utf8'),'keep');
    await fs.writeFile(path.join(base,'model.lock'),JSON.stringify({pid:process.pid}));
    await assert.rejects(withAssistantLane(base,'interactive',async()=>{}),{code:'ASSISTANT_BUSY'});
    assert.equal((await fs.readdir(interactiveBase)).includes('model.lock'),false);
  }finally{await removeScratch(base);}
});

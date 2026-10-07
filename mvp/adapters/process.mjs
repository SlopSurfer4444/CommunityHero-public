import { spawn } from 'node:child_process';
import path from 'node:path';
import {StringDecoder} from 'node:string_decoder';

// Project only OS facts; child output, arguments and free-form messages are private.
export function processExitFacts(exit) {
  return {
    ...(Number.isInteger(exit?.exitCode)&&exit.exitCode>=-2147483648&&exit.exitCode<=4294967295?{exitCode:exit.exitCode}:{}),
    ...(['SIGABRT','SIGBUS','SIGFPE','SIGHUP','SIGILL','SIGINT','SIGKILL','SIGPIPE','SIGQUIT','SIGSEGV','SIGTERM'].includes(exit?.signal)?{signal:exit.signal}:{})
  };
}

export function runProcess(executable, args, {input = '', cwd, env = process.env, timeoutMs = 120000, idleTimeoutMs, maxOutputBytes = 8 * 1024 * 1024, onStdout, processRole} = {}) {
  if(idleTimeoutMs!==undefined&&(!Number.isSafeInteger(idleTimeoutMs)||idleTimeoutMs<1))
    return Promise.reject(Object.assign(new Error('Invalid idle timeout'),{code:'ADAPTER_INVALID_TIMEOUT'}));
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, {cwd, env, windowsHide: true, shell: false, stdio: ['pipe','pipe','pipe']});
    let finished = false, bytes = 0, failure = null, killerPending = null, idleTimer, timeoutKind;
    const stdout = [], stderr = [];
    const outputBytes={stdout:0,stderr:0};
    const stdoutDecoder=new StringDecoder('utf8');
    const killTree = () => {
      if (!child.pid) return;
      if (process.platform === 'win32') {
        killerPending = new Promise(resolve => {
          const killer = spawn(path.join(process.env.SystemRoot || 'C:/Windows', 'System32/taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], {windowsHide:true, stdio:'ignore'});
          killer.on('error', () => {child.kill(); resolve();});
          killer.on('close', resolve);
        });
      } else child.kill('SIGKILL');
    };
    const cleanup = () => {clearTimeout(timer);clearTimeout(idleTimer); process.off('SIGINT', cancel); process.off('SIGTERM', cancel);};
    const fail = (code,kind) => {if (finished || failure) return; failure = code;timeoutKind=kind; killTree();};
    const cancel = () => fail('CANCELLED');
    const timer = setTimeout(() => fail('ADAPTER_TIMEOUT','total'), timeoutMs);
    // Only an explicit true from the caller's validated stdout observer counts
    // as progress. Stderr, arbitrary bytes and incomplete records cannot renew it.
    const progress=()=>{if(idleTimeoutMs!==undefined){clearTimeout(idleTimer);idleTimer=setTimeout(()=>fail('ADAPTER_TIMEOUT','idle'),idleTimeoutMs);}};
    progress();
    process.once('SIGINT', cancel); process.once('SIGTERM', cancel);
    for (const [stream, chunks, key] of [[child.stdout, stdout,'stdout'], [child.stderr, stderr,'stderr']]) stream.on('data', data => {bytes += data.length; outputBytes[key]=Math.min(2147483647,outputBytes[key]+data.length); if (bytes > maxOutputBytes) fail('ADAPTER_OUTPUT_LIMIT'); else chunks.push(data);});
    if(onStdout)child.stdout.on('data',data=>{if(!failure)try{if(onStdout(stdoutDecoder.write(data))===true)progress();}catch(e){fail(e.code||'ADAPTER_OBSERVER_FAILED');}});
    child.on('error', () => fail('ADAPTER_PROCESS_UNAVAILABLE'));
    child.stdin.on('error', () => {});
    child.on('close', async (code,signal) => {if (finished) return; finished=true; cleanup(); if(killerPending) await killerPending; if (failure || code !== 0) {const e = new Error(failure || 'Adapter subprocess failed'); e.code=failure || 'ADAPTER_PROCESS_FAILED';if(timeoutKind)e.timeoutKind=timeoutKind; e.processExit=processExitFacts({exitCode:code,signal}); e.outputBytes={...outputBytes}; if(['provider-process','provider-worker','credential-helper','legacy-process-guard'].includes(processRole)){e.processRole=processRole;if(Number.isSafeInteger(child.pid)&&child.pid>0)e.processId=child.pid;} reject(e); return;} resolve({code,stdout:Buffer.concat(stdout).toString('utf8'),stderr:Buffer.concat(stderr).toString('utf8')});});
    child.stdin.end(input);
  });
}

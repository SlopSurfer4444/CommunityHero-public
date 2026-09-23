import { spawn } from 'node:child_process';
import path from 'node:path';
import {StringDecoder} from 'node:string_decoder';

export function runProcess(executable, args, {input = '', cwd, env = process.env, timeoutMs = 120000, maxOutputBytes = 8 * 1024 * 1024, onStdout} = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, args, {cwd, env, windowsHide: true, shell: false, stdio: ['pipe','pipe','pipe']});
    let finished = false, bytes = 0, failure = null, killerPending = null;
    const stdout = [], stderr = [];
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
    const cleanup = () => {clearTimeout(timer); process.off('SIGINT', cancel); process.off('SIGTERM', cancel);};
    const fail = code => {if (finished || failure) return; failure = code; killTree();};
    const cancel = () => fail('CANCELLED');
    const timer = setTimeout(() => fail('ADAPTER_TIMEOUT'), timeoutMs);
    process.once('SIGINT', cancel); process.once('SIGTERM', cancel);
    for (const [stream, chunks] of [[child.stdout, stdout], [child.stderr, stderr]]) stream.on('data', data => {bytes += data.length; if (bytes > maxOutputBytes) fail('ADAPTER_OUTPUT_LIMIT'); else chunks.push(data);});
    if(onStdout)child.stdout.on('data',data=>{if(!failure)try{onStdout(stdoutDecoder.write(data));}catch(e){fail(e.code||'ADAPTER_OBSERVER_FAILED');}});
    child.on('error', () => fail('ADAPTER_PROCESS_UNAVAILABLE'));
    child.stdin.on('error', () => {});
    child.on('close', async code => {if (finished) return; finished=true; cleanup(); if(killerPending) await killerPending; if (failure || code !== 0) {const e = new Error(failure || 'Adapter subprocess failed'); e.code=failure || 'ADAPTER_PROCESS_FAILED'; reject(e); return;} resolve({code,stdout:Buffer.concat(stdout).toString('utf8'),stderr:Buffer.concat(stderr).toString('utf8')});});
    child.stdin.end(input);
  });
}

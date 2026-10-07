import {readdir,readFile} from 'node:fs/promises';
import path from 'node:path';
import {reject} from './config.mjs';

/** Read only process metadata; no command is executed on a portable host. */
export async function assertNoLinuxLegacyConveyor(account,configFile,{readdirFn=readdir,readFileFn=readFile,procRoot='/proc'}={}) {
  let entries;try{entries=await readdirFn(procRoot);}catch{reject('CONVEYOR_INSPECTION_UNAVAILABLE');}
  const pids=entries.filter(name=>/^\d+$/.test(name));
  if(pids.length>16384)reject('CONVEYOR_INSPECTION_UNAVAILABLE');
  for(const pid of pids){
    let raw;try{raw=await readFileFn(path.posix.join(procRoot,pid,'cmdline'));}
    catch(error){if(error?.code==='ENOENT'||error?.code==='ESRCH')continue;reject('CONVEYOR_INSPECTION_UNAVAILABLE');}
    if(raw.length>65536)reject('CONVEYOR_INSPECTION_UNAVAILABLE');
    const args=raw.toString('utf8').split('\0').filter(Boolean);
    if(!args.length)continue;
    const legacyPython=args.some(arg=>arg==='commentops_fast'||arg.endsWith('/commentops_fast/__main__.py'))&&args.includes('run')&&
      args.some((arg,index)=>arg==='--account'&&args[index+1]===account||arg===`--account=${account}`);
    const legacyNode=args.some(arg=>arg.endsWith('/fast-conveyor-cli.ts')||arg==='fast-conveyor-cli.ts')&&
      args.some((arg,index)=>arg==='--config'&&args[index+1]===configFile||arg===`--config=${configFile}`);
    if(legacyPython||legacyNode)reject('CONCURRENT_CONVEYOR_ACTIVE');
  }
}

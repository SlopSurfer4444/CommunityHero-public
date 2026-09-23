import {pathToFileURL} from 'node:url';
import path from 'node:path';

export async function dispatch() {
  const error = new Error('ADAPTER_DISABLED');
  error.code = 'ADAPTER_DISABLED';
  throw error;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  process.stdin.resume();
  for await (const _chunk of process.stdin) { /* drain bounded by the server */ }
  process.stdout.write(JSON.stringify({ok:false,error:{code:'ADAPTER_DISABLED',message:'ADAPTER_DISABLED'}}));
}

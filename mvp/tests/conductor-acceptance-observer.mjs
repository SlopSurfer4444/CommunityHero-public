// Optional exploratory observer. It never changes requests or responses.
// Never enable in a final acceptance run: even pure diagnostic IO changes timing.
import {appendFile} from 'node:fs/promises';
import path from 'node:path';
const fixture=process.env.COMMUNITYHERO_CONDUCTOR_FIXTURE_ROOT;
if(!fixture||path.basename(fixture)!=='fixture')throw Error('Observer requires own fixture');
const original=globalThis.fetch;
globalThis.fetch=async function(input,init){
  const response=await original.call(this,input,init);
  try{
    const url=new URL(typeof input==='string'||input instanceof URL?input:input.url);
    if(url.protocol!=='http:'||url.hostname!=='127.0.0.1'||url.pathname!=='/rpc')return response;
    const envelope=await response.clone().json();
    if(!Number.isInteger(envelope.status)||envelope.status<400)return response;
    const request=typeof init?.body==='string'?JSON.parse(init.body):{};
    const operation=typeof request.operation==='string'&&/^[a-zA-Z]{1,40}$/.test(request.operation)?request.operation:'unknown';
    const error=typeof envelope.body?.error==='string'?envelope.body.error.slice(0,1000):'No string error';
    await appendFile(path.join(fixture,'rpc-errors.jsonl'),JSON.stringify({operation,status:envelope.status,error,at:new Date().toISOString()})+'\n');
  }catch{/* Observation cannot change transport disposition. */}
  return response;
};

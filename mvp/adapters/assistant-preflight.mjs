import {createHash} from 'node:crypto';
import {accountDefinition} from './config.mjs';
import {assistantAccount,prepareAssistantRequest} from './assistant.mjs';
import {conservativeImageEvidenceBytes} from './assistant-images.mjs';

const MAX_REQUESTS=16;
const MAX_INPUT_BYTES=8*1024*1024;
const MAX_CAPTURE_BYTES=2_400_000;
const MAX_MODEL_BYTES=550_000;
const ASSISTANT_STDIN_BYTES=2*1024*1024;
// Rust's assistant bridge replaces the captured account value with its key and
// inserts operation:"assistant". Both account display/key pairs have equal UTF-8
// lengths; 64 bytes exceeds that fixed insertion and serialization punctuation.
const ASSISTANT_WIRE_ALLOWANCE=64;
const SHA256=/^[a-f0-9]{64}$/;
const invalid=()=>Object.assign(new Error('ASSISTANT_PREFLIGHT_INVALID_REQUEST'),
  {code:'ASSISTANT_PREFLIGHT_INVALID_REQUEST'});

// This operation prepares exact captured JSON without starting the assistant,
// probing credentials, reading images, or contacting a connector or model.
export function assistantPreflight(command) {
  let account;
  try {account=accountDefinition(command?.account);}
  catch {throw invalid();}
  const keys=command&&typeof command==='object'&&!Array.isArray(command)?Object.keys(command).sort():[];
  const expected=Object.hasOwn(command,'operation')?['account','operation','requests']:['account','op','requests'];
  if(keys.join('|')!==expected.sort().join('|')
    ||(command.op??command.operation)!=='assistant_preflight'
    ||!Array.isArray(command.requests)||command.requests.length<1
    ||command.requests.length>MAX_REQUESTS)throw invalid();
  let total=0;
  const results=[];
  for(const entry of command.requests){
    if(!entry||typeof entry!=='object'||Array.isArray(entry)
      ||Object.keys(entry).sort().join('|')!=='requestSha256|serializedRequest'
      ||typeof entry.serializedRequest!=='string'||typeof entry.requestSha256!=='string'
      ||!SHA256.test(entry.requestSha256))throw invalid();
    const length=Buffer.byteLength(entry.serializedRequest,'utf8');
    total+=length;
    if(length<2||length>MAX_CAPTURE_BYTES||total>MAX_INPUT_BYTES)throw invalid();
    const actual=createHash('sha256').update(entry.serializedRequest,'utf8').digest('hex');
    if(actual!==entry.requestSha256)throw invalid();
    let request;
    try {request=JSON.parse(entry.serializedRequest);}
    catch {throw invalid();}
    if(!request||typeof request!=='object'||Array.isArray(request))throw invalid();
    try {
      if(assistantAccount(request.account).accountKey!==account.accountKey)throw invalid();
      if(length+ASSISTANT_WIRE_ALLOWANCE>ASSISTANT_STDIN_BYTES){
        results.push({requestSha256:entry.requestSha256,status:'oversized',
          textBytes:null,boundedModelBytes:null,maxModelBytes:MAX_MODEL_BYTES});
        continue;
      }
      const prepared=prepareAssistantRequest(request);
      if(!prepared.singlePass||!prepared.compactOutput||!prepared.sharedModeration||!prepared.contextSufficient)
        throw invalid();
      const textBytes=Buffer.byteLength(prepared.input,'utf8');
      const boundedModelBytes=textBytes+conservativeImageEvidenceBytes(prepared);
      results.push({requestSha256:entry.requestSha256,
        status:boundedModelBytes<=MAX_MODEL_BYTES?'fits':'oversized',
        textBytes,boundedModelBytes,maxModelBytes:MAX_MODEL_BYTES});
    }catch(error){
      if(error?.code==='ASSISTANT_CONTEXT_TOO_LARGE'){
        results.push({requestSha256:entry.requestSha256,status:'oversized',
          textBytes:null,boundedModelBytes:null,maxModelBytes:MAX_MODEL_BYTES});
      }else throw invalid();
    }
  }
  return {version:1,results};
}

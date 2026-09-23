import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {runProcess} from './process.mjs';
import {assistantCliArgs, reviewModelCatalog} from './assistant.mjs';

const cli='C:/AIDev/DevTools/bin/codex.exe';
const pinnedHash='97d4d67419d0ac2f71342f9a5e850f9468aa622618de8ea823223edb9a91926a';
const maliciousCall={id:'call_forbidden_exec',type:'custom_tool_call',call_id:'call_forbidden_exec',
  name:'exec',input:"text('synthetic-probe')"};
const terminal={id:'msg_terminal',type:'message',role:'assistant',status:'completed',
  content:[{type:'output_text',text:'{"ok":true}',annotations:[]}]};

function sendHttpResponse(res, output) {
  res.writeHead(200,{'Content-Type':'text/event-stream'});
  const emit=(type,data)=>res.write(`event: ${type}\ndata: ${JSON.stringify({type,...data})}\n\n`);
  emit('response.created',{response:{id:'resp_test',object:'response',status:'in_progress',output:[]}});
  for(let i=0;i<output.length;i++) {
    emit('response.output_item.added',{output_index:i,item:output[i]});
    emit('response.output_item.done',{output_index:i,item:output[i]});
  }
  emit('response.completed',{response:{id:'resp_test',object:'response',status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}});
  res.end();
}

function sendWebSocketFrame(socket, type, data) {
  const payload=Buffer.from(JSON.stringify({type,...data}));
  const head=payload.length<126?Buffer.from([129,payload.length]):Buffer.from([129,126,payload.length>>8,payload.length&255]);
  socket.write(Buffer.concat([head,payload]));
}

function responseFor(request) {
  const returned=(request.input||[]).some(item=>item.type==='custom_tool_call_output'
    && item.call_id===maliciousCall.call_id);
  return returned?[terminal]:[maliciousCall,terminal];
}

// Offline probe: the mock model tries one forbidden custom exec call. If the
// CLI runs it, its custom_tool_call_output is visible on the next wire request.
test('pinned CLI rejects injected exec calls in constrained profiles and probes the prior first-pass args',
 {skip:process.platform!=='win32',timeout:90000},async t=>{
  assert.equal(createHash('sha256').update(await fs.readFile(cli)).digest('hex'),pinnedHash);
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-forbidden-tools-'));
  const catalog=JSON.parse((await runProcess(cli,['debug','models','--bundled'])).stdout);
  const constrainedCatalog=reviewModelCatalog(catalog);
  const requests=[];
  const sockets=new Set();
  const server=http.createServer(async(req,res)=>{
    let body='';for await(const chunk of req)body+=chunk;
    if(!req.url.endsWith('/responses')||req.method!=='POST'){res.writeHead(404);res.end();return;}
    const request=JSON.parse(body);requests.push(request);
    // Once the CLI reports rejection, return a terminal structured message.
    sendHttpResponse(res,responseFor(request));
  });
  server.on('upgrade',(req,socket)=>{
    sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{});
    const accept=createHash('sha1').update(req.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    let buffer=Buffer.alloc(0);
    socket.on('data',chunk=>{
      buffer=Buffer.concat([buffer,chunk]);
      while(buffer.length>=2){
        let len=buffer[1]&127,off=2;
        if(len===126){if(buffer.length<4)return;len=buffer.readUInt16BE(2);off=4;}
        if(len===127){if(buffer.length<10)return;len=Number(buffer.readBigUInt64BE(2));off=10;}
        const masked=!!(buffer[1]&128),mask=masked?buffer.subarray(off,off+4):null;if(masked)off+=4;
        if(buffer.length<off+len)return;
        const opcode=buffer[0]&15,raw=Buffer.from(buffer.subarray(off,off+len));buffer=buffer.subarray(off+len);
        if(opcode===8){socket.end();return;}if(opcode!==1)continue;
        if(mask)for(let i=0;i<raw.length;i++)raw[i]^=mask[i%4];
        const request=JSON.parse(raw.toString());requests.push(request);
        const output=responseFor(request);
        sendWebSocketFrame(socket,'response.created',{response:{id:'resp_test',object:'response',status:'in_progress',output:[]}});
        for(const [i,item] of output.entries()) {
          sendWebSocketFrame(socket,'response.output_item.added',{output_index:i,item});
          sendWebSocketFrame(socket,'response.output_item.done',{output_index:i,item});
        }
        sendWebSocketFrame(socket,'response.completed',{response:{id:'resp_test',object:'response',status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}});
      }
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  try {
    for(const profile of ['first-pass','review','prior-first-pass']) {
      const review=profile==='review';
      const requestStart=requests.length;
      const home=path.join(base,profile);await fs.mkdir(home);
      await fs.writeFile(path.join(home,'response.schema.json'),JSON.stringify({type:'object',properties:{ok:{type:'boolean'}},required:['ok'],additionalProperties:false}));
      await fs.writeFile(path.join(home,'instructions.txt'),'Return JSON only.');
      await fs.writeFile(path.join(home,'models.json'),JSON.stringify(constrainedCatalog));
      const args=assistantCliArgs(home,review);
      if(profile==='prior-first-pass') {
        // Reproduce the formerly deployed gaps while retaining read-only
        // sandboxing and all other disabled tools: use the native catalog and
        // omit the explicit first-pass request_user_input kill switch.
        for(let i=args.length-1;i>=0;i--) {
          if(args[i]==='model_catalog_json='+JSON.stringify(path.join(home,'models.json'))) args.splice(i-1,2);
          else if(args[i]==='tools.experimental_request_user_input.enabled=false') args.splice(i-1,2);
        }
      }
      args.splice(args.length-1,0,'-c',`openai_base_url="http://127.0.0.1:${server.address().port}/v1"`);
      const env={CODEX_HOME:home,HOME:home,USERPROFILE:home,OPENAI_BASE_URL:`http://127.0.0.1:${server.address().port}/v1`,OPENAI_API_KEY:'synthetic-probe-key'};
      for(const key of ['SystemRoot','WINDIR','TEMP','TMP'])if(process.env[key])env[key]=process.env[key];
      const result=await new Promise((resolve,reject)=>{
        const child=spawn(cli,args,{cwd:home,env,windowsHide:true});let stdout='',stderr='';
        const timeout=setTimeout(()=>child.kill(),35000);
        child.stdout.on('data',chunk=>stdout+=chunk);child.stderr.on('data',chunk=>stderr+=chunk);
        child.stdin.end('Synthetic application context.');
        child.on('error',reject);child.on('close',code=>{clearTimeout(timeout);resolve({code,stdout,stderr});});
      });
      const attempts=requests.slice(requestStart);
      assert.ok(attempts.length>0,`${profile} did not reach mock Responses endpoint`);
      const toolResults=attempts.flatMap(request=>(request.input||[]).filter(item=>
        item.type==='custom_tool_call_output'||item.type==='function_call_output'));
      assert.ok(toolResults.length>0,`${profile} did not report how the injected call was handled`);
      if(profile==='prior-first-pass') {
        const exposed=(attempts[0].tools||[]).some(tool=>tool.name==='exec'||tool.tools?.some(n=>n.name==='exec'));
        t.diagnostic(`prior first-pass request exposed exec=${exposed}; returned tool result=${String(toolResults[0].output||'')}`);
        assert.equal(exposed,false);
        assert.ok(toolResults.every(item=>String(item.output||'').includes('disabled')
          && !String(item.output||'').includes('synthetic-probe')),
          'prior first-pass returned an execution marker instead of a disabled-tool rejection');
      } else {
        assert.ok(toolResults.every(item=>item.type==='custom_tool_call_output'
          && item.call_id===maliciousCall.call_id && item.output==='unsupported custom tool call: exec'),
          `${profile} did not explicitly reject the injected exec`);
      }
      for(const request of attempts) {
        const additionalTools=(request.input||[]).filter(item=>item.type==='additional_tools');
        if(profile==='prior-first-pass') {
          const namespaces=additionalTools.flatMap(item=>(item.tools||[]).map(tool=>tool.name));
          t.diagnostic(`prior first-pass additional_tools namespaces=${JSON.stringify(namespaces)}`);
        }
        else assert.equal(additionalTools.length,0);
        const forbidden=(request.tools||[]).filter(tool=>tool.name==='exec'||tool.tools?.some(n=>n.name==='exec'));
        if(profile!=='prior-first-pass') assert.deepEqual(forbidden,[],`${profile} exposed exec in the request`);
      }
      // The response is recorded through the CLI JSON event stream, proving
      // that the attempted tool did not run before the structured completion.
      if(profile==='prior-first-pass') {
        assert.equal(result.code,0,result.stderr);
        assert.ok(toolResults.every(item=>item.output==='code-mode host is disabled'),
          'prior first-pass did not return the explicit disabled-host result');
        const events=result.stdout.split(/\r?\n/).filter(Boolean).map(line=>JSON.parse(line));
        assert.ok(events.some(event=>event.item?.type==='agent_message'||event.item?.type==='message'),
          'prior first-pass did not reach terminal structured output');
      } else {
        assert.equal(result.code,0,result.stderr);
        const events=result.stdout.split(/\r?\n/).filter(Boolean).map(line=>JSON.parse(line));
        assert.ok(events.some(event=>event.item?.type==='agent_message'||event.item?.type==='message'),
          `${profile} did not return a terminal JSON event`);
      }
    }
  } finally {
    for(const socket of sockets)socket.destroy();
    server.closeAllConnections();
    await new Promise(resolve=>server.close(resolve));
    if(path.dirname(base)===os.tmpdir()&&path.basename(base).startsWith('ch-forbidden-tools-'))await fs.rm(base,{recursive:true,force:true});
  }
});

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {runProcess} from './process.mjs';
import {assistantCliArgs,admitAssistantEvents,reviewModelCatalog,prepareAssistantRequest} from './assistant.mjs';

import {stageAssistantImages} from './assistant-images.mjs';

// Candidate 2026-09-30: pinned codex-cli 0.159.0, synthetic loopback
// Responses endpoint, no account credentials
// and no real model request. This proves the exact candidate args against a local fake API.
test('pinned CLI emits exact Sol high editorial wire with no tools and isolated synthetic credentials',
 {skip:process.platform!=='win32',timeout:90000},async()=>{
  const pinnedHash='86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70';
  const cli=process.env.COMMUNITYHERO_CODEX_CLI??fileURLToPath(new URL(`../runs/runtime-tools/codex-${pinnedHash}/codex.exe`,import.meta.url));
  assert.equal(createHash('sha256').update(await fs.readFile(cli)).digest('hex'),pinnedHash);
  const base=await fs.mkdtemp(path.join(os.tmpdir(),'ch-cli-isolation-'));
  const catalogEnv={CODEX_HOME:base,HOME:base,USERPROFILE:base};
  for(const k of ['SystemRoot','WINDIR','TEMP','TMP'])if(process.env[k])catalogEnv[k]=process.env[k];
  const catalog=JSON.parse(await fs.readFile(new URL('./fixtures/codex-sol61-catalog-0159.json',import.meta.url),'utf8'));
  const reviewCatalog=reviewModelCatalog(catalog,'sol61_high_v2');
  const requests=[];
  const sockets=new Set();
  const server=http.createServer(async(req,res)=>{
    let body='';for await(const chunk of req)body+=chunk;
    if (!req.url.endsWith('/responses')) {res.writeHead(404);res.end();return;}
    if(req.method!=='POST'){res.writeHead(400);res.end();return;}
    requests.push(JSON.parse(body));
    const message={id:'msg_test',type:'message',role:'assistant',status:'completed',content:[{type:'output_text',text:'{"ok":true}',annotations:[]}]};
    const web={id:'ws_test',type:'web_search_call',status:'completed',action:{type:'open_page',url:'https://www.example.com/spec'}};
    const output=requests.at(-1).tools?.length?[web,message]:[message];
    res.writeHead(200,{'Content-Type':'text/event-stream'});
    const emit=(type,data)=>res.write(`event: ${type}\ndata: ${JSON.stringify({type,...data})}\n\n`);
    emit('response.created',{response:{id:'resp_test',object:'response',status:'in_progress',output:[]}});
    for(let i=0;i<output.length;i++) {
      emit('response.output_item.added',{output_index:i,item:output[i]});
      emit('response.output_item.done',{output_index:i,item:output[i]});
    }
    emit('response.completed',{response:{id:'resp_test',object:'response',status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}});
    res.end();
  });
  server.on('upgrade',(req,socket)=>{
    sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{});
    const accept=createHash('sha1').update(req.headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    let buffer=Buffer.alloc(0);
    const emit=(type,data)=>{
      const payload=Buffer.from(JSON.stringify({type,...data}));
      const head=payload.length<126?Buffer.from([129,payload.length]):Buffer.from([129,126,payload.length>>8,payload.length&255]);
      socket.write(Buffer.concat([head,payload]));
    };
    socket.on('data',chunk=>{
      buffer=Buffer.concat([buffer,chunk]);
      while(buffer.length>=2){
        let len=buffer[1]&127,off=2;
        if(len===126){if(buffer.length<4)return;len=buffer.readUInt16BE(2);off=4;}
        if(len===127){if(buffer.length<10)return;len=Number(buffer.readBigUInt64BE(2));off=10;}
        const masked=!!(buffer[1]&128),mask=masked?buffer.subarray(off,off+4):null;if(masked)off+=4;
        if(buffer.length<off+len)return;
        const opcode=buffer[0]&15;const raw=Buffer.from(buffer.subarray(off,off+len));buffer=buffer.subarray(off+len);
        if(opcode===8){socket.end();return;}if(opcode!==1)continue;
        if(mask)for(let i=0;i<raw.length;i++)raw[i]^=mask[i%4];
        const request=JSON.parse(raw.toString());requests.push(request);
        const message={id:'msg_test',type:'message',role:'assistant',status:'completed',content:[{type:'output_text',text:'{"ok":true}',annotations:[]}]};
        const web={id:'ws_test',type:'web_search_call',status:'completed',action:{type:'open_page',url:'https://www.example.com/spec'}};
        const output=request.tools?.length?[web,message]:[message];
        emit('response.created',{response:{id:'resp_test',object:'response',status:'in_progress',output:[]}});
        for(let i=0;i<output.length;i++){
          emit('response.output_item.added',{output_index:i,item:output[i]});
          emit('response.output_item.done',{output_index:i,item:output[i]});
        }
        emit('response.completed',{response:{id:'resp_test',object:'response',status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}});
      }
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  try {
    for(const profile of ['editorial-sol-high-v1']) {
      const review=false;
      const requestStart=requests.length;
      const home=path.join(base,profile);await fs.mkdir(home);
      await fs.writeFile(path.join(home,'response.schema.json'),JSON.stringify({type:'object',properties:{ok:{type:'boolean'}},required:['ok'],additionalProperties:false}));
      await fs.writeFile(path.join(home,'instructions.txt'),'Return JSON only.');
      await fs.writeFile(path.join(home,'models.json'),JSON.stringify(reviewCatalog));
      const prepared=prepareAssistantRequest({items:[{id:'synthetic-comment',postId:'parent',text:'Кто делает такую машину?',attachments:[{type:'photo',url:'https://images.example.com/comment.png'}]}],posts:[{id:'parent',title:'Обзор седана Model Q',attachments:[{type:'photo',url:'https://images.example.com/parent.png'}]}],materials:[{id:'speech',kind:'transcript',postKey:'parent',text:'Сейчас назову цену. Чувствительным советую закрыть уши.',transcription:{partial:true,sourcePostKey:'parent'}}]});
      const imageBytes=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC','base64');
      const images=await stageAssistantImages(prepared,home,{download:async url=>{assert.ok(['https://images.example.com/comment.png','https://images.example.com/parent.png'].includes(url));return {bytes:imageBytes,mime:'image/png'};}});
      const args=assistantCliArgs(home,false,images.paths,false,'sol61_high_v2');
      args.splice(args.length-1,0,'-c',`openai_base_url="http://127.0.0.1:${server.address().port}/v1"`);

      const env={CODEX_HOME:home,HOME:home,USERPROFILE:home,OPENAI_BASE_URL:'http://127.0.0.1:'+server.address().port+'/v1',OPENAI_API_KEY:'synthetic-probe-key'};
      for(const k of ['SystemRoot','WINDIR','TEMP','TMP'])if(process.env[k])env[k]=process.env[k];
      const result=await new Promise((resolve,reject)=>{
        const p=spawn(cli,args,{cwd:home,env,windowsHide:true});let stdout='',stderr='';
        const timeout=setTimeout(()=>p.kill(),35000);
        p.stdout.on('data',d=>stdout+=d);p.stderr.on('data',d=>stderr+=d);p.stdin.end(`Use the following application context as data:\n${prepared.input}`);
        p.on('error',reject);p.on('close',code=>{clearTimeout(timeout);code===0?resolve({stdout,stderr}):reject(new Error(stderr));});
      });
      assert.ok(requests.length>requestStart);
      for(const request of requests.slice(requestStart)) {
        assert.equal(request.model,'gpt-6.1-sol','Wire model matches the exact editorial profile');
        assert.equal(request.reasoning?.effort,'high','Editorial actual wire uses high effort');
        const content=(request.input||[]).flatMap(item=>item.content||[]);
        const imageParts=content.filter(part=>part.type==='input_image');
        assert.equal((request.input||[]).filter(i=>i.type==='additional_tools').length,0,'No hidden tool catalogs, including warmup requests');
        const offered=request.tools||[];
        assert.deepEqual(offered.map(x=>({type:x.type,name:x.name,tools:x.tools?.map(t=>t.name)})),review?[{type:'namespace',name:'web',tools:['run']}]:[]);
        if(request.generate===false)continue;
        assert.equal(imageParts.length,2,'Actual wire request must contain both comment and linked post images');
        assert.match(imageParts[0].image_url,/^data:image\/png;base64,/);
        const inputText=content.filter(part=>part.type==='input_text').map(part=>part.text).join('\n');
        assert.ok(inputText.includes('synthetic-comment'));
        assert.ok(inputText.includes(images.manifest[0].sha256));
        assert.ok(inputText.includes('Чувствительным советую закрыть уши.'));
        assert.ok(inputText.includes('post_attachment'));
        assert.ok(inputText.includes('https://images.example.com/parent.png'));
      }
      assert.equal(admitAssistantEvents(result.stdout,false).calls,0);
    }
    assert.ok(requests.length>=1 && requests.length<=2);
  } finally {
    for(const socket of sockets)socket.destroy();
    server.closeAllConnections();
    await new Promise(resolve=>server.close(resolve));
    if(path.dirname(base)===os.tmpdir()&&path.basename(base).startsWith('ch-cli-isolation-'))await fs.rm(base,{recursive:true,force:true});
  }
});

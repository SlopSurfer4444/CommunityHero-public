// Synthetic, read-only browser fixture for workshop reading-state checks.
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {resolve,extname,sep} from 'node:path';

const root=resolve(import.meta.dirname,'../../workshop');
const port=Number(process.env.COMMUNITYHERO_FIXTURE_PORT||42918);
const listeners=new Set();
let generation=1;
const createdAt='2026-09-23T12:00:00+03:00';
const items=Array.from({length:60},(_,index)=>({id:`item-${index+1}`,branchId:`branch-${index+1}`,postId:'fixture-post',
  targetId:`message-${index+1}`,workflow:'attention',providerStatus:'inprogress',revision:1,draft:'',createdAt}));
const branches=items.map((item,index)=>({id:item.branchId,postId:'fixture-post',messages:[{id:item.targetId,
  author:`Автор ${index+1}`,role:'user',text:'Сколько стоит комплектация?',createdAt,time:'23 сент.'}]}));
const transcript=Array.from({length:90},(_,index)=>`Фрагмент ${index+1}. Подробная расшифровка видео для проверки позиции чтения после обновления страницы.`).join('\n\n');
const snapshot=()=>({workspaceVersion:`fixture-v${generation}`,operator:{id:'fixture-reader',name:'Тестовый оператор',role:'owner'},
  csrfToken:'fixture',account:'LikeAvto',items,branches,posts:[{id:'fixture-post',postKey:'fixture-post',account:'LikeAvto',
    channel:'VK',title:'Синтетический пост для проверки чтения',text:'Описание тестового поста',createdAt}],
  materials:[{id:'fixture-transcript',kind:'transcript',postKey:'fixture-post',account:'LikeAvto',text:transcript}],
  conversations:[],jobs:[],proposals:[],operations:[],sync:{status:'idle'}});
const catalog={entries:[{id:'fixture-rule',currentVersionId:'fixture-rule-v1'}],versions:[{id:'fixture-rule-v1',entryId:'fixture-rule',
  title:'Правило для тестового поста',text:'Сначала уточнять комплектацию. '.repeat(35),kind:'rule',status:'active',trust:'verified',
  validFrom:'2026-01-01T00:00:00Z',scope:{account:'LikeAvto',postKeys:['fixture-post']}}]};
const json=(response,value)=>{response.writeHead(200,{'Content-Type':'application/json; charset=utf-8','Cache-Control':'no-store'});response.end(JSON.stringify(value));};
const mime={'.html':'text/html','.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.json':'application/json','.svg':'image/svg+xml','.png':'image/png'};
const server=createServer(async(request,response)=>{
  const path=new URL(request.url,'http://127.0.0.1').pathname;
  if(request.method!=='GET'){response.writeHead(405);response.end();return;}
  if(path==='/api/session'){json(response,{operator:{id:'fixture-reader',name:'Тестовый оператор',role:'owner'},csrfToken:'fixture'});return;}
  if(path==='/api/bootstrap'){json(response,snapshot());return;}
  if(path==='/api/workspace-version'){json(response,{workspaceVersion:`fixture-v${generation}`,actorId:'fixture-reader',csrfToken:'fixture'});return;}
  if(path==='/api/bootstrap/delta'){response.writeHead(404);response.end();return;}
  if(path==='/api/knowledge/instructions'){json(response,catalog);return;}
  if(path==='/api/events'){
    response.writeHead(200,{'Content-Type':'text/event-stream','Cache-Control':'no-store','Connection':'keep-alive'});
    response.write(': fixture connected\n\n');listeners.add(response);request.on('close',()=>listeners.delete(response));return;
  }
  if(path==='/fixture/refresh'){
    generation++;for(const listener of listeners)listener.write('event: refresh\ndata: {}\n\n');
    json(response,{workspaceVersion:`fixture-v${generation}`});return;
  }
  const file=resolve(root,`.${path==='/'?'/index.html':path}`);
  if(file!==root&&!file.startsWith(root+sep)){response.writeHead(404);response.end();return;}
  try{const body=await readFile(file);response.writeHead(200,{'Content-Type':mime[extname(file)]||'application/octet-stream','Cache-Control':'no-store'});response.end(body);}
  catch{response.writeHead(404);response.end();}
});
server.listen(port,'127.0.0.1',()=>process.stdout.write(`synthetic fixture listening on http://127.0.0.1:${port}\n`));

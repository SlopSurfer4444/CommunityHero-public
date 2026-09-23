// Deterministic transport for isolated HTTP acceptance tests. Never calls a network.
import {readFile,appendFile} from 'node:fs/promises';
let raw=''; for await(const chunk of process.stdin) raw+=chunk;
const request=JSON.parse(raw);
const operation=request.operation;
const scenarioFile=process.env.COMMUNITYHERO_TEST_SCENARIO;
let scenario={};try{scenario=JSON.parse(await readFile(scenarioFile,'utf8'));}catch{}
const digest=scenario.changed?'b'.repeat(64):'a'.repeat(64);
const item={id:'item-11391-42',itemId:'42',objectId:'11391',postKey:'11391:post-7',conversationKey:'11391:thread-42',contextEvidenceDigest:digest,branchId:'branch-42',targetId:'comment-42',createdAt:'2026-09-18T12:00:00Z',providerStatus:'new',status:'new',platform:'vk',workflow:'attention',draft:'',revision:1,author:'Test Author',text:'Question',commentText:'Question',expectedStatuses:['new']};
if(scenario.autoData) item.createdAt=scenario.createdAt;
const snapshot={posts:[{id:'post-7',postKey:'11391:post-7',title:'Test post',text:'Verified test information',channel:'VK'}],branches:[{id:'branch-42',postId:'post-7',messages:[{id:'comment-42',author:'Test Author',text:'Question',role:'customer',createdAt:item.createdAt}]}],items:[item],notice:'Test-only data',live:{account:'LikeAvto',fetchedAt:new Date().toISOString(),hasMore:false,observedCount:1,readOnly:true}};
let result;
switch(operation){
case 'read':
 if(scenario.secondPageFailure&&request.cursor){process.stdout.write(JSON.stringify({ok:false,error:{code:'TEST_PAGE_FAILURE'}}));process.exit(0);}
 result={...snapshot,hasMore:!!scenario.pagination&&!request.cursor,cursor:scenario.pagination&&!request.cursor?'fake-second-page':null,window:request.window};break;
case 'context':result={...item,items:[item],contextEvidenceDigest:scenario.contextChanged?'b'.repeat(64):digest};break;
case 'materials':result={materials:[{id:'test-knowledge',title:'Test rule',text:'Only use verified information.',kind:'knowledge',revision:1}]};break;
case 'assistant':
  if(scenario.modelDelay) await new Promise(r=>setTimeout(r,scenario.modelDelay));
  if(['triage','triage_review'].includes(request.purpose)){
    const outcome=scenario.triageOutcome||'reply';
    if(process.env.COMMUNITYHERO_TEST_EFFECTS) await appendFile(process.env.COMMUNITYHERO_TEST_EFFECTS+'.prepare',JSON.stringify({itemIds:request.items.map(i=>i.id),purpose:request.purpose,firstPass:request.firstPass})+'\n');
    result={text:'Автоматический разбор',sources:[],assessments:request.items.map(i=>({itemId:i.id,outcome,reason:outcome==='needs_attention'?'Нет проверенных сведений о цене':'Достаточно данных для решения'})),proposals:outcome==='needs_attention'?[]:request.items.map(i=>({itemId:i.id,kind:outcome==='close'?'close':'reply_and_close',text:outcome==='close'?'':'Подготовленный ответ'}))};
    if(request.purpose==='triage_review') {
      const at=new Date().toISOString();
      result.runMetadata={schemaVersion:1,model:'test-review',reasoningEffort:'medium',promptVersion:'test',instructionSha256:'a'.repeat(64),inputSha256:'b'.repeat(64),cliSha256:'c'.repeat(64),elapsedMs:1,completedAt:at,research:{version:1,status:'no_sources',model:'test-review',reasoningEffort:'medium',instructionSha256:'a'.repeat(64),inputSha256:'b'.repeat(64),elapsedMs:1,completedAt:at,webCalls:0,sources:[]}};
    }
  }else result={text:'Prepared a test proposal using the supplied source.',sources:['test-knowledge'],proposals:[{itemId:item.id,kind:'reply_and_close',text:'Test reply'}]};break;
case 'execute':
  if(process.env.COMMUNITYHERO_TEST_EFFECTS) await appendFile(process.env.COMMUNITYHERO_TEST_EFFECTS,JSON.stringify(request)+'\n');
  result={account:'likeavto',results:request.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:scenario.uncertain?'unknown':'verified',receipt:scenario.uncertain?undefined:{id:'fake-receipt'}}))};break;
case 'readback':result={account:'likeavto',results:request.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:scenario.readbackUnknown?'unknown':'verified',receipt:{id:'fake-receipt'}}))};break;
case 'media':result={materials:[]};break;
default:process.stdout.write(JSON.stringify({ok:false,error:{code:'TEST_BAD_OPERATION',message:'Unsupported test operation'}}));process.exit(0);
}
  if(operation==='assistant' && scenario.runMetadata) result.runMetadata=scenario.runMetadata;
  process.stdout.write(JSON.stringify({ok:true,result}));

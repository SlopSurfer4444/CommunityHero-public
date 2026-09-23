import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {activeInstructions,displayInstructions,chronologicalHistory} from '../workshop/active-instructions.js';
import {createMvpConnection} from '../workshop/mvp-connection.js';
const now='2026-09-22T12:00:00Z';
const version=(id,extra={})=>({id,entryId:id,sourceMaterialId:id,kind:'rule',status:'active',trust:'verified',scope:{account:'account',postKeys:[]},validFrom:'2026-09-01T00:00:00Z',validUntil:null,text:id,...extra});
const catalog=versions=>({versions,entries:versions.map(v=>({id:v.entryId,currentVersionId:v.id}))});

test('legacy post rules never become global or cross connector/account scope',()=>{
  const rule=version('legacy',{scope:{account:'LikeAvto',postKeys:[]},companyImport:{companyKey:'likeavto',scope:{postAliases:[{namespace:'commentops-fast.post-key',value:'target'}]}}});
  const options={account:'LikeAvto',now,postKeys:['target'],posts:[{postKey:'target',account:'LikeAvto'}],connectorBinding:{connector:'angryspace',accountId:'LikeAvto',providerAccountId:'likeavto'}};
  assert.equal(activeInstructions(catalog([rule]),options).post.length,1);
  for(const overrides of [{postKeys:['other']},{posts:[{postKey:'target',account:'BAW Russia'}]},{connectorBinding:{...options.connectorBinding,connector:'vk'}},{connectorBinding:undefined}]){
    assert.deepEqual(activeInstructions(catalog([rule]),{...options,...overrides}),{global:[],post:[]});
  }
});

test('instructions show current admitted global and selected post rules, never stale/local/unverified text',()=>{
  const data=catalog([version('global'),version('post',{scope:{account:'account',postKeys:['p']}}),version('other',{scope:{account:'account',postKeys:['other']}}),version('alien',{scope:{account:'other-account',postKeys:[]}}),version('fact',{kind:'fact'}),version('pending',{status:'pending_review'}),version('untrusted',{trust:'source_only'}),version('expired',{validUntil:now}),version('future',{validFrom:'2027-01-01T00:00:00Z'})]);
  data.versions.push(version('old-head',{entryId:'global',text:'obsolete'}));
  const result=activeInstructions(data,{account:'account',postKeys:['p'],now});
  assert.deepEqual(result.global.map(v=>v.id),['global']);assert.deepEqual(result.post.map(v=>v.id),['post']);
  assert.equal(activeInstructions({entries:[{id:'missing',currentVersionId:'lost'}],versions:[]},{account:'account',now}),null);
});

test('numbered rule titles read naturally without mutating admitted order or text',()=>{
  const rows=[version('r7',{title:'7. Правило',text:'raw 7'}),version('r10',{title:'10. Правило',text:'raw 10'}),version('r12',{title:'12. Правило',text:'raw 12'}),version('r3',{title:'3. Правило',text:'raw 3'})];
  assert.deepEqual(displayInstructions(rows).map(row=>row.title),['3. Правило','7. Правило','10. Правило','12. Правило']);
  assert.deepEqual(rows.map(row=>row.id),['r7','r10','r12','r3']);
  assert.equal(displayInstructions(rows)[0].text,'raw 3');
});

function importedVersion(marker,values) {
  const field=marker.field,editorial=!!marker.symbol;
  const provenance=editorial?{path:'commentops_fast/decision.py',symbol:marker.symbol}
    :{source:field==='forbidden_reply_prefixes'?'configs/commentops-fast/common.json':'configs/commentops-fast/likeavto.json',field};
  const metadata=editorial?{category:'existing_editorial_guidance',examplesAreVerifiedFacts:false,grantsExecutionAuthority:false}
    :{category:'brand_policy',grantsExecutionAuthority:false,value:values,legacyProvenance:{...provenance},legacyScope:{companyKey:'likeavto'}};
  return version(field||marker.symbol,{title:'Imported rule evidence',text:editorial?'Mixed source text':JSON.stringify(values),
    trust:'imported_policy',scope:{account:'LikeAvto',postKeys:[]},hash:'c'.repeat(64),
    companyImport:{companyKey:'likeavto',scope:{companyKey:'likeavto'},importKey:field||marker.symbol,
      recordSha256:'a'.repeat(64),source:{origin:editorial?'commentops-fast.editorial-guidance':'commentops-fast.account-card',
        sha256:'b'.repeat(64),originalIds:provenance},metadata}});
}

test('known imported rules use verified semantic labels and values; forged provenance stays generic',()=>{
  const versions=[importedVersion({field:'forbidden_substrings'},['BAW']),
    importedVersion({field:'allowed_reply_urls'},['https://example.com']),
    importedVersion({field:'forbidden_reply_prefixes'},['Hello']),
    importedVersion({symbol:'REPLY_EDITING'}),importedVersion({symbol:'FACT_CHECKING'})];
  const forged=structuredClone(versions[0]);forged.id='forged';forged.entryId='forged';
  forged.companyImport.metadata.value=['changed'];versions.push(forged);
  const unknown=version('unknown',{title:'Imported rule evidence',scope:{account:'LikeAvto',postKeys:[]}});versions.push(unknown);
  const data=catalog(versions),before=JSON.stringify(data);
  const selected=activeInstructions(data,{account:'LikeAvto',now});
  assert.equal(JSON.stringify(data),before);
  const byId=new Map(selected.global.map(row=>[row.id,row]));
  for(const [id,label] of [
    ['forbidden_substrings','Запрещённые упоминания'],['allowed_reply_urls','Разрешённые ссылки'],
    ['forbidden_reply_prefixes','Запрещённые начала ответов'],
    ['REPLY_EDITING','Редактура ответов — смешанное руководство'],
    ['FACT_CHECKING','Проверка фактов — смешанное руководство']])
    assert.equal(byId.get(id).displaySemantics?.label,label);
  assert.deepEqual(byId.get('allowed_reply_urls').displaySemantics.constraint.values,['https://example.com']);
  assert.equal(byId.get('forged').displaySemantics,undefined);
  assert.equal(byId.get('unknown').displaySemantics,undefined);
  assert.equal(activeInstructions(data,{account:'BAW Russia',now}).global.length,0);
  assert.equal(displayInstructions(selected.global).length,7);
});

test('browser and adapter semantic validators are byte-identical',async()=>{
  const browser=await readFile(new URL('../workshop/assistant-rule-semantics.mjs',import.meta.url));
  const adapter=await readFile(new URL('../adapters/assistant-rule-semantics.mjs',import.meta.url));
  assert.deepEqual(browser,adapter);
});

test('history order uses event time rather than array insertion and keeps undated last',()=>{
  const rows=[{id:'new',createdAt:'2026-09-22T12:00:00Z'},{id:'unknown'},{id:'old',createdAt:'2026-08-01T12:00:00Z'}];
  assert.deepEqual(chronologicalHistory(rows).map(r=>r.id),['new','old','unknown']);
  assert.deepEqual(chronologicalHistory(rows,'oldest').map(r=>r.id),['old','new','unknown']);
  assert.equal(rows[0].id,'new');
});

test('bridge reads catalog separately, binds connector account/post and removes active claims on fetch failure',async t=>{
  const original=globalThis.fetch;t.after(()=>globalThis.fetch=original);
  let failed=false;
  globalThis.fetch=async path=>({ok:!(failed&&path==='/api/knowledge/instructions'),status:failed?503:200,json:async()=>path==='/api/bootstrap'?{account:'Label',connectorBinding:{accountId:'account'},posts:[{id:'p',postKey:'post-key'}]}:failed?{error:'Unavailable'}:catalog([version('global'),version('post',{scope:{account:'account',postKeys:['post-key']}})])});
  const connection=createMvpConnection({getSaved:()=>({overviewInstructions:{fake:{text:'Not active'}}})});
  await connection.load();await connection.loadInstructions({repaint:false});
  assert.equal(connection.instructionContext('p').status,'ready');
  assert.equal(connection.instructionContext('p').post[0].id,'post');
  failed=true;await connection.loadInstructions({repaint:false});
  assert.deepEqual(connection.instructionContext('p'),{status:'error',global:[],post:[]});
  connection.stop();
});

test('manual instruction save is explicit, retries the same request and confirms the persisted active head',async t=>{
  const originalFetch=globalThis.fetch,originalDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=originalFetch;globalThis.document=originalDocument;});
  const listeners={},notices=[],requests=[],button={disabled:false},result={textContent:''};
  const form={elements:{scope:{value:'post'},title:{value:'Tone',focus(){}},text:{value:'Be concise'}},reportValidity:()=>true,addEventListener:(name,callback)=>listeners[name]=callback,querySelector:()=>button};
  let closed=false,fail=true,persisted=null;
  const node={querySelector:selector=>selector==='[data-close]'?{addEventListener(){}}:selector==='#instruction-editor'?form:result,showModal(){},addEventListener(){},close(){closed=true;},remove(){}};
  globalThis.document={createElement:tag=>tag==='textarea'?{set innerHTML(value){this.value=value;}}:node,body:{append(){}}};
  globalThis.fetch=async(path,options={})=>{
    if(path==='/api/bootstrap')return {ok:true,json:async()=>({account:'account',posts:[{id:'p',postKey:'post-key',title:'Post'}]})};
    if(path==='/api/knowledge/instructions'&&options.method==='POST'){
      requests.push(JSON.parse(options.body));
      if(fail){fail=false;throw Error('Connection lost');}
      persisted=version('manual',{title:'Tone',text:'Be concise',scope:{account:'account',postKeys:['post-key']}});
      return {ok:true,json:async()=>({entry:{id:'manual'},version:persisted})};
    }
    assert.equal(path,'/api/knowledge/instructions');return {ok:true,json:async()=>catalog([persisted])};
  };
  const connection=createMvpConnection({getSaved:()=>({}),esc:String,icon:()=>'',announce:message=>notices.push(message)});
  await connection.load();connection.openInstructionEditor('p');
  assert.equal(requests.length,0,'opening the editor must not activate a rule');
  await listeners.submit({preventDefault(){}});
  assert.equal(closed,false);assert.match(result.textContent,/Connection lost/);
  await listeners.submit({preventDefault(){}});
  assert.equal(requests.length,2);assert.equal(requests[0].requestId,requests[1].requestId);
  assert.equal(requests[1].postKey,'post-key');assert.equal(closed,true);
  assert.equal(connection.instructionContext('p').post[0].text,'Be concise');
  assert.match(notices[0],/сохранено и действует/);connection.stop();
});


test('late rules response cannot replace the new operator catalog',async t=>{
  const oldFetch=globalThis.fetch,oldDocument=globalThis.document;
  t.after(()=>{globalThis.fetch=oldFetch;globalThis.document=oldDocument;});
  const saved={};const response=data=>({ok:true,json:async()=>data});
  const snapshot=operator=>({account:'account',operator:{id:operator},posts:[{id:'p',postKey:'post-key'}],items:[],branches:[],materials:[],proposals:[],conversations:[],jobs:[]});
  let resolveOld,started;const oldGate=new Promise(resolve=>{resolveOld=resolve;});const firstStarted=new Promise(resolve=>{started=resolve;});let requests=0;
  globalThis.document={querySelector:()=>null};
  globalThis.fetch=async path=>{
    if(path==='/api/bootstrap')return response(snapshot('first'));
    assert.equal(path,'/api/knowledge/instructions');
    if(++requests===1){started();return oldGate;}
    return response(catalog([version('new',{title:'New operator rules'})]));
  };
  const connection=createMvpConnection({getSaved:()=>saved,esc:String,icon:()=>''});
  await connection.load();const oldRequest=connection.loadInstructions({repaint:false});await firstStarted;
  connection.hydrate(snapshot('second'),{repaint:false});
  await connection.loadInstructions({repaint:false});
  resolveOld(response(catalog([version('old',{title:'Stale operator rules'})])));await oldRequest;
  assert.equal(connection.instructionContext('p').global[0].title,'New operator rules');connection.stop();
});

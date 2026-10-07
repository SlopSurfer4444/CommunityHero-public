import test from 'node:test';
import assert from 'node:assert/strict';
import {accountNavigation,accountStorageKey,loadAccountState,readAccountNavigation,bindAccountNavigation,bindAccountPageRestore} from '../workshop/account-navigation.js';
import {createMvpConnection} from '../workshop/mvp-connection.js';
import {workspaceBasePath,workspacePath} from '../workshop/workspace-path.js';
import {createWorkspaceGenerationFence} from '../workshop/workspace-generation.js';

const origin = 'https://communityhero.ru/likeavto/';
const payload = () => ({account:'LikeAvto',accounts:[
  {id:'likeavto',label:'LikeAvto',url:origin},
  {id:'baw-russia',label:'BAW Russia',url:'https://communityhero.ru/baw/'}
]});
const storage = () => {const values = new Map(); return {getItem:key=>values.get(key)??null,setItem:(key,value)=>values.set(key,value)};};

test('navigation accepts configured companies without a fixed company list',()=>{
  const data=payload(); data.accounts.push({id:'third-company',label:'Third Company',url:'https://communityhero.ru/third-company/'});
  assert.equal(accountNavigation(data,origin).accounts.length,3);
  assert.equal(accountNavigation({account:'Local Company',accounts:[]},'http://127.0.0.1:4199/').account,'Local Company');
});

test('same-origin configured company paths resolve independently without carrying selected context',async()=>{
  const data={account:'LikeAvto',accounts:[{id:'likeavto',label:'LikeAvto',url:'https://communityhero.ru/likeavto/'},
    {id:'baw-russia',label:'BAW Russia',url:'https://communityhero.ru/baw/'}]};
  const current='https://communityhero.ru/likeavto/#item/old-comment';
  assert.equal(accountNavigation(data,current).accounts[1].url,'https://communityhero.ru/baw/');
  let requested;
  await readAccountNavigation(async path=>{requested=path;return {ok:true,json:async()=>data};},current);
  assert.equal(requested,'/likeavto/api/accounts');
  assert.throws(()=>accountNavigation({...data,basePath:'/baw/'},current));
});

test('workspace request prefix is mechanical, strict and independent of company identifiers',()=>{
  assert.equal(workspacePath('/api/bootstrap','/baw/'),'/baw/api/bootstrap');
  assert.equal(workspacePath('/api/items/a?mode=open','/likeavto/'),'/likeavto/api/items/a?mode=open');
  assert.equal(workspacePath('/icons.json','/third-company/'),'/third-company/icons.json');
  assert.equal(workspacePath('/api/session','/'),'/api/session');
  for(const path of ['/baw','/baw/item/old','//baw/','/../','/%62aw/']) assert.throws(()=>workspaceBasePath(path));
  assert.throws(()=>workspacePath('//foreign.invalid/api','/baw/'));
  assert.throws(()=>workspacePath('/../likeavto/api','/baw/'));
});

test('destinations cannot carry a comment, credentials, query or another company on current origin',()=>{
  for (const url of ['javascript:alert(1)','https://user:secret@baw.communityhero.ru/','https://baw.communityhero.ru/item/old',
    'https://baw.communityhero.ru/?item=old','https://baw.communityhero.ru/#item/old',origin,'http://baw.communityhero.ru/']) {
    const data=payload();data.accounts[1].url=url;
    assert.throws(()=>accountNavigation(data,origin),Error,url);
  }
  for (const change of [data=>data.accounts[1].id='likeavto',data=>data.accounts[1].id='../company',
    data=>data.accounts[0].url='https://wrong.communityhero.ru/']) {
    const data=payload();change(data);assert.throws(()=>accountNavigation(data,origin));
  }
});

test('configured IDs and paths match backend character and length bounds',()=>{
  for(const id of ['THIRD_Company','_', '-','x'.repeat(64)]) {
    const data=payload();data.accounts[1].id=id;
    data.accounts[1].url='https://communityhero.ru/third--company/';
    assert.equal(accountNavigation(data,origin).accounts[1].id,id);
  }
  for(const id of ['', 'x'.repeat(65),'русский','with.space','two words']) {
    const data=payload();data.accounts[1].id=id;assert.throws(()=>accountNavigation(data,origin));
  }
  for(const path of ['/a/','/a--b/',`/${'a'.repeat(64)}/`]) assert.equal(workspaceBasePath(path),path);
  for(const path of [`/${'a'.repeat(65)}/`,'/Upper/','/under_score/','/-leading/','/trailing-/','/--/']) assert.throws(()=>workspaceBasePath(path));
});

test('presentation labels do not select company authority and normalized unsafe URLs are rejected',()=>{
  const data=payload();data.accounts[0].label='Лайкавто';
  assert.equal(accountNavigation(data,origin).account,'LikeAvto');
  for(const url of ['https://communityhero.ru/baw/../likeavto/','https://communityhero.ru/baw/?','https://communityhero.ru/baw/#']) {
    const unsafe=payload();unsafe.accounts[1].url=url;assert.throws(()=>accountNavigation(unsafe,origin));
  }
});

test('HTTP is restricted to loopback development and cannot downgrade a public site',()=>{
  const data={account:'Local A',accounts:[{id:'a',label:'Local A',url:'http://127.0.0.1:4199/a/'},{id:'b',label:'Local B',url:'http://127.0.0.1:4199/b/'}]};
  assert.equal(accountNavigation(data,'http://127.0.0.1:4199/a/').accounts.length,2);
  assert.throws(()=>accountNavigation(data,origin));
});

test('public discovery is same-origin and never sends account/session tokens',async()=>{
  let observed;
  await readAccountNavigation(async(...args)=>{observed=args;return {ok:true,json:async()=>payload()};},origin);
  assert.deepEqual(observed,['/likeavto/api/accounts',{credentials:'same-origin',cache:'no-store'}]);
});

test('drafts, selection and assistant state are scoped to both company and operator',()=>{
  const local=storage();
  const first=loadAccountState(local,'LikeAvto','alice');
  Object.assign(first.saved,{selected:'comment-a',mvpAiInput:'draft assistant text',items:{a:{draft:'unsent reply'}}});
  local.setItem(first.key,JSON.stringify(first.saved));
  assert.deepEqual(loadAccountState(local,'BAW Russia','alice').saved,{mvpAccount:'BAW Russia'});
  assert.deepEqual(loadAccountState(local,'LikeAvto','bob').saved,{mvpAccount:'LikeAvto'});
  assert.deepEqual(loadAccountState(local,'LikeAvto','alice').saved,first.saved);
  local.setItem(accountStorageKey('BAW Russia','alice'),JSON.stringify(first.saved));
  assert.deepEqual(loadAccountState(local,'BAW Russia','alice').saved,{mvpAccount:'BAW Russia'});
});

test('untagged legacy state remains intact and cannot leak into either company regardless of visit order',()=>{
  for(const accounts of [['LikeAvto','BAW Russia'],['BAW Russia','LikeAvto']]) {
    const local=storage(),legacy='communityhero-operator-alice-v1';
    const original=JSON.stringify({selected:'old',items:{old:{draft:'keep me'}},mvpAiInput:'private discussion',
      mvpConversationId:'old-chat',mvpPendingProposalBatches:{one:{kind:'close',itemIds:['old']}},mvpFeedbackOutbox:[{eventId:'old-operation'}]});
    local.setItem(legacy,original);
    for(const account of accounts)assert.deepEqual(loadAccountState(local,account,'alice').saved,{mvpAccount:account});
    assert.equal(local.getItem(legacy),original);
  }
});

test('legacy migration requires both an explicit company tag and verified operator identity',()=>{
  for(const tags of [{},{mvpAccount:'LikeAvto'},{mvpActorId:'alice'},{mvpAccount:'LikeAvto',mvpActorId:'bob'},
    {mvpAccount:'BAW Russia',mvpActorId:'alice'},{mvpAccount:'LikeAvto',mvpActorId:'alice'}]) {
    const local=storage(),legacy='communityhero-operator-alice-v1';
    const original=JSON.stringify({...tags,selected:'old',items:{old:{draft:'keep me'}},mvpAiInput:'private discussion'});
    local.setItem(legacy,original);
    const loaded=loadAccountState(local,'LikeAvto','alice');
    if(tags.mvpAccount==='LikeAvto'&&tags.mvpActorId==='alice')assert.equal(loaded.saved.items.old.draft,'keep me');
    else assert.deepEqual(loaded.saved,{mvpAccount:'LikeAvto'});
    assert.equal(local.getItem(legacy),original);
  }
});

test('malformed persisted state cannot become a company workspace',()=>{
  for(const value of ['[]','42','"text"','{broken']) {
    const local=storage();local.setItem(accountStorageKey('LikeAvto','alice'),value);
    assert.deepEqual(loadAccountState(local,'LikeAvto','alice').saved,{mvpAccount:'LikeAvto'});
  }
});

test('actual switch control invalidates current work before a clean full-page navigation',t=>{
  const original=globalThis.location,events=[];
  globalThis.location={origin:new URL(origin).origin,pathname:'/likeavto/',assign:url=>events.push(['navigate',url])};
  t.after(()=>{globalThis.location=original;});
  class Element {
    constructor(){this.children=[];this.listeners={};this.value='';}
    append(child){this.children.push(child);if(child.selected)this.value=child.value;}
    setAttribute(){}
    addEventListener(name,callback){this.listeners[name]=callback;}
  }
  const header=new Element();header.ownerDocument={createElement:()=>new Element()};
  const control=bindAccountNavigation(header,accountNavigation(payload(),origin),destination=>events.push(['invalidate',destination.id]));
  const select=control.children[1];select.value='baw-russia';select.listeners.change();
  assert.deepEqual(events,[['invalidate','baw-russia'],['navigate','https://communityhero.ru/baw/']]);
  assert.equal(select.disabled,true);
});

test('early login/bootstrap switching reloads a restored page even before connection exists',()=>{
  for(const connection of [undefined,{start:()=>assert.fail('invalidated connection restarted')}]) {
    const handlers={},events=[];
    bindAccountPageRestore({addEventListener:(name,callback)=>handlers[name]=callback},
      {isLeaving:()=>true,getConnection:()=>connection,reload:()=>events.push('reload')});
    handlers.pageshow({persisted:true});
    assert.deepEqual(events,['reload']);
    handlers.pageshow({persisted:false});
    assert.deepEqual(events,['reload']);
  }
});

test('normal restored workspace resumes refresh while unfinished login remains usable',async()=>{
  const handlers={},events=[];let connection;
  bindAccountPageRestore({addEventListener:(name,callback)=>handlers[name]=callback},
    {isLeaving:()=>false,getConnection:()=>connection,reload:()=>assert.fail('normal page reload')});
  handlers.pageshow({persisted:true});assert.deepEqual(events,[]);
  connection={start:()=>events.push('start'),refresh:async options=>events.push(options)};
  handlers.pageshow({persisted:true});
  assert.deepEqual(events,['start',{background:true}]);
});

function connectionFixture(t,response,operator) {
  const original={fetch:globalThis.fetch,document:globalThis.document};
  globalThis.document={querySelector:()=>null,removeEventListener:()=>{}};
  let release,calls=0;
  globalThis.fetch=async()=>{calls++;await new Promise(resolve=>release=resolve);return {ok:true,json:async()=>response};};
  const saved={items:{a:{draft:'retained local draft'}}};
  const connection=createMvpConnection({account:'LikeAvto',operator,getSaved:()=>saved,getData:()=>({items:[]})});
  t.after(()=>{connection.stop();Object.assign(globalThis,original);});
  return {connection,saved,release:()=>release(),calls:()=>calls};
}

test('switching rejects delayed bootstrap and all later API dispatches while preserving drafts',async t=>{
  const fixture=connectionFixture(t,{account:'LikeAvto',items:[],posts:[],branches:[]});
  const loading=fixture.connection.load();fixture.connection.leaveAccount();fixture.release();
  await assert.rejects(loading,/Пользователь изменился/);
  await assert.rejects(fixture.connection.load(),/перезагружается/);
  assert.equal(fixture.calls(),1);
  assert.equal(fixture.saved.items.a.draft,'retained local draft');
});

test('a bootstrap from another company is rejected before saved state is touched',async t=>{
  const fixture=connectionFixture(t,{account:'BAW Russia',items:[{id:'foreign'}],posts:[],branches:[]});
  const loading=fixture.connection.load();fixture.release();
  await assert.rejects(loading,/Аккаунт рабочего места изменился/);
  assert.deepEqual(Object.keys(fixture.saved.items),['a']);
  await assert.rejects(fixture.connection.load(),/перезагружается/);
  assert.equal(fixture.calls(),1);
});

test('engine requests keep the original prefix if browser pathname changes before unload',async t=>{
  const original=globalThis.location;
  globalThis.location={pathname:'/likeavto/'};
  t.after(()=>{globalThis.location=original;});
  const fixture=connectionFixture(t,{account:'LikeAvto',items:[],posts:[],branches:[]});
  const paths=[];
  globalThis.fetch=async path=>{paths.push(path);return {ok:true,json:async()=>({account:'LikeAvto',items:[],posts:[],branches:[]})};};
  globalThis.location.pathname='/baw/';
  await fixture.connection.load();
  assert.deepEqual(paths,['/likeavto/api/bootstrap']);
});

const firstGeneration='e4e1d9f2-49b8-4b48-846f-ab113f519f89';
const secondGeneration='7b02e571-572b-4b41-87a6-40d2c2a36bc3';

test('a pristine database never imports old company drafts, selections or pending approval outboxes',()=>{
  const local=storage();const legacy=loadAccountState(local,'LikeAvto','alice');
  Object.assign(legacy.saved,{selected:'same-item',items:{'same-item':{draft:'old unsent reply'}},mvpPendingApprovals:{same:{body:{requestId:'old'}}}});
  local.setItem(legacy.key,JSON.stringify(legacy.saved));
  const first=loadAccountState(local,'LikeAvto','alice',firstGeneration);
  assert.deepEqual(first.saved,{mvpAccount:'LikeAvto',mvpWorkspaceGeneration:firstGeneration});
  Object.assign(first.saved,{items:{'same-item':{draft:'current reply'}},selected:'same-item'});local.setItem(first.key,JSON.stringify(first.saved));
  const second=loadAccountState(local,'LikeAvto','alice',secondGeneration);
  assert.deepEqual(second.saved,{mvpAccount:'LikeAvto',mvpWorkspaceGeneration:secondGeneration});
  assert.equal(loadAccountState(local,'LikeAvto','alice',firstGeneration).saved.items['same-item'].draft,'current reply');
  assert.equal(loadAccountState(local,'LikeAvto','alice').saved.items['same-item'].draft,'old unsent reply');
  local.setItem(second.key,JSON.stringify(first.saved));
  assert.deepEqual(loadAccountState(local,'LikeAvto','alice',secondGeneration).saved,{mvpAccount:'LikeAvto',mvpWorkspaceGeneration:secondGeneration});
});

test('browser generation pins once, keeps exact header and cannot turn a legacy session into a new episode',()=>{
  const fence=createWorkspaceGenerationFence();fence.observe({storageGeneration:firstGeneration},null,{initial:true});
  assert.deepEqual(fence.headers(),{'x-communityhero-workspace-generation':firstGeneration});
  assert.throws(()=>fence.observe({storageGeneration:secondGeneration}),{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.equal(fence.generation,firstGeneration);
  const legacy=createWorkspaceGenerationFence(null);
  assert.throws(()=>legacy.observe({storageGeneration:firstGeneration}),{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.equal(legacy.generation,null);assert.deepEqual(legacy.headers(),{});
});

test('a delayed bootstrap from a new database is rejected before any same-ID draft is merged or replayed',async t=>{
  const fixture=connectionFixture(t,{account:'LikeAvto',storageGeneration:secondGeneration,items:[{id:'a',revision:1}],posts:[],branches:[]},
    {id:'alice',storageGeneration:firstGeneration});
  const loading=fixture.connection.load();fixture.release();
  await assert.rejects(loading,{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.equal(fixture.saved.items.a.draft,'retained local draft');
  await assert.rejects(fixture.connection.load());assert.equal(fixture.calls(),1);
});

test('browser reads carry the original generation and compare the native response header',async t=>{
  const fixture=connectionFixture(t,{account:'LikeAvto',items:[],posts:[],branches:[]},{id:'alice',storageGeneration:firstGeneration});
  const calls=[];globalThis.fetch=async(path,init)=>{calls.push({path,init});return {ok:true,headers:{get:()=>secondGeneration},json:async()=>({account:'LikeAvto',items:[],posts:[],branches:[]})};};
  await assert.rejects(fixture.connection.load(),{code:'WORKSPACE_GENERATION_MISMATCH'});
  assert.equal(calls[0].init.headers['x-communityhero-workspace-generation'],firstGeneration);assert.equal(calls.length,1);
});

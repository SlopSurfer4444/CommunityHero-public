import test from 'node:test';
import assert from 'node:assert/strict';
import {deriveClosure} from '../workshop/closure-outcomes.js';
import {mapRead,projectContext} from '../adapters/provider.mjs';
import {createMvpConnection} from '../workshop/mvp-connection.js';

const item = {id:'item',workflow:'closed',branchId:'branch',postId:'post',targetId:'target',objectId:'account',providerItemId:'provider-target',createdAt:'2026-09-21T12:00:00Z'};
const target = {id:'target',role:'customer',providerItemId:'provider-target',providerObjectId:'account'};
const brand = {id:'reply',role:'brand',parentId:'target',providerItemId:'provider-reply',providerObjectId:'account',replyToProviderItemId:'provider-target'};
const branch = (messages=[target],extra={}) => ({id:'branch',postId:'post',contextComplete:false,messages,...extra});
const outcome = (messages,extra={}) => deriveClosure(item,[branch(messages,extra)]).outcome;

test('direct provider brand reply establishes reply despite incomplete branch',()=>{
  const result=deriveClosure(item,[branch([target,brand])]);
  assert.equal(result.outcome,'reply');assert.equal(result.replyId,'reply');assert.equal(result.at,null);
});
test('sibling, ancestor, nested brand and mention text never establish direct reply',()=>{
  for(const reply of [
    {...brand,parentId:'other',replyToProviderItemId:'other'},
    {...brand,id:'parent',parentId:null,replyToProviderItemId:null},
    {...brand,parentId:'another-brand',replyToProviderItemId:'another-brand'},
    {...brand,parentId:null,replyToProviderItemId:null,text:'@target Here is your answer'}
  ]) assert.equal(outcome([target,reply]),'unknown');
});
test('explicit local parent works but contradictory provider edge cannot be overridden',()=>{
  assert.equal(outcome([target,{...brand,replyToProviderItemId:null}]),'reply');
  assert.equal(outcome([target,{...brand,replyToProviderItemId:'other'}]),'unknown');
  assert.equal(outcome([target,{...brand,parentId:'other'}]),'unknown');
  assert.equal(outcome([target,{...brand,parentId:null}]),'reply');
});
test('cross-object messages, unavailable replies and non-brand authors do not prove reply',()=>{
  for(const extra of [{providerObjectId:'other'},{role:'customer'},{deleted:true},{unavailable:true}])
    assert.equal(outcome([target,{...brand,...extra}]),'unknown');
});
test('no_reply requires explicit intact branch completeness, never queue completeness',()=>{
  assert.equal(outcome([target]),'unknown');
  assert.equal(outcome([target],{coverage:{complete:true}}),'unknown');
  assert.equal(outcome([target],{contextComplete:true}),'no_reply');
  for(const extra of [{contextTruncated:true},{missingParentIds:['missing']},{unavailableReason:'Partial'},{knownMessageCount:2}])
    assert.equal(outcome([target],{contextComplete:true,...extra}),'unknown');
});
test('ambiguous brand parent and unavailable messages prevent negative inference',()=>{
  for(const message of [
    {...brand,parentId:null,replyToProviderItemId:null},
    {...brand,parentId:'missing',replyToProviderItemId:null},
    {id:'missing',unavailable:true},
    {...target,id:'deleted',deleted:true},
    {...target,id:'foreign',providerObjectId:'other'}
  ]) assert.equal(outcome([target,message],{contextComplete:true}),'unknown');
});
test('complete observed branch with a reply to another target can prove no direct reply',()=>{
  const sibling={id:'sibling',providerItemId:'sibling'};
  assert.equal(outcome([target,sibling,{...brand,parentId:'sibling',replyToProviderItemId:'sibling'}],{contextComplete:true}),'no_reply');
});
test('missing, misbound and duplicate targets or branches remain unknown',()=>{
  for(const branches of [[],[branch([], {contextComplete:true})],[branch([target,brand],{postId:'other'})],
    [branch([target,target,brand])],[branch([target,brand]),branch([target,brand])],
    [branch([target,{...brand,providerItemId:'provider-target'}])],
    [branch([{...target,providerItemId:'other'},brand])]]) assert.equal(deriveClosure(item,branches).outcome,'unknown');
});
test('successful operations take precedence; failed and unrecognized operations cannot classify',()=>{
  const op={id:'op',itemId:'item',status:'succeeded',action:{action:'close'},updatedAt:'2026-09-22T12:00:00Z'};
  assert.equal(deriveClosure(item,[branch([target,brand])],[op]).outcome,'no_reply');
  assert.equal(deriveClosure(item,[],[{...op,action:{action:'reply_and_close'}}]).outcome,'reply');
  assert.equal(deriveClosure(item,[],[op]).at,op.updatedAt);
  for(const extra of [{status:'unknown'},{status:'failed'},{itemId:'other'},{action:{action:'delete'}}])
    assert.equal(deriveClosure(item,[],[{...op,...extra}]).outcome,'unknown');
});
test('open items do not inherit historic closures',()=>assert.equal(deriveClosure({...item,workflow:'attention'},[branch([target,brand])]),null));
test('actual provider projection classifies only its explicit official reply recipient',()=>{
  const helpers={fastCommentAttachments:x=>x||[],fastConveyorPublicSourceUrl:()=>'',fastConveyorAuthorId:()=>null,computeThreadContextEvidenceDigest:()=> 'digest'};
  const source={item:{id:'target',status:'closed',text:'Hello'},parent:null,replyTo:null,officialReplies:[]};
  for(const [replies,expected] of [[[],'unknown'],[[{id:'r',text:'@target reply'}],'unknown'],[[{id:'r',reply_to_item_id:'target'}],'reply'],[[{id:'r',reply_to_item_id:'sibling'}],'unknown']]){
    const snapshot=mapRead([projectContext({...source,officialReplies:replies},'account',helpers)],{coverage:{complete:true}});
    assert.equal(deriveClosure(snapshot.items[0],snapshot.branches).outcome,expected);
  }
});

test('connection refresh derives imported closure and admits authoritative operation updates',async t=>{
  const original={fetch:globalThis.fetch,document:globalThis.document};
  const data={items:[]},saved={items:{}};
  const snapshot={items:[item],branches:[branch([target,brand])],posts:[],operations:[]};
  globalThis.document={activeElement:null};
  globalThis.fetch=async()=>({ok:true,json:async()=>structuredClone(snapshot)});
  const connection=createMvpConnection({getData:()=>data,getSaved:()=>saved,stateFor:row=>saved.items[row.id]??=structuredClone(row.initialState)});
  t.after(()=>{connection.stop();for(const [key,value] of Object.entries(original)){if(value===undefined)delete globalThis[key];else globalThis[key]=value;}});
  await connection.refresh();assert.equal(saved.items.item.closure.outcome,'reply');
  snapshot.branches[0].messages=[target];
  await connection.refresh();assert.equal(saved.items.item.closure.outcome,'unknown');
  snapshot.operations=[{itemId:'item',status:'succeeded',action:{action:'close'}}];
  await connection.refresh();assert.equal(saved.items.item.closure.outcome,'no_reply');
});

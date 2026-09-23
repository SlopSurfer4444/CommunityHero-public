import test from 'node:test';
import assert from 'node:assert/strict';
import {providerOfficial} from '../adapters/provider-message-role.mjs';
import {projectContext,mapRead} from '../adapters/provider.mjs';
const helpers={fastCommentAttachments:()=>[],fastConveyorPublicSourceUrl:()=>null,fastConveyorAuthorId:()=>null,computeThreadContextEvidenceDigest:()=> 'a'.repeat(64)};
test('brand role follows explicit official evidence for parent and target, never display name',()=>{
  const actor=(id,official)=>({id,official,author:{name:'LikeAvto — Авто из Китая'},text:'reply'});
  for(const [flag,expected] of [[1,'brand'],[true,'brand'],[0,'participant'],[false,'participant'],[undefined,'participant'],['1','participant']]){
    const c={item:actor('target',flag),replyTo:actor('parent',flag),parent:{id:'post'},officialReplies:[actor('official',undefined)]};
    const messages=mapRead([projectContext(c,'11341',helpers)],{}).branches[0].messages;
    assert.equal(messages[0].role,expected);
    assert.equal(messages[1].role,expected==='brand'?'brand':'customer');
    assert.equal(messages[0].providerOfficial,providerOfficial(flag));
    assert.equal(messages[2].role,'brand');
    assert.equal(messages[2].roleEvidence,'provider-official-replies');
  }
});

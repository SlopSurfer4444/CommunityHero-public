import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantRuntimePaths,assistantInstructions,generationMetadata,prepareAssistantRequest,reviewInstructions} from './assistant.mjs';
import {researchInstructions,researchMetadata} from './assistant-research.mjs';

test('BAW assistant payload, drafting prompt and research remain account-bound',()=>{
  const prepared=prepareAssistantRequest({account:'baw-russia',items:[{id:'i',postId:'p'}],posts:[{id:'p',text:'Post'}]});
  assert.deepEqual(prepared.payload.account,{accountKey:'baw-russia',providerAccountId:'baw-russia',displayName:'BAW Russia'});
  assert.equal(prepared.payload.items[0].postId,'p');
  for(const text of [prepared.input,assistantInstructions(true,'baw-russia'),reviewInstructions('baw-russia'),researchInstructions('baw-russia')]){
    assert.match(text,/BAW Russia/);assert.doesNotMatch(text,/LikeAvto/);
  }
  assert.throws(()=>prepareAssistantRequest({account:'foreign',items:[]}),{code:'ACCOUNT_NOT_ALLOWED'});
  assert.throws(()=>prepareAssistantRequest({account:'baw-russia',items:[{id:'i'}],materials:[{id:'m',account:'LikeAvto'}]}),{code:'ASSISTANT_INVALID_REQUEST'});
  assert.throws(()=>prepareAssistantRequest({account:'baw-russia',items:[{id:'i'}],customerCases:[{itemId:'i',accountId:'LikeAvto'}]}),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('generation and research evidence hashes bind the selected account instructions',()=>{
  const like=generationMetadata('same',true,0,'likeavto'),baw=generationMetadata('same',true,0,'baw-russia');
  assert.equal(like.promptVersion,'communityhero-drafting-v14-imported-rule-semantics');
  assert.notEqual(like.instructionSha256,baw.instructionSha256);
  assert.notEqual(researchMetadata('same','no_sources',[],{},0,'likeavto').instructionSha256,researchMetadata('same','no_sources',[],{},0,'baw-russia').instructionSha256);
});

test('portable assistant requires explicit absolute runtime and data paths without provider setup',()=>{
  assert.throws(()=>assistantRuntimePaths({COMMUNITYHERO_RUNTIME_MODE:'portable'}),{code:'ASSISTANT_UNAVAILABLE'});
  assert.throws(()=>assistantRuntimePaths({COMMUNITYHERO_CODEX_CLI:'relative.exe'}),{code:'ASSISTANT_UNAVAILABLE'});
  const paths=assistantRuntimePaths({COMMUNITYHERO_RUNTIME_MODE:'portable',COMMUNITYHERO_CODEX_CLI:'C:/fixture/codex.exe',COMMUNITYHERO_ASSISTANT_DATA_DIR:'C:/fixture/assistant'});
  assert.match(paths.cli,/fixture[\\/]codex\.exe$/);
  assert.match(paths.base,/fixture[\\/]assistant$/);
});

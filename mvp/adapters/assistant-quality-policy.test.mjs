import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,reviewInstructions,prepareAssistantRequest,generationMetadata} from './assistant.mjs';

test('untitled same-caption posts retain distinct source bindings through adapter projection',()=>{
  const request={items:[{id:'selected',postId:'p2',postKey:'yt:two',branchId:'b'}],posts:[
    {id:'p1',title:'',text:'Same caption',postKey:'vk:one',sourceUrl:'https://vk.com/wall1_1',channel:'VK',accessToken:'private'},
    {id:'p2',title:'',text:'Same caption',postKey:'yt:two',sourceUrl:'https://youtube.com/watch?v=two',channel:'YouTube'}],
    branches:[{id:'b',postId:'p2',messages:[]}],materials:[{id:'t',kind:'transcript',postKey:'vk:one',text:'Source words',transcription:{sourcePostKey:'vk:one',partial:true}}]};
  const {payload,input}=prepareAssistantRequest(request);
  assert.equal(payload.items[0].postId,'p2');
  assert.equal(payload.items[0].postKey,'yt:two');
  assert.equal(payload.items[0].branchId,'b');
  assert.equal(payload.branches[0].postId,'p2');
  assert.deepEqual(payload.posts.map(p=>[p.id,p.postKey,p.sourceUrl,p.channel]),[
    ['p1','vk:one','https://vk.com/wall1_1','VK'],['p2','yt:two','https://youtube.com/watch?v=two','YouTube']]);
  assert.equal(payload.materials[0].transcription.sourcePostKey,'vk:one');
  assert.equal(input.includes('private'),false);
  // Preserving identifiers is not an invented assertion of equivalent video content.
  assert.equal(payload.materials[0].postKey,'vk:one');
  const altered=structuredClone(request);altered.posts[1].sourceUrl='https://youtube.com/watch?v=other';
  assert.notEqual(generationMetadata(input,true).inputSha256,generationMetadata(prepareAssistantRequest(altered).input,true).inputSha256);
});

test('both preparation stages receive exact-point constraints and versioned provenance',()=>{
  for(const account of ['likeavto','baw-russia'])for(const instructions of [assistantInstructions(true,account),reviewInstructions(account)]){
    assert.match(instructions,/Do not replace an additive\s+comparison with a substitution/);
    assert.match(instructions,/nearby version does not\s+resolve a correction/);
    assert.match(instructions,/not already answered by supplied context/);
    assert.match(instructions,/Absence of a question alone is not a\s+reason to close/);
  }
  assert.equal(generationMetadata('input',true).promptVersion,'communityhero-drafting-v19-intent-scoped-evidence');
});

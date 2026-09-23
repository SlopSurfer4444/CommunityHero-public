import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantDraftCandidates,candidateDraftPatch} from '../workshop/assistant-candidates.js';
const item={id:'a',revision:2,workflow:'prepared',draft:'Manual draft',contextEvidenceDigest:'c',branchContextDigest:'b'};
const candidate={id:'proposal',revision:1,itemId:'a',itemRevision:2,prepareRunId:'run',status:'draft',kind:'reply_and_close',text:'New candidate',contextEvidenceDigest:'c',branchContextDigest:'b'};
test('candidate panel excludes other conversations, screens and changed recipients',()=>{
  const chat={messages:[{role:'assistant',prepareRunId:'run'}]};
  const snapshot={items:[item],proposals:[{...candidate,prepareRunId:'other'},candidate,{...candidate,id:'stale',itemRevision:1}]};
  assert.deepEqual(assistantDraftCandidates(snapshot,chat,{itemIds:['a']}),[candidate]);
  assert.deepEqual(assistantDraftCandidates(snapshot,chat,{itemIds:['b']}),[]);
  assert.deepEqual(assistantDraftCandidates(snapshot,{messages:[]},{itemIds:['a']}),[]);
});
test('apply binds exact proposal and current item revision without any publication field',()=>{
  const body=candidateDraftPatch(candidate,item,{eventId:'event',draftSessionId:'draft',sessionId:'session'});
  assert.equal(body.expectedRevision,2);assert.equal(body.draft,'New candidate');assert.equal(body.sourceProposalId,'proposal');
  assert.equal(body.workflow,undefined);assert.equal(body.approved,undefined);assert.equal(item.draft,'Manual draft');
  for(const changed of [{...item,revision:3},{...item,id:'b'},{...item,workflow:'closed'}])assert.throws(()=>candidateDraftPatch(candidate,changed,{eventId:'e',draftSessionId:'d'}));
});

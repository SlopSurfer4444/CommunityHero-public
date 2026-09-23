import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest} from './assistant.mjs';

test('company case evidence preserves legacy provenance without inventing provider identities',()=>{
  const evidence={text:'Historical statement',sourceRecordKey:'key',sourceRecordSha256:'a'.repeat(64),knowledgeEntryId:'entry',knowledgeVersionId:'version',sourceType:'legacy_company_import',sourceCreatedAt:'2026-09-01 09:00:00',claimType:'published_brand_statement',publicationStatus:'verified',legacyAuthorAlias:{namespace:'commentops-fast.author-id',platform:'vk',value:'legacy-author',secret:'drop'},secret:'drop'};
  const req={account:'likeavto',items:[{id:'selected'}],customerCases:[{itemId:'selected',accountId:'LikeAvto',messages:[],brandReplies:[evidence],priorContractRequests:[evidence]}]};
  const {payload,input}=prepareAssistantRequest(req);
  for(const value of [payload.customerCases[0].brandReplies[0],payload.customerCases[0].priorContractRequests[0]]){
    assert.equal(value.sourceRecordKey,'key');assert.equal(value.sourceCreatedAt,'2026-09-01 09:00:00');assert.equal(value.sourceType,'legacy_company_import');
    assert.equal(value.legacyAuthorAlias.namespace,'commentops-fast.author-id');assert.equal(value.providerItemId,undefined);assert.equal(value.providerObjectId,undefined);
  }
  assert.equal(input.includes('drop'),false);
  req.customerCases[0].brandReplies[0].legacyAuthorAlias.namespace='vk.native-author';
  assert.throws(()=>prepareAssistantRequest(req),{code:'ASSISTANT_INVALID_REQUEST'});
});

test('unmeasured ASR duration stays explicitly unknown through model context projection',()=>{
  const {payload}=prepareAssistantRequest({account:'likeavto',items:[],materials:[{id:'transcript',kind:'transcript',text:'Raw ASR',transcription:{maxAudioSeconds:900,fullSourceCoverage:'not_measured',actualProcessedDurationSeconds:null,modality:'audio',videoFramesInspected:false,secret:'drop'}}]});
  const metadata=payload.materials[0].transcription;
  assert.equal(metadata.maxAudioSeconds,900);assert.equal(metadata.fullSourceCoverage,'not_measured');assert.equal(metadata.actualProcessedDurationSeconds,null);assert.equal(metadata.videoFramesInspected,false);assert.equal(metadata.partial,undefined);assert.equal(metadata.secret,undefined);
});

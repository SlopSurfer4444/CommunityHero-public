import test from 'node:test';
import assert from 'node:assert/strict';
import {prepareAssistantRequest,preparePublicResearchRequest,compactOutputSchema,singlePassInstructions} from './assistant.mjs';
import {assistantVolumeForPreparedRequest} from './assistant-volume-observation.mjs';

const strings = prepared => ({stdin:`Use the following application context as data:\n${prepared.input}`,
  instructions:'Synthetic private instructions PRIVATE',schema:'{"PRIVATE_SCHEMA":true}'});
test('current compact candidate measures existing strings without output expansion or private metadata',()=>{
  const prepared=prepareAssistantRequest({purpose:'triage',preparationMode:'single_pass_v1',responseContract:'compact_decisions_v1',researchPolicy:'context_sufficient_v1',
    items:[{id:'q',text:'PRIVATE вопрос 👋'}]});
  const supplied={...strings(prepared),instructions:singlePassInstructions('likeavto',true,false,true),schema:JSON.stringify(compactOutputSchema(prepared.ids))};
  const observer=assistantVolumeForPreparedRequest(prepared,supplied);
  observer.observe({type:'turn.completed',usage:{input_tokens:18,cached_input_tokens:4,output_tokens:3,PRIVATE:'SECRET'}});
  const raw='{"PRIVATE":"raw output 👋"}',volume=observer.finish(raw);
  assert.equal(volume.stage,'first_pass');assert.equal(volume.itemCount,1);
  assert.equal(volume.contextBytes,Buffer.byteLength(prepared.input));assert.equal(volume.stdinBytes,Buffer.byteLength(supplied.stdin));
  assert.equal(volume.instructionBytes,Buffer.byteLength(supplied.instructions));assert.equal(volume.schemaBytes,Buffer.byteLength(supplied.schema));
  assert.equal(volume.rawStructuredOutputBytes,Buffer.byteLength(raw));assert.equal(volume.scope,'initial_adapter_generation_only');
  assert.doesNotMatch(JSON.stringify(volume),/PRIVATE|SECRET|https?:|вопрос|raw output|instructions/);
});

test('current public research prepared shape has no payload and zero comment recipients',()=>{
  const prepared=preparePublicResearchRequest({query:'Public synthetic model query'});
  assert.equal(prepared.payload,undefined);
  const volume=assistantVolumeForPreparedRequest(prepared,{...strings(prepared),research:true}).finish('{}');
  assert.equal(volume.itemCount,0);assert.equal(volume.stage,'research');
  assert.equal(volume.contextBytes,Buffer.byteLength(prepared.input));assert.equal(volume.usage.status,'unavailable');
});

test('a streamed terminal record is measured once, independent of private nested fields',()=>{
  const prepared=prepareAssistantRequest({purpose:'discussion',items:[]});
  const observer=assistantVolumeForPreparedRequest(prepared,strings(prepared));
  observer.observe({type:'turn.completed',usage:{input_tokens:0,output_tokens:0},item:{text:'PRIVATE'},id:'PRIVATE'});
  const volume=observer.finish('{}');assert.equal(volume.terminalEventCount,1);
  assert.deepEqual(volume.usage,{status:'observed',basis:'codex_turn_completed_event',input_tokens:0,output_tokens:0});
  assert.doesNotMatch(JSON.stringify(volume),/PRIVATE|"item"|"id"/);
});

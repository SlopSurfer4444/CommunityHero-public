import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantVolumeObservation} from './assistant-volume-observation.mjs';
const input = {input:'{"text":"Привет 👋 PRIVATE_CONTEXT"}', stdin:'Context\nПривет 👋 PRIVATE_STDIN',
  instructions:'PRIVATE_INSTRUCTIONS', schema:'{"PRIVATE_SCHEMA":true}', stage:'first_pass', itemCount:2};

test('counts actual UTF-8 strings before output expansion and allowlists observed CLI usage', () => {
  const observer=assistantVolumeObservation(input), raw='{"decisions":[{"text":"Ответ 👋 PRIVATE_OUTPUT"}]}';
  observer.observe({type:'turn.completed',usage:{input_tokens:100,cached_input_tokens:80,output_tokens:25,
    total_tokens:125,reasoning_tokens:7,PRIVATE_USAGE:'SECRET'}, thread_id:'PRIVATE_THREAD',access_token:'PRIVATE_TOKEN'});
  const value=observer.finish(raw);
  assert.equal(value.contextBytes,Buffer.byteLength(input.input));
  assert.ok(value.contextBytes>input.input.length);
  for (const [field,string] of [['stdinBytes',input.stdin],['instructionBytes',input.instructions],['schemaBytes',input.schema],['rawStructuredOutputBytes',raw]])
    assert.equal(value[field],Buffer.byteLength(string));
  assert.equal(value.stage,'first_pass'); assert.equal(value.itemCount,2); assert.equal(value.terminalEventCount,1);
  assert.deepEqual(value.usage,{status:'observed',basis:'codex_turn_completed_event',input_tokens:100,cached_input_tokens:80,output_tokens:25});
  assert.doesNotMatch(JSON.stringify(value),/PRIVATE|SECRET|total_tokens|reasoning_tokens|thread_id|access_token/);
});

test('absent usage remains unavailable, including streams without a terminal event', () => {
  for (const event of [undefined,{type:'turn.completed'},{type:'turn.completed',usage:null},
    {type:'item.completed',usage:{input_tokens:5,output_tokens:5}}]) {
    const observer=assistantVolumeObservation(input);observer.observe(event);
    assert.equal(observer.finish('{}').usage.status,'unavailable');
  }
});

test('zero usage is observed; missing cached input is never invented', () => {
  const observer=assistantVolumeObservation(input);observer.observe({type:'turn.completed',usage:{input_tokens:0,output_tokens:0}});
  assert.deepEqual(observer.finish('{}').usage,{status:'observed',basis:'codex_turn_completed_event',input_tokens:0,output_tokens:0});
});

test('invalid counts do not leak into a usage claim', () => {
  for (const usage of [[],5,{input_tokens:4},{input_tokens:-1,output_tokens:2},
    {input_tokens:4,output_tokens:NaN},{input_tokens:4,output_tokens:2,cached_input_tokens:1.2},
    {input_tokens:Number.MAX_SAFE_INTEGER+1,output_tokens:2},{input_tokens:'4',output_tokens:2}]) {
    const observer=assistantVolumeObservation(input);observer.observe({type:'turn.completed',usage});
    assert.deepEqual(observer.finish('{}').usage,{status:'invalid',basis:'codex_turn_completed_event'});
  }
});

test('duplicate terminal events remain ambiguous; no double counting or selected last usage', () => {
  const observer=assistantVolumeObservation(input);
  observer.observe({type:'turn.completed',usage:{input_tokens:4,output_tokens:2}});
  observer.observe({type:'turn.completed',usage:{input_tokens:100,output_tokens:20}});
  const value=observer.finish('{}');assert.equal(value.terminalEventCount,2);
  assert.deepEqual(value.usage,{status:'ambiguous',basis:'codex_turn_completed_event'});
});

test('finish is immutable and observations cannot change the captured generation later', () => {
  const observer=assistantVolumeObservation(input);const first=observer.finish('{}');
  first.usage.status='forged';first.contextBytes=0;
  observer.observe({type:'turn.completed',usage:{input_tokens:4,output_tokens:2}});
  const second=observer.finish('ignored');assert.equal(second.rawStructuredOutputBytes,2);
  assert.equal(second.contextBytes,Buffer.byteLength(input.input));assert.equal(second.usage.status,'unavailable');
});

test('call stage and item count remain bounded without retaining request contents', () => {
  for (const stage of ['first_pass','stronger_review','editorial_review','discussion','research'])
    assert.equal(assistantVolumeObservation({...input,stage,itemCount:0}).finish('{}').stage,stage);
  for (const override of [{stage:'PRIVATE_STAGE'},{itemCount:101},{itemCount:-1},{input:null},{schema:{}}])
    assert.throws(()=>assistantVolumeObservation({...input,...override}),TypeError);
  assert.throws(()=>assistantVolumeObservation(input).finish({}),TypeError);
});

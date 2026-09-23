import test from 'node:test';
import assert from 'node:assert/strict';
import {captureChatReading,restoreChatReading} from '../workshop/assistant-reading.js';

test('refresh and appended response preserve the position while reading older messages',()=>{
  const reading=captureChatReading({scrollTop:240,clientHeight:400,scrollHeight:1600});
  const replacement={scrollTop:0,clientHeight:400,scrollHeight:2200};
  restoreChatReading(replacement,reading);
  assert.equal(replacement.scrollTop,240);
});
test('reader at the bottom follows the new answer and initial opening shows latest messages',()=>{
  const reading=captureChatReading({scrollTop:1200,clientHeight:400,scrollHeight:1600});
  const replacement={scrollTop:0,clientHeight:400,scrollHeight:2200};
  restoreChatReading(replacement,reading);
  assert.equal(replacement.scrollTop,1800);
  replacement.scrollTop=0;restoreChatReading(replacement,null);
  assert.equal(replacement.scrollTop,1800);
});
test('collapsed panel cannot overwrite reading position and restoration clamps after content shrinks',()=>{
  assert.equal(captureChatReading({scrollTop:0,clientHeight:0,scrollHeight:0}),null);
  const replacement={scrollTop:0,clientHeight:400,scrollHeight:450};
  restoreChatReading(replacement,{top:600,follow:false});
  assert.equal(replacement.scrollTop,50);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {assistantInstructions,reviewInstructions,generationMetadata} from './assistant.mjs';
test('both preparation passes distinguish useful engagement from no-question automatic closure',()=>{
  for(const prompt of [assistantInstructions(true),reviewInstructions()]){
    assert.match(prompt,/Absence of a question alone is not a\s+reason to close/);
    assert.match(prompt,/prefer one natural, context-specific reaction/);
    assert.match(prompt,/Close when a reply merely repeats the comment/);
    assert.match(prompt,/terminal thanks after an\s+already answered question can close/);
    assert.match(prompt,/never silently dismiss an unanswered factual question or complaint/);
  }
  assert.match(reviewInstructions(),/close may\s+become reply/);
  assert.match(reviewInstructions(),/Never manufacture\s+a question/);
  assert.match(generationMetadata('',true).promptVersion,/v19-intent-scoped-evidence/);
});
test('discussion search contract is read-only bounded and does not pretend it already ran',()=>{
  const prompt=assistantInstructions();
  assert.match(prompt,/one application search/);assert.match(prompt,/at most eight matches/);
  assert.match(prompt,/never claim success before they arrive/);
  assert.match(prompt,/Do not silently pick one ambiguous recipient/);
  assert.match(prompt,/Search alone does not request drafting or action/);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {captureTopicReading} from '../workshop/topic-reading.js';

function panel(height,top,details=[]) {
  return {querySelector:()=>({clientHeight:height,scrollTop:top}),querySelectorAll:()=>details};
}
const rule=(id,open,sourceOpen)=>({dataset:{ruleVersion:id},open,querySelector:()=>({open:sourceOpen})});

test('hidden topic column cannot replace expanded rules and deep reading position',()=>{
  const remembered={top:1800,open:['rule-1'],sources:['rule-1']};
  assert.equal(captureTopicReading(panel(0,0,[rule('rule-1',false,false)]),remembered),null);
  assert.deepEqual(remembered,{top:1800,open:['rule-1'],sources:['rule-1']});
  assert.deepEqual(captureTopicReading(panel(600,1800,[rule('rule-1',true,true)]),remembered),remembered);
});

test('visible topic column captures user changes while an empty rule list preserves disclosures',()=>{
  const remembered={top:1800,open:['rule-1'],sources:['rule-1']};
  assert.deepEqual(captureTopicReading(panel(600,450,[rule('rule-1',false,false),rule('rule-2',true,false)]),remembered),
    {top:450,open:['rule-2'],sources:[]});
  assert.deepEqual(captureTopicReading(panel(600,900),remembered),{top:900,open:['rule-1'],sources:['rule-1']});
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createAssistantRunTracker} from './assistant-run-tracker.js';

const base = {conversationId:'conversation-a',operatorId:'operator-a',navigationRevision:4,screenKey:'queue:attention'};
const job = {id:'job-a',kind:'assistant',refId:base.conversationId,operatorId:base.operatorId,status:'completed'};
const message = {role:'assistant',prepareRunId:job.id,navigation:{kind:'queue',workflow:'prepared'}};

test('only the current submitted and completed run navigates once', () => {
  const runs=createAssistantRunTracker();
  const token=runs.begin(base);
  runs.bind(token,job.id);
  assert.deepEqual(runs.navigation({...base,job,message}),message.navigation);
  assert.equal(runs.navigation({...base,job,message}),null);
});

test('historical, unrelated, or manually displaced runs cannot navigate', () => {
  const runs=createAssistantRunTracker();
  const token=runs.begin(base);
  runs.bind(token,job.id);
  assert.equal(runs.navigation({...base,job:{...job,id:'older'},message}),null);
  assert.equal(runs.navigation({...base,job,message:{...message,prepareRunId:'older'}}),null);
  assert.equal(runs.navigation({...base,job,message,navigationRevision:5}),null);
  assert.equal(runs.navigation({...base,job,message,screenKey:'comment:x'}),null);
  assert.equal(runs.navigation({...base,job:{...job,operatorId:'operator-b'},message}),null);
  assert.equal(runs.navigation({...base,job:{...job,status:'running'},message}),null);
  const next=runs.begin(base);
  runs.bind(token,job.id);
  assert.equal(runs.navigation({...base,job,message}),null);
  runs.bind(next,'job-b');
  assert.equal(runs.navigation({...base,job,message}),null);
});

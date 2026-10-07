import test from 'node:test';
import assert from 'node:assert/strict';
import { planPrepareWindows } from '../cli/prepare-plan-windows.mjs';

const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const tick = () => new Promise(resolve => setImmediate(resolve));
const plan = ids => ({ advisory: true, selectedItemIds: ids, batches: [{ itemIds: ids, bytes: 100 }], held: [] });

test('two readers start concurrently, bounded refill preserves exact window order', async () => {
  const requests = [], gates = [deferred(), deferred(), deferred()]; let active = 0, peak = 0;
  const client = { async planPrepare(ids, instruction) {
    assert.equal(instruction, 'company rule'); const index = Number(ids[0].slice(1)); requests.push(index);
    active++; peak = Math.max(peak, active); await gates[index].promise; active--; return plan(ids);
  }, prepare() { assert.fail('no paid admission'); }, execute() { assert.fail('no provider action'); } };
  const input = [['i0'], ['i1'], ['i2']];
  const pending = planPrepareWindows(client, input, 'company rule', { parallelism: 2 });
  assert.deepEqual(requests, [0, 1]); gates[1].resolve(); await tick(); assert.deepEqual(requests, [0, 1, 2]);
  gates[2].resolve(); await tick(); gates[0].resolve(); const output = await pending;
  assert.equal(peak, 2); assert.deepEqual(output.map(row => row.window), [0, 1, 2]);
  assert.deepEqual(output.map(row => row.selectedItemIds), input); assert.deepEqual(input, [['i0'], ['i1'], ['i2']]);
});

test('failed read stops new dispatch and drains its already started sibling before return', async () => {
  const gates = [deferred(), deferred()], requests = []; const failure = new Error('source unavailable');
  const client = { async planPrepare(ids) { const index = Number(ids[0].slice(1)); requests.push(index); await gates[index].promise; return plan(ids); } };
  let settled = false;
  const result = planPrepareWindows(client, [['i0'], ['i1'], ['i2']], undefined, { parallelism: 2 })
    .then(() => assert.fail('partial set must never be accepted'), error => { settled = true; assert.equal(error, failure); });
  gates[0].reject(failure); await tick(); assert.equal(settled, false); assert.deepEqual(requests, [0, 1]);
  gates[1].resolve(); await result; assert.equal(settled, true); assert.deepEqual(requests, [0, 1]);
});

test('default remains sequential and abort cannot admit another plan', async () => {
  const controller = new AbortController(), gates = [deferred(), deferred()], requests = [];
  const client = { async planPrepare(ids) { const index = Number(ids[0].slice(1)); requests.push(index); await gates[index].promise; return plan(ids); } };
  const pending = planPrepareWindows(client, [['i0'], ['i1']], undefined, { signal: controller.signal });
  assert.deepEqual(requests, [0]); controller.abort(); gates[0].resolve(); await assert.rejects(pending, { code: 'STOPPED' });
  assert.deepEqual(requests, [0]);
});

test('scope or width violations fail before a client call', async () => {
  const client = { planPrepare() { assert.fail('invalid bounds dispatch'); } };
  for (const windows of [[], [['same'], ['same']], [Array(101).fill('id')], Array(9).fill(['id']), [[null]]])
    await assert.rejects(planPrepareWindows(client, windows), { code: 'USAGE' });
  for (const parallelism of [0, 9, 1.5, Infinity])
    await assert.rejects(planPrepareWindows(client, [['one']], undefined, { parallelism }), { code: 'USAGE' });
});

test('already aborted context performs no read', async () => {
  const controller = new AbortController(); controller.abort();
  await assert.rejects(planPrepareWindows({ planPrepare() { assert.fail('aborted dispatch'); } }, [['one']], undefined,
    { parallelism: 2, signal: controller.signal }), { code: 'STOPPED' });
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { familySelectionInput, validateFamilyWindows } from '../cli/queue-family.mjs';
import { ConductorClient } from '../cli/conductor-client.mjs';

const ids = Array.from({ length: 5000 }, (_, n) => `item-${n}`);
const response = (windows, selectedItemIds = ids) => ({ account: 'LikeAvto', advisory: true, selectedItemIds, windows });

test('family metadata spans a campaign while model windows remain bounded and exact', () => {
  assert.equal(familySelectionInput(ids, 100, 2).itemIds.length, 5000);
  const windows = [ids.slice(0, 60).concat(ids.slice(110, 150)), ids.slice(60, 110)];
  assert.deepEqual(validateFamilyWindows(response(windows), ids, 100, 2), windows);
  assert.throws(() => familySelectionInput([...ids, 'overflow'], 100, 1));
  assert.throws(() => familySelectionInput(['a', 'a'], 100, 1));
});

test('four independent family windows retain exact bounded membership', () => {
  assert.equal(familySelectionInput(ids, 100, 4).maxBatches, 4);
  const windows = Array.from({ length: 4 }, (_, n) => ids.slice(n * 100, (n + 1) * 100));
  assert.deepEqual(validateFamilyWindows(response(windows), ids, 100, 4), windows);
  assert.throws(() => familySelectionInput(ids, 100, 9), { code: 'USAGE' });
  assert.throws(() => validateFamilyWindows(response([...windows, ['item-401']]), ids, 100, 4), { code: 'INVALID_FAMILY_SELECTION' });
  assert.throws(() => validateFamilyWindows(response([...windows.slice(0, 3), ['item-0']]), ids, 100, 4), { code: 'INVALID_FAMILY_SELECTION' });
});

test('hostile family responses cannot widen, duplicate, reorder source echo or exceed budgets', () => {
  for (const value of [response([['foreign']]), response([['item-0'], ['item-0']]), response([ids.slice(0, 101)]),
    response([['item-0']], [...ids].reverse()), response([]), response([['item-0'], ['item-1'], ['item-2']]),
    { ...response([['item-0']]), advisory: false }])
    assert.throws(() => validateFamilyWindows(value, ids, 100, 2), { code: 'INVALID_FAMILY_SELECTION' });
});

const config = { version: 1, runId: 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa', leaseGeneration: 2,
  account: 'LikeAvto', capability: 'a'.repeat(64), mode: 'prepare', scopeItemIds: ids,
  checkpointPath: 'private-path', batchSize: 100, maxRepairRounds: 2, maxCycles: 100,
  resume: false, baseUrl: 'http://127.0.0.1:4185', rpcUrl: 'http://127.0.0.1:12345/rpc',
  connectionBinding: { id: 'fixture', workspaceId: 'local-pilot', accountId: 'LikeAvto', connector: 'vk', revision: 1, providerAccountId: 'synthetic' } };

test('conductor selects families through finite RPC with exact campaign identifiers', async () => {
  const calls = [];
  const client = new ConductorClient(config, { fetchImpl: async (url, init) => {
    calls.push({ url, request: JSON.parse(init.body) });
    return new Response(JSON.stringify({ status: 200, body: response([ids.slice(0, 100)]) }));
  } });
  assert.deepEqual(await client.selectPrepareFamilies(ids, 100), [ids.slice(0, 100)]);
  assert.deepEqual(calls, [{ url: config.rpcUrl, request: { operation: 'selectPrepareFamilies',
    args: { itemIds: ids, batchSize: 100, maxBatches: 1 } } }]);
});

test('authorization errors and malformed family responses never silently fall back', async () => {
  for (const status of [403, 404]) {
    let calls = 0;
    const client = new ConductorClient(config, { fetchImpl: async () => {
      calls++; return new Response(JSON.stringify({ status, body: { error: 'denied' } }));
    } });
    await assert.rejects(client.selectPrepareFamilies(ids, 100)); assert.equal(calls, 1);
  }
});

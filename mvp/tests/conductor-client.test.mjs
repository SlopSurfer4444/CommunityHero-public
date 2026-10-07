import test from 'node:test';
import assert from 'node:assert/strict';
import { campaignHolds, emitCampaignResult } from '../cli/conductor.mjs';
import { rpcRequest, ConductorClient } from '../cli/conductor-client.mjs';

test('5000 Unicode holds fit bounded ordered frames and retain every exact recipient', async () => {
  const ids = Array.from({ length: 5000 }, (_, n) => `item-${n}`);
  const itemHolds = campaignHolds({ scopeHolds: ids.map(itemId => ({ itemId, reason: 'Частный факт '.repeat(1000) })) }, ids);
  const frames = [];
  await emitCampaignResult({ mode: 'complete-with-holds', summary: { total: 5000, held: 5000 }, itemHolds }, async frame => frames.push(frame));
  assert.equal(frames[0].report.event, 'holds-reset');
  const pages = frames.filter(frame => frame.report?.event === 'holds-page');
  assert.equal(pages.length, 50);
  assert.deepEqual(pages.flatMap(frame => frame.report.itemHolds.map(hold => hold.itemId)), ids);
  assert.ok(frames.every(frame => Buffer.byteLength(JSON.stringify(frame)) < 256 * 1024));
  assert.deepEqual(frames.at(-1), { type: 'result', result: { mode: 'complete-with-holds', summary: { total: 5000, held: 5000 } } });
  assert.ok(itemHolds.every(hold => Buffer.byteLength(hold.reason) <= 1027));
});

test('original operation reconcile uses finite RPC while arbitrary mutation remains denied', async () => {
  assert.deepEqual(rpcRequest(new URL('http://127.0.0.1:4185/api/operations/original/reconcile'), 'POST', {}),
    { operation: 'reconcile', args: { operationId: 'original' } });
  assert.throws(() => rpcRequest(new URL('http://127.0.0.1:4185/api/operations/original/retry'), 'POST', {}));
  const calls = [];
  const config = { version: 1, runId: 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa', leaseGeneration: 2,
    connectionBinding: { id: 'fixture', workspaceId: 'local-pilot', accountId: 'LikeAvto', connector: 'vk', revision: 1, providerAccountId: 'fixture-account' },
    account: 'LikeAvto', capability: 'a'.repeat(64), mode: 'execute', scopeItemIds: ['item-1'],
    checkpointPath: 'parent-supplied-private-path', batchSize: 100, maxRepairRounds: 2, maxCycles: 100,
    resume: true, baseUrl: 'http://127.0.0.1:4185', rpcUrl: 'http://127.0.0.1:12345/rpc' };
  const client = new ConductorClient(config, { fetchImpl: async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body) });
    return new Response(JSON.stringify({ status: 200, body: JSON.parse(init.body).operation === 'engineStatus'
      ? { account: config.account, connectionDependency: null } : { jobId: 'original-readback' } }));
  } });
  await client.reconcile('original');
  assert.equal(calls.length, 2); assert.ok(calls.every(call => call.url === config.rpcUrl));
  assert.deepEqual(calls[1].body, { operation: 'reconcile', args: { operationId: 'original' } });
});

test('public fact resolver uses exact finite RPC and captured campaign capability', async () => {
  const config = { version: 1, runId: 'aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa', leaseGeneration: 2,
    connectionBinding: { id: 'fixture', workspaceId: 'local-pilot', accountId: 'BAW Russia', connector: 'vk', revision: 1, providerAccountId: 'fixture-account' },
    account: 'BAW Russia', capability: 'a'.repeat(64), mode: 'prepare', scopeItemIds: ['a', 'b'],
    checkpointPath: 'private-path', batchSize: 100, maxRepairRounds: 2, maxCycles: 100,
    resume: false, baseUrl: 'http://127.0.0.1:4185', rpcUrl: 'http://127.0.0.1:12345/rpc' };
  const calls = [];
  const client = new ConductorClient(config, { fetchImpl: async (url, init) => {
    const request = JSON.parse(init.body); calls.push({ url, request, headers: init.headers });
    return new Response(JSON.stringify({ status: 200, body: request.operation === 'engineStatus'
      ? { account: config.account, connectionDependency: null } : { jobIds: ['original-research'], readyItemIds: [], held: [] } }));
  } });
  await client.resolvePublicFacts('prepare-original', ['b']);
  assert.deepEqual(calls[1].request, { operation: 'resolvePublicFacts', args: { prepareJobId: 'prepare-original', itemIds: ['b'] } });
  assert.ok(calls.every(call => call.url === config.rpcUrl && call.headers['x-conductor-run'] === config.runId
    && call.headers['x-conductor-generation'] === '2' && call.headers['x-conductor-capability'] === config.capability));
  for (const ids of [['outside'], ['a', 'a'], []]) await assert.rejects(client.resolvePublicFacts('prepare-original', ids), { code: 'INVALID_DEPENDENCY_SCOPE' });
  assert.equal(calls.length, 2);
  assert.throws(() => rpcRequest(new URL(`${config.baseUrl}/api/engine/prepare/facts/arbitrary`), 'POST', {}), { code: 'CONDUCTOR_OPERATION_DENIED' });
});

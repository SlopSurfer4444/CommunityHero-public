import test from 'node:test';
import assert from 'node:assert/strict';
import { CommunityHeroClient, UnknownMutationError } from '../cli/client.mjs';

const refs = [{ id: 'held-video', revision: 3 }, { id: 'ready-text', revision: 7 }];
const binding = { id: 'fixture-connection', workspaceId: 'offline-workspace', accountId: 'LikeAvto',
  connector: 'vk', revision: 1, providerAccountId: 'isolated' };
function fixture({ allHeld = false } = {}) {
  const ready = allHeld ? [] : [refs[1]];
  return {
    version: 1, contract: 'communityhero-operator-review-frontier-v1', account: 'LikeAvto', connectorBinding: binding,
    requested: refs, readyForOperatorReview: ready,
    held: (allHeld ? refs : [refs[0]]).map(reference => ({ reference, stage: 'operator_candidate', reason: 'Complete media evidence required' })),
    preview: ready.length ? {
      version: 1, contract: 'communityhero-operator-assisted-editorial-v1', method: 'assistant_on_operator_authority',
      account: 'LikeAvto', connectorBinding: binding, proposals: ready, previewDigest: 'a'.repeat(64),
      entries: ready.map(ref => ({ candidate: { proposalId: ref.id, proposalRevision: ref.revision }, evidence: {} }))
    } : null
  };
}
function mocked(value, { signal, fetchError } = {}) {
  const calls = [];
  const client = new CommunityHeroClient({ account: 'likeavto', signal, fetchImpl: async (url, options) => {
    calls.push({ path: url.pathname, ...options, body: options.body && JSON.parse(options.body) });
    if (url.pathname === '/api/session') return Response.json({ csrfToken: 'offline-fixture' });
    assert.equal(url.pathname, '/api/proposals/operator-review/frontier', 'no approval, execution, model or other request');
    if (fetchError) throw fetchError;
    return Response.json(value);
  } });
  return { client, calls };
}

test('mixed frontier sends one CSRF-protected read and retains exact ready and held revisions', async () => {
  const { client, calls } = mocked(fixture());
  const result = await client.operatorReviewFrontier(refs);
  assert.deepEqual(result.readyForOperatorReview, [refs[1]]);
  assert.deepEqual(result.held[0].reference, refs[0]);
  assert.deepEqual(calls.map(call => call.path), ['/api/session', '/api/proposals/operator-review/frontier']);
  assert.equal(calls[1].method, 'POST');
  assert.equal(calls[1].headers['x-csrf-token'], 'offline-fixture');
  assert.deepEqual(calls[1].body, { proposals: refs });
});

test('all-held remains inspectable without an admissible preview', async () => {
  const { client } = mocked(fixture({ allHeld: true }));
  const result = await client.operatorReviewFrontier(refs);
  assert.equal(result.preview, null);
  assert.deepEqual(result.readyForOperatorReview, []);
  assert.equal(result.held.length, 2);
});

test('foreign company all-held evidence cannot be consumed', async () => {
  const value = fixture({ allHeld: true }); value.account = 'BAW';
  const { client } = mocked(value);
  await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'WRONG_ACCOUNT');
});

test('invalid request is rejected before session or frontier request', async () => {
  for (const input of [[], [...refs, refs[0]], [{ id: 'p', revision: 0 }], [{ id: 'p', revision: 1, itemId: 'extra' }],
    Array.from({ length: 101 }, (_, index) => ({ id: `p-${index}`, revision: 1 }))]) {
    const { client, calls } = mocked(fixture());
    await assert.rejects(client.operatorReviewFrontier(input), error => error.code === 'USAGE');
    assert.deepEqual(calls, []);
  }
});

test('malformed or foreign native bindings are rejected for mixed and all-held frontiers', async () => {
  for (const allHeld of [false, true]) {
    for (const invalidBinding of [{}, { ...binding, accountId: 'BAW Russia' }, { ...binding, id: '' },
      { ...binding, workspaceId: null }, { ...binding, providerAccountId: '' }, { ...binding, revision: 0 },
      { ...binding, connector: 'invented-connector' }]) {
      const value = JSON.parse(JSON.stringify(fixture({ allHeld })));
      value.connectorBinding = invalidBinding;
      if (value.preview) value.preview.connectorBinding = invalidBinding;
      const { client } = mocked(value);
      await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'INVALID_OPERATOR_FRONTIER');
    }
  }
});

test('coverage corruption, stale revisions and forged ready preview fail closed', async () => {
  const mutations = [
    value => { value.held = []; },
    value => { value.readyForOperatorReview.push(refs[0]); },
    value => { value.readyForOperatorReview[0].revision += 1; },
    value => { value.requested.reverse(); },
    value => { value.held[0].stage = 'approved'; },
    value => { value.preview.proposals = [refs[0]]; },
    value => { value.preview.entries[0].candidate.proposalRevision += 1; },
    value => { value.preview.connectorBinding.providerAccountId = 'foreign'; },
    value => { value.preview.previewDigest = ''; },
    value => { value.preview = null; },
  ];
  for (const mutate of mutations) {
    // Independent object graphs ensure each hostile case actually changes one
    // observed seam, rather than mutating the expected request with the result.
    const value = JSON.parse(JSON.stringify(fixture())); mutate(value);
    const { client } = mocked(value);
    await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'INVALID_OPERATOR_FRONTIER');
  }
});

test('all-held cannot carry a phantom admissible preview', async () => {
  const value = fixture({ allHeld: true }); value.preview = fixture().preview;
  const { client } = mocked(value);
  await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'INVALID_OPERATOR_FRONTIER');
});

test('frontier transport failure is a read failure with no mutation retry or UNKNOWN admission', async () => {
  const { client, calls } = mocked(null, { fetchError: new TypeError('offline synthetic failure') });
  await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'NETWORK_ERROR' && !(error instanceof UnknownMutationError));
  assert.equal(calls.filter(call => call.path.endsWith('/frontier')).length, 1);
});

test('cancelled inspection starts no request or side effect', async () => {
  const controller = new AbortController(); controller.abort();
  const { client, calls } = mocked(fixture(), { signal: controller.signal });
  await assert.rejects(client.operatorReviewFrontier(refs), error => error.code === 'STOPPED');
  assert.equal(calls.length, 0);
});

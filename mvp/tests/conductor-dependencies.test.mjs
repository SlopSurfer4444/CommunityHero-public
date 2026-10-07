import test from 'node:test';
import assert from 'node:assert/strict';
import { campaignHolds, createCampaignDependencies } from '../cli/conductor.mjs';

const context = { heldItemIds: ['media', 'fact'], heldSlices: [
  { itemIds: ['media'], planHoldReason: 'media_pending' }, { itemIds: ['fact'], status: 'prepare-partial-failed' }
], factDependencies: [{ prepareJobId: 'parent', itemId: 'fact', kind: 'missing_public_fact' }] };

test('pending media does not prevent a resolved public fact from waking in the same sweep', async () => {
  const calls = [];
  const media = { observeOnce: async selected => { calls.push('media'); assert.deepEqual(selected.heldItemIds, ['media']); return { readyItemIds: [], pending: true }; } };
  const facts = { observeOnce: async selected => { calls.push('facts'); assert.deepEqual(selected.factDependencies, context.factDependencies); return { readyItemIds: ['fact'], pending: false }; } };
  const observer = createCampaignDependencies(media, facts, { pollMs: 60000, maxPolls: 1 });
  assert.deepEqual(await observer(context), ['fact']); assert.deepEqual(calls, ['media', 'facts']);
});

test('pending public fact does not prevent ready media; later turns rotate lane priority', async () => {
  const calls = [];
  const media = { observeOnce: async () => { calls.push('media'); return { readyItemIds: ['media'], pending: false }; } };
  const facts = { observeOnce: async () => { calls.push('facts'); return { readyItemIds: [], pending: true }; } };
  const observer = createCampaignDependencies(media, facts, { pollMs: 60000, maxPolls: 1 });
  assert.deepEqual(await observer(context), ['media']); assert.deepEqual(calls, ['media']);
  assert.deepEqual(await observer(context), ['media']); assert.deepEqual(calls, ['media', 'facts', 'media']);
});

test('all terminal dependency holds return empty; pending holds preserve the common poll bound', async () => {
  const terminal = { observeOnce: async () => ({ readyItemIds: [], pending: false }) };
  assert.deepEqual(await createCampaignDependencies(terminal, terminal, { maxPolls: 1 })(context), []);
  const pending = { observeOnce: async () => ({ readyItemIds: [], pending: true }) };
  await assert.rejects(createCampaignDependencies(pending, terminal, { maxPolls: 1 })(context), { code: 'POLL_LIMIT' });
});

test('a dependency cannot wake another lane recipient or expand scope', async () => {
  for (const id of ['fact', 'outside']) {
    const media = { observeOnce: async () => ({ readyItemIds: [id], pending: false }) };
    await assert.rejects(createCampaignDependencies(media, null, { maxPolls: 1 })(context), { code: 'INVALID_DEPENDENCY_SCOPE' });
  }
});

test('per-recipient resolution removes only its original slice hold while its sibling stays visible', () => {
  const checkpoint = { slices: [{ itemIds: ['resolved', 'sibling'], status: 'held', dependencyResolvedItemIds: ['resolved'],
    child: { proposals: [{ id: 'p-resolved', itemId: 'resolved' }, { id: 'p-sibling', itemId: 'sibling' }],
      editorialReview: { outcome: { held: [{ reference: { id: 'p-resolved' }, reason: 'missing_public_fact' },
        { reference: { id: 'p-sibling' }, reason: 'private_fact' }] } } } }] };
  const holds = campaignHolds(checkpoint, ['resolved', 'sibling']);
  assert.deepEqual(holds.map(row => row.itemId), ['sibling']);
});

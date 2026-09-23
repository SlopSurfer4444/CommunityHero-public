// Deterministic bridge for the isolated engine HTTP acceptance. It never opens a socket.
import {appendFile, mkdir, readFile, rm, writeFile} from 'node:fs/promises';

let input = '';
for await (const chunk of process.stdin) input += chunk;
const request = JSON.parse(input);
const operation = request.operation ?? request.op;
// Every operation must carry the server-owned canonical account key.
const account = request.account;
const allowedAccounts = new Set(['likeavto', 'baw-russia']);

function fail(code, message = code) {
  process.stdout.write(JSON.stringify({ok: false, error: {code, message}}));
  process.exit(0);
}
if (!allowedAccounts.has(account)) fail('ACCOUNT_SCOPE_MISMATCH');

async function jsonFile(file, fallback = {}) {
  try { return JSON.parse(await readFile(file, 'utf8')); }
  catch { return fallback; }
}
const scenario = await jsonFile(process.env.COMMUNITYHERO_TEST_SCENARIO, {});
const effectsFile = process.env.COMMUNITYHERO_TEST_EFFECTS;
const traceFile = process.env.COMMUNITYHERO_TEST_TRACE;
const concurrencyFile = process.env.COMMUNITYHERO_TEST_CONCURRENCY;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function withLock(file, task) {
  if (!file) return task();
  const lock = `${file}.lock`;
  for (let attempt = 0; attempt < 6000; attempt += 1) {
    try { await mkdir(lock); break; }
    catch (error) {
      if (error?.code !== 'EEXIST') throw error;
      if (attempt === 5999) throw Error('fake bridge lock timeout');
      await delay(5);
    }
  }
  try { return await task(); }
  finally { await rm(lock, {recursive: true, force: true}); }
}

async function appendJson(file, value) {
  if (!file) return;
  await withLock(file, () => appendFile(file, `${JSON.stringify(value)}\n`));
}
await appendJson(traceFile, {operation, account, request});

const digest = 'a'.repeat(64);
const itemCount = Number.isSafeInteger(scenario.itemCount) ? scenario.itemCount : 10;
function fixtureItem(index) {
  const sharedConversation = itemCount >= 3 && index >= itemCount - 3
    ? `${account}:shared-conversation`
    : `${account}:conversation-${index}`;
  const platform = index === itemCount - 1 ? 'tiktok' : 'vk';
  return {
    id: `item-${account}-${index}`,
    itemId: `${account}-provider-item-${index}`,
    objectId: `${account}-object-${index}`,
    postKey: `${account}:post-${index}`,
    conversationKey: sharedConversation,
    contextEvidenceDigest: digest,
    branchId: `branch-${account}-${index}`,
    targetId: `comment-${account}-${index}`,
    createdAt: `2026-09-22T10:${String(index).padStart(2, '0')}:00Z`,
    providerStatus: 'new',
    status: 'new',
    platform,
    workflow: 'attention',
    draft: '',
    revision: 1,
    author: `Synthetic Author ${index}`,
    text: `Synthetic question ${index}`,
    commentText: `Synthetic question ${index}`,
    expectedStatuses: ['new'],
  };
}
const items = Array.from({length: itemCount}, (_, index) => fixtureItem(index));
const itemByProviderId = new Map(items.map(item => [item.itemId, item]));
const posts = items.map((item, index) => ({
  id: `post-${account}-${index}`,
  postKey: item.postKey,
  title: `Synthetic post ${index}`,
  text: `Verified synthetic source ${index}`,
  channel: item.platform.toUpperCase(),
}));
const branches = items.map((item, index) => ({
  id: item.branchId,
  postId: `post-${account}-${index}`,
  contextComplete: true,
  missingParentIds: [],
  contextTruncated: false,
  messages: [{
    id: item.targetId,
    author: item.author,
    text: item.text,
    role: 'customer',
    createdAt: item.createdAt,
  }],
}));
const baselineFor = action => [`old-official-${action.itemId}`];

async function executeAction(action) {
  const stateFile = concurrencyFile;
  const itemKey = `item:${action.itemId}`;
  const conversationKey = action.action === 'reply_and_close'
    ? `conversation:${action.conversationKey}`
    : null;
  const keys = [itemKey, conversationKey].filter(Boolean);
  await withLock(stateFile, async () => {
    const state = await jsonFile(stateFile, {active: [], maxActive: 0, violations: [], starts: [], finishes: []});
    const collision = keys.find(key => state.active.some(active => active.keys.includes(key)));
    if (collision) state.violations.push({collision, actionId: action.actionId});
    state.active.push({actionId: action.actionId, keys});
    state.maxActive = Math.max(state.maxActive, state.active.length);
    state.starts.push({actionId: action.actionId, itemId: action.itemId, conversationKey: action.conversationKey});
    await writeFile(stateFile, JSON.stringify(state));
  });
  try {
    await delay(scenario.executeDelayMs ?? 140);
    if (!scenario.executeFailure) await appendJson(effectsFile, {operation: 'execute', account, action});
  } finally {
    await withLock(stateFile, async () => {
      const state = await jsonFile(stateFile, {active: [], maxActive: 0, violations: [], starts: [], finishes: []});
      state.active = state.active.filter(active => active.actionId !== action.actionId);
      state.finishes.push({actionId: action.actionId, itemId: action.itemId});
      await writeFile(stateFile, JSON.stringify(state));
    });
  }
  return {
    actionId: action.actionId,
    itemId: action.itemId,
    status: scenario.executeFailure ? 'failed' : scenario.executeUnknown ? 'unknown' : 'accepted',
    ...(scenario.executeFailure ? {mutationOutcome: scenario.executeFailure} : {}),
    receipt: {id: `receipt-${action.actionId}`},
    readbackEvidence: {baselineReplyIds: baselineFor(action)},
  };
}

let result;
switch (operation) {
case 'caps':
  result = {
    version: 1,
    operation: 'caps',
    account,
    contractVersion: 'synthetic-engine-acceptance-v1',
    local: {
      operations: ['caps', 'scan', 'read', 'context', 'execute', 'readback'],
      actions: ['close', 'reply_and_close', 'hide', 'delete'],
      runtimeSideEffects: 'synthetic-only',
      maxActions: 100,
      maxInFlight: {min: 1, max: 100},
    },
    provider: {source: 'engine-fake-bridge', readOnlyProbe: true},
  };
  break;
case 'scan':
  result = {
    version: 1,
    operation: 'scan',
    account,
    items: items.map(item => ({...item})),
    continuation: null,
    coverage: {complete: true, synthetic: true},
    containsExecutableActions: false,
  };
  break;
case 'read':
  result = {
    account,
    posts,
    branches,
    items,
    hasMore: false,
    cursor: null,
    coverage: 'complete synthetic page',
    observedCount: items.length,
    scannedCount: items.length,
    skipped: [],
    live: {account, fetchedAt: new Date().toISOString(), hasMore: false, observedCount: items.length, readOnly: true},
  };
  break;
case 'context': { // Baseline is deliberately supplied before the mutation.
  const item = itemByProviderId.get(request.itemId);
  if (!item || request.objectId !== item.objectId) fail('ITEM_SCOPE_MISMATCH');
  result = {...item, officialReplyIds: baselineFor({itemId: item.itemId})};
  break;
}
case 'assistant':
  if (request.purpose === 'triage' || request.purpose === 'triage_review') {
    result = {
      text: `Reviewed ${request.items?.length ?? 0} synthetic candidates for ${account}.`,
      sources: [],
      assessments: (request.items ?? []).map(item => ({
        itemId: item.id,
        outcome: 'reply',
        reason: 'Synthetic substantive question requires an exact reviewed reply.',
        tags: ['question'],
      })),
      proposals: (request.items ?? []).map(item => ({
        itemId: item.id,
        kind: 'reply_and_close',
        text: `Synthetic reviewed reply for ${item.id}`,
      })),
    };
    if (request.purpose === 'triage_review') {
      const completedAt = new Date().toISOString();
      result.runMetadata = {
        schemaVersion: 1,
        model: 'engine-fake-review',
        reasoningEffort: 'medium',
        promptVersion: 'engine-acceptance-v1',
        instructionSha256: 'a'.repeat(64),
        inputSha256: 'b'.repeat(64),
        cliSha256: 'c'.repeat(64),
        elapsedMs: 1,
        completedAt,
        research: {
          version: 1,
          status: 'no_sources',
          model: 'engine-fake-review',
          reasoningEffort: 'medium',
          instructionSha256: 'a'.repeat(64),
          inputSha256: 'b'.repeat(64),
          elapsedMs: 1,
          completedAt,
          webCalls: 0,
          sources: [],
        },
      };
    }
  } else result = {
      text: `Prepared ${request.items?.length ?? 0} synthetic proposals for ${account}.`,
      sources: [],
      proposals: (request.items ?? []).map(item => ({
        itemId: item.id,
        kind: 'reply_and_close',
        text: `Synthetic approved reply for ${item.id}`,
      })),
    };
  break;
case 'execute':
  result = {account, results: await Promise.all((request.actions ?? []).map(executeAction))};
  break;
case 'readback':
  result = {
    account,
    results: (request.actions ?? []).map(action => {
      const expected = baselineFor(action);
      const supplied = action.readbackEvidence?.baselineReplyIds;
      const strictEvidence = Array.isArray(supplied)
        && supplied.length === expected.length
        && supplied.every((value, index) => value === expected[index]);
      return {
        actionId: action.actionId,
        itemId: action.itemId,
        status: !scenario.readbackUnknown && strictEvidence ? 'verified' : 'unknown',
        evidence: {strictEvidence, baselineReplyIds: supplied ?? null},
      };
    }),
  };
  break;
case 'materials':
  result = {account, materials: [{
    id: `policy-${account}`,
    title: `Synthetic ${account} policy`,
    text: 'Use only the deterministic acceptance evidence.',
    kind: 'knowledge',
    revision: 1,
  }]};
  break;
case 'media':
  result = {account, materials: []};
  break;
default:
  fail('TEST_BAD_OPERATION');
}

process.stdout.write(JSON.stringify({ok: true, result}));

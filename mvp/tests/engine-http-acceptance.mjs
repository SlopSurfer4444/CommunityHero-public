// End-to-end synthetic acceptance for the isolated, single-account engine.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {mkdtemp, readFile, writeFile} from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';

const mvp = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const binary = process.env.COMMUNITYHERO_TEST_BINARY
  || path.join(mvp, 'server', 'target', 'debug', 'communityhero-server.exe');
const bridge = path.join(mvp, 'tests', 'engine-fake-bridge.mjs');
const children = new Set();

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => server.listen(0, '127.0.0.1', resolve).once('error', reject));
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return port;
}
function cleanEnvironment(explicit) {
  return Object.fromEntries([
    ...Object.entries(process.env).filter(([key]) => !key.toUpperCase().startsWith('COMMUNITYHERO_')),
    ...Object.entries(explicit),
  ]);
}
async function terminate(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill();
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    sleep(3000),
  ]);
  if (child.exitCode === null && child.signalCode === null) {
    if (process.platform === 'win32') {
      const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], {windowsHide: true, stdio: 'ignore'});
      await new Promise(resolve => killer.once('exit', resolve));
    } else child.kill('SIGKILL');
  }
}

async function launch({account, directory, scenario, effects, trace, concurrency, expectFailure = false}) {
  const port = await freePort();
  let stderr = '';
  const child = spawn(binary, [], {
    cwd: path.join(mvp, 'server'),
    windowsHide: true,
    env: cleanEnvironment({
      COMMUNITYHERO_ACCOUNT: account,
      COMMUNITYHERO_PORT: String(port),
      COMMUNITYHERO_DATA_DIR: directory,
      COMMUNITYHERO_NODE: process.execPath,
      COMMUNITYHERO_BRIDGE: bridge,
      COMMUNITYHERO_TEST_SCENARIO: scenario,
      COMMUNITYHERO_TEST_EFFECTS: effects,
      COMMUNITYHERO_TEST_TRACE: trace,
      COMMUNITYHERO_TEST_CONCURRENCY: concurrency,
      COMMUNITYHERO_EXTERNAL_WRITES: 'enabled',
      COMMUNITYHERO_BACKGROUND_DISABLED: '1',
      COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED: '1',
    }),
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  children.add(child);
  child.once('exit', () => children.delete(child));
  child.stderr.on('data', chunk => { stderr += chunk.toString(); });
  const base = `http://127.0.0.1:${port}`;
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (child.exitCode !== null) {
      if (expectFailure) return {child, base, port, stderr: () => stderr, failed: true};
      throw Error(`server exited during startup (${child.exitCode}): ${stderr}`);
    }
    try {
      const response = await fetch(`${base}/api/health`);
      if (response.ok) {
        if (expectFailure) {
          await terminate(child);
          assert.fail(`server unexpectedly accepted ${account} for ${directory}`);
        }
        return {child, base, port, stderr: () => stderr, failed: false};
      }
    } catch {}
    await sleep(50);
  }
  await terminate(child);
  throw Error(`server startup timeout: ${stderr}`);
}

async function lines(file) {
  try {
    const text = await readFile(file, 'utf8');
    return text.trim() ? text.trim().split(/\r?\n/).map(line => JSON.parse(line)) : [];
  } catch { return []; }
}

function client(server) {
  let csrf = '';
  async function api(url, method = 'GET', body) {
    const response = await fetch(`${server.base}${url}`, {
      method,
      headers: {'Content-Type': 'application/json', 'X-CSRF-Token': csrf, Origin: server.base},
      ...(body === undefined ? {} : {body: JSON.stringify(body)}),
    });
    let value;
    try { value = await response.json(); }
    catch { value = {}; }
    return {status: response.status, value};
  }
  async function bootstrap() {
    const response = await api('/api/bootstrap');
    assert.equal(response.status, 200, JSON.stringify(response.value));
    csrf = response.value.csrfToken;
    assert.ok(csrf);
    return response.value;
  }
  async function waitJob(jobId, timeoutMs = 20000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const response = await api(`/api/engine/jobs/${jobId}`);
      assert.equal(response.status, 200, JSON.stringify(response.value));
      if (['completed', 'failed', 'cancelled', 'interrupted'].includes(response.value.status)) return response.value;
      await sleep(200);
    }
    throw Error(`job ${jobId} timed out`);
  }
  return {api, bootstrap, waitJob};
}

async function runProcess(command, args, {cwd = mvp, env = cleanEnvironment({}), timeoutMs = 120000} = {}) {
  const child = spawn(command, args, {cwd, env, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe']});
  children.add(child);
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk.toString(); });
  child.stderr.on('data', chunk => { stderr += chunk.toString(); });
  const timer = setTimeout(() => terminate(child), timeoutMs);
  const code = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  clearTimeout(timer);
  children.delete(child);
  return {code, stdout, stderr};
}

async function runCliAcceptance() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'communityhero-engine-cli-'));
  const directory = path.join(root, 'data');
  const scenario = path.join(root, 'scenario.json');
  const effects = path.join(root, 'effects.jsonl');
  const trace = path.join(root, 'trace.jsonl');
  const concurrency = path.join(root, 'concurrency.json');
  await writeFile(scenario, JSON.stringify({itemCount: 2, executeDelayMs: 40}));
  const server = await launch({account: 'likeavto', directory, scenario, effects, trace, concurrency});
  const cli = path.join(mvp, 'cli', 'communityhero.mjs');
  const common = ['--base-url', server.base, '--account', 'likeavto', '--poll-ms', '50', '--max-polls', '800'];
  try {
    const queueCheckpoint = path.join(root, 'queue.json');
    const queue = await runProcess(process.execPath, [cli, 'queue', ...common,
      '--checkpoint', queueCheckpoint, '--batch-size', '2', '--max-cycles', '5']);
    assert.equal(queue.code, 0, queue.stderr);
    const queueResult = JSON.parse(queue.stdout);
    assert.equal(queueResult.mode, 'complete', JSON.stringify(queueResult));
    assert.equal(queueResult.counts.prepared, 2);
    assert.equal((await lines(effects)).length, 0, 'prepare-only queue executed an action');

    const upgrade = await runProcess(process.execPath, [cli, 'queue', ...common,
      '--resume', queueCheckpoint, '--autonomous', '--execute', '--max-cycles', '5']);
    assert.equal(upgrade.code, 0, upgrade.stderr);
    assert.equal(JSON.parse(upgrade.stdout).mode, 'complete', upgrade.stdout);
    assert.equal((await lines(effects)).length, 2, 'queue upgrade did not execute its two exact prepared actions');
    const replay = await runProcess(process.execPath, [cli, 'queue', ...common,
      '--resume', queueCheckpoint, '--autonomous', '--execute', '--max-cycles', '5']);
    assert.equal(replay.code, 0, replay.stderr);
    assert.equal(JSON.parse(replay.stdout).mode, 'complete', replay.stdout);
    assert.equal((await lines(effects)).length, 2, 'completed queue replay duplicated execution');
    const traceRows = await lines(trace);
    assert.ok(traceRows.length > 0);
    assert.ok(traceRows.every(row => row.account === 'likeavto'), 'CLI bridge call lost canonical account binding');
    return {directory: root, queuePrepared: queueResult.counts.prepared, upgradedEffects: 2, replayEffects: 0};
  } finally {
    await terminate(server.child);
  }
}

async function runAccount(account, {exerciseRecovery = false} = {}) {
  const root = await mkdtemp(path.join(os.tmpdir(), `communityhero-engine-${account}-`));
  const directory = path.join(root, 'data');
  const scenario = path.join(root, 'scenario.json');
  const effects = path.join(root, 'effects.jsonl');
  const trace = path.join(root, 'trace.jsonl');
  const concurrency = path.join(root, 'concurrency.json');
  const batchSize = exerciseRecovery ? 100 : 8;
  const itemCount = batchSize + 3;
  await writeFile(scenario, JSON.stringify({itemCount, executeDelayMs: 140}));
  let server = await launch({account, directory, scenario, effects, trace, concurrency});
  let http = client(server);
  try {
    let state = await http.bootstrap();
    const status = await http.api('/api/engine/status');
    assert.equal(status.status, 200);
    assert.equal(status.value.account, account);
    assert.equal(status.value.externalWrites, true);
    assert.equal(status.value.authority, 'shared-rust-engine');

    const effectsBeforeReads = (await lines(effects)).length;
    const caps = await http.api('/api/engine/capabilities');
    assert.equal(caps.status, 200, JSON.stringify(caps.value));
    assert.equal(caps.value.account, account);
    assert.equal(caps.value.provider.readOnlyProbe, true);
    const emptyExport = await http.api('/api/engine/export');
    assert.equal(emptyExport.status, 200);
    assert.equal(emptyExport.value.account, account);
    assert.equal(emptyExport.value.containsExecutableActions, false);
    for (const forbidden of ['proposals', 'approvals', 'operations', 'jobs']) {
      assert.equal(Object.hasOwn(emptyExport.value, forbidden), false, `export leaked ${forbidden}`);
    }
    assert.equal((await lines(effects)).length, effectsBeforeReads, 'read-only engine endpoints caused execution');

    const scan = await http.api('/api/engine/scan', 'POST', {statuses: ['open'], pageSize: 10, maxPages: 1});
    assert.equal(scan.status, 200, JSON.stringify(scan.value));
    const scanJob = await http.waitJob(scan.value.jobId);
    assert.equal(scanJob.status, 'completed', JSON.stringify(scanJob));
    assert.equal(scanJob.result.account, account);
    assert.equal(scanJob.result.containsExecutableActions, false);
    assert.equal((await lines(effects)).length, effectsBeforeReads, 'provider scan caused execution');

    const sync = await http.api('/api/sync', 'POST', {mode: 'open'});
    assert.equal(sync.status, 200, JSON.stringify(sync.value));
    assert.equal((await http.waitJob(sync.value.jobId)).status, 'completed');
    state = await http.bootstrap();
    assert.equal(state.items.length, itemCount);
    assert.ok(state.items.every(item => item.connectorBinding?.providerAccountId === account));

    const prepareItemId = state.items[0].id;
    const prepare = await http.api('/api/engine/prepare', 'POST', {
      itemIds: [prepareItemId],
      instruction: 'Prepare one synthetic candidate with a second-pass review.',
    });
    assert.equal(prepare.status, 200, JSON.stringify(prepare.value));
    const prepareJob = await http.waitJob(prepare.value.jobId, 30000);
    assert.equal(prepareJob.status, 'completed', JSON.stringify(prepareJob));
    assert.equal(prepareJob.kind, 'assistant');
    assert.equal(prepareJob.purpose, 'engine_prepare');
    assert.deepEqual(prepareJob.result.selectedItemIds, [prepareItemId]);
    assert.deepEqual(prepareJob.result.preparedItemIds, [prepareItemId]);
    assert.deepEqual(prepareJob.result.held, []);
    assert.equal(prepareJob.result.candidates.length, 1);
    assert.equal(prepareJob.preparationStages.first.status, 'completed');
    assert.equal(prepareJob.preparationStages.first.reviewRequired, true);
    assert.equal(prepareJob.preparationStages.review.status, 'completed');
    assert.equal(prepareJob.preparationStages.review.research.status, 'no_sources');
    state = await http.bootstrap();
    const preparedCandidate = state.proposals.find(proposal => proposal.prepareRunId === prepare.value.jobId);
    assert.ok(preparedCandidate, 'engine prepare did not persist its candidate');
    assert.equal(preparedCandidate.itemId, prepareItemId);
    assert.equal(preparedCandidate.status, 'draft');
    assert.equal((await lines(effects)).length, effectsBeforeReads, 'engine prepare caused execution');
    const preparationCalls = (await lines(trace)).filter(row => row.operation === 'assistant'
      && ['triage', 'triage_review'].includes(row.request.purpose));
    assert.deepEqual(preparationCalls.slice(-2).map(row => row.request.purpose), ['triage', 'triage_review']);
    assert.ok(preparationCalls.slice(-2).every(row => row.account === account));

    const vk = state.items.at(-2);
    const tiktok = state.items.at(-1);
    assert.equal(vk.platform, 'vk');
    assert.equal(tiktok.platform, 'tiktok');
    assert.ok(vk && tiktok);
    assert.equal((await http.api('/api/proposals', 'POST', {itemId: vk.id, expectedRevision: vk.revision, kind: 'hide', text: ''})).status, 400);
    assert.equal((await http.api('/api/proposals', 'POST', {itemId: tiktok.id, expectedRevision: tiktok.revision, kind: 'delete', text: ''})).status, 400);
    assert.equal((await http.api('/api/proposals', 'POST', {itemId: vk.id, expectedRevision: vk.revision, kind: 'delete', text: ''})).status, 200);
    assert.equal((await http.api('/api/proposals', 'POST', {itemId: tiktok.id, expectedRevision: tiktok.revision, kind: 'hide', text: ''})).status, 200);

    const batchItems = state.items.slice(0, batchSize);
    const assistantItems = batchItems.slice(0, Math.min(8, batchItems.length));
    const conversation = await http.api('/api/conversations', 'POST', {title: `${account} engine acceptance`, itemIds: assistantItems.map(item => item.id)});
    assert.equal(conversation.status, 200, JSON.stringify(conversation.value));
    const message = await http.api(`/api/conversations/${conversation.value.id}/messages`, 'POST', {
      text: 'Prepare the synthetic acceptance batch',
      itemIds: assistantItems.map(item => item.id),
    });
    assert.equal(message.status, 200, JSON.stringify(message.value));
    const assistantJob = await http.waitJob(message.value.jobId, exerciseRecovery ? 60000 : 20000);
    assert.equal(assistantJob.status, 'completed', JSON.stringify(assistantJob));
    state = await http.bootstrap();
    const generated = state.proposals.filter(proposal => proposal.prepareRunId === message.value.jobId);
    assert.equal(generated.length, assistantItems.length);
    const manual = [];
    for (const item of batchItems.slice(assistantItems.length)) {
      const proposal = await http.api('/api/proposals', 'POST', {
        itemId: item.id,
        expectedRevision: item.revision,
        kind: 'reply_and_close',
        text: `Synthetic approved reply for ${item.id}`,
      });
      assert.equal(proposal.status, 200, JSON.stringify(proposal.value));
      manual.push(proposal.value);
    }
    const batchProposals = [...generated, ...manual];
    assert.equal(batchProposals.length, batchItems.length);

    const exact = batchProposals.map(proposal => ({id: proposal.id, revision: proposal.revision}));
    const wrong = exact.map((proposal, index) => index === 0 ? {...proposal, revision: proposal.revision + 1} : proposal);
    assert.equal((await http.api('/api/approvals', 'POST', {proposals: wrong})).status, 409, 'non-exact approval was accepted');
    const approval = await http.api('/api/approvals', 'POST', {proposals: exact});
    assert.equal(approval.status, 200, JSON.stringify(approval.value));
    const execution = await http.api(`/api/approvals/${approval.value.id}/execute`, 'POST', {});
    assert.equal(execution.status, 200, JSON.stringify(execution.value));
    const executionJob = await http.waitJob(execution.value.jobId, exerciseRecovery ? 300000 : 30000);
    assert.equal(executionJob.status, 'completed', JSON.stringify(executionJob));
    assert.equal(executionJob.result.total, batchItems.length);
    assert.equal(executionJob.result.parallelism, batchItems.length);
    if (exerciseRecovery) assert.equal(executionJob.result.parallelism, 100, 'default engine parallelism was not 100');
    assert.equal(executionJob.result.known, batchItems.length);
    assert.equal(executionJob.result.unknown, 0);

    const probe = await (async () => {
      for (let attempt = 0; attempt < 100; attempt += 1) {
        try { return JSON.parse(await readFile(concurrency, 'utf8')); }
        catch { await sleep(20); }
      }
      throw Error('missing concurrency evidence');
    })();
    assert.ok(probe.maxActive > 1, `batch did not execute in parallel: ${JSON.stringify(probe)}`);
    assert.deepEqual(probe.violations, [], 'same item/conversation overlapped');
    state = await http.bootstrap();
    const operations = state.operations.filter(operation => batchProposals.some(proposal => proposal.id === operation.proposalId));
    assert.equal((await lines(effects)).length, batchItems.length, JSON.stringify(operations.map(operation => ({itemId: operation.itemId, status: operation.status, evidence: operation.evidence}))));
    assert.equal(operations.length, batchItems.length);
    for (const operation of operations) {
      assert.equal(operation.status, 'succeeded');
      assert.deepEqual(operation.action.readbackEvidence?.baselineReplyIds, [`old-official-${operation.action.itemId}`]);
      assert.equal(operation.evidence.results[0].evidence.strictEvidence, true);
    }

    const sharedConversation = await http.api('/api/conversations', 'POST', {
      title: `${account} same-conversation serialization`,
      itemIds: [vk.id, tiktok.id],
    });
    assert.equal(sharedConversation.status, 200, JSON.stringify(sharedConversation.value));
    const sharedMessage = await http.api(`/api/conversations/${sharedConversation.value.id}/messages`, 'POST', {
      text: 'Prepare both same-conversation recipients',
      itemIds: [vk.id, tiktok.id],
    });
    assert.equal(sharedMessage.status, 200, JSON.stringify(sharedMessage.value));
    const sharedAssistantJob = await http.waitJob(sharedMessage.value.jobId);
    assert.equal(sharedAssistantJob.status, 'completed', JSON.stringify(sharedAssistantJob));
    state = await http.bootstrap();
    const sharedGenerated = state.proposals.filter(proposal => [vk.id, tiktok.id].includes(proposal.itemId) && proposal.prepareRunId === sharedMessage.value.jobId);
    assert.equal(sharedGenerated.length, 2);
    const sharedApproval = await http.api('/api/approvals', 'POST', {
      proposals: sharedGenerated.map(proposal => ({id: proposal.id, revision: proposal.revision})),
    });
    assert.equal(sharedApproval.status, 200, JSON.stringify(sharedApproval.value));
    const sharedExecution = await http.api(`/api/approvals/${sharedApproval.value.id}/execute`, 'POST', {});
    assert.equal(sharedExecution.status, 200, JSON.stringify(sharedExecution.value));
    const sharedExecutionJob = await http.waitJob(sharedExecution.value.jobId, 30000);
    assert.equal(sharedExecutionJob.status, 'completed', JSON.stringify(sharedExecutionJob));
    assert.equal(sharedExecutionJob.result.total, 2);
    assert.equal(sharedExecutionJob.result.parallelism, 2);
    assert.equal(sharedExecutionJob.result.known, 2);
    state = await http.bootstrap();
    const sharedOperations = state.operations.filter(operation => sharedGenerated.some(proposal => proposal.id === operation.proposalId));
    assert.equal(sharedOperations.filter(operation => operation.status === 'succeeded').length, 1, JSON.stringify(sharedOperations));
    assert.equal(sharedOperations.filter(operation => operation.status === 'stale').length, 1, JSON.stringify(sharedOperations));
    const staleShared = sharedOperations.find(operation => operation.status === 'stale');
    const staleSharedItem = state.items.find(item => item.id === staleShared.itemId);
    const sameConversationDisposition = staleSharedItem.revision !== staleShared.target.revision
      ? 'stale-after-sibling-item-revision-change'
      : 'stale-after-sibling-review-fingerprint-change';
    assert.equal((await lines(effects)).length, batchItems.length + 1, 'stale same-conversation sibling reached execute');
    const sharedProbe = JSON.parse(await readFile(concurrency, 'utf8'));
    assert.deepEqual(sharedProbe.violations, [], 'same-conversation replies overlapped');

    const populatedExport = await http.api('/api/engine/export');
    assert.equal(populatedExport.status, 200);
    assert.equal(populatedExport.value.items.length, state.items.length);
    assert.equal(populatedExport.value.containsExecutableActions, false);
    for (const forbidden of ['proposals', 'approvals', 'operations', 'jobs']) assert.equal(Object.hasOwn(populatedExport.value, forbidden), false);

    const closed = state.items.find(item => item.id === batchItems[0].id);
    assert.equal(closed.providerStatus, 'closed');
    assert.equal((await http.api('/api/proposals', 'POST', {
      itemId: closed.id, expectedRevision: closed.revision, kind: 'reply_and_close', text: 'Late synthetic reply',
    })).status, 409);
    const lateReply = await http.api('/api/proposals', 'POST', {
      itemId: closed.id, expectedRevision: closed.revision, kind: 'reply_and_close', text: 'Late synthetic reply', allowClosedReply: true,
    });
    assert.equal(lateReply.status, 200, JSON.stringify(lateReply.value));
    assert.equal(lateReply.value.allowClosedReply, true);
    const lateApproval = await http.api('/api/approvals', 'POST', {
      proposals: [{id: lateReply.value.id, revision: lateReply.value.revision}],
    });
    assert.equal(lateApproval.status, 200, JSON.stringify(lateApproval.value));
    const beforeLateReply = (await lines(effects)).length;
    const lateExecution = await http.api(`/api/approvals/${lateApproval.value.id}/execute`, 'POST', {});
    assert.equal(lateExecution.status, 200, JSON.stringify(lateExecution.value));
    assert.equal((await http.waitJob(lateExecution.value.jobId)).status, 'completed');
    state = await http.bootstrap();
    assert.equal(state.operations.find(operation => operation.proposalId === lateReply.value.id)?.status, 'succeeded');
    assert.equal((await lines(effects)).length, beforeLateReply + 1, 'explicit follow-up was not dispatched exactly once');
    assert.equal((await http.api(`/api/approvals/${lateApproval.value.id}/execute`, 'POST', {})).status, 409);
    assert.equal((await lines(effects)).length, beforeLateReply + 1, 'consumed follow-up approval was replayed');

    if (exerciseRecovery) {
      state = await http.bootstrap();
      const staleSharedOperation = sharedOperations.find(operation => operation.status === 'stale');
      const failureItem = state.items.find(item => item.id === staleSharedOperation.itemId);
      const failureProposal = await http.api('/api/proposals', 'POST', {
        itemId: failureItem.id,
        expectedRevision: failureItem.revision,
        kind: 'reply_and_close',
        text: 'Synthetic confirmed non-mutation',
        allowClosedReply: true,
      });
      assert.equal(failureProposal.status, 200, JSON.stringify(failureProposal.value));
      const failureApproval = await http.api('/api/approvals', 'POST', {
        proposals: [{id: failureProposal.value.id, revision: failureProposal.value.revision}],
      });
      assert.equal(failureApproval.status, 200, JSON.stringify(failureApproval.value));
      await writeFile(scenario, JSON.stringify({itemCount, executeDelayMs: 40, executeFailure: 'not-attempted'}));
      const effectsBeforeFailure = (await lines(effects)).length;
      const traceBeforeFailure = (await lines(trace)).length;
      const failureExecution = await http.api(`/api/approvals/${failureApproval.value.id}/execute`, 'POST', {});
      assert.equal(failureExecution.status, 200, JSON.stringify(failureExecution.value));
      assert.equal((await http.waitJob(failureExecution.value.jobId)).status, 'completed');
      state = await http.bootstrap();
      const failed = state.operations.find(operation => operation.proposalId === failureProposal.value.id);
      assert.equal(failed.status, 'failed', JSON.stringify(failed));
      assert.equal(failed.executeReceipt.results[0].mutationOutcome, 'not-attempted');
      assert.equal((await lines(effects)).length, effectsBeforeFailure, 'confirmed non-mutation produced an external effect');
      const failureTrace = (await lines(trace)).slice(traceBeforeFailure).filter(row =>
        row.request.actions?.some(action => action.actionId === failed.action.actionId)
        || (row.operation === 'context' && row.request.itemId === failed.action.itemId));
      assert.deepEqual(failureTrace.map(row => row.operation), ['context', 'execute'], 'confirmed failure incorrectly performed readback');

      state = await http.bootstrap();
      const recoveryItem = state.items.find(item => item.id === staleSharedOperation.itemId);
      const proposal = await http.api('/api/proposals', 'POST', {
        itemId: recoveryItem.id,
        expectedRevision: recoveryItem.revision,
        kind: 'reply_and_close',
        text: 'Unknown synthetic reply',
      });
      assert.equal(proposal.status, 200, JSON.stringify(proposal.value));
      const recoveryApproval = await http.api('/api/approvals', 'POST', {proposals: [{id: proposal.value.id, revision: proposal.value.revision}]});
      assert.equal(recoveryApproval.status, 200);
      await writeFile(scenario, JSON.stringify({itemCount, executeDelayMs: 80, executeUnknown: true, readbackUnknown: true}));
      const unknownExecution = await http.api(`/api/approvals/${recoveryApproval.value.id}/execute`, 'POST', {});
      assert.equal(unknownExecution.status, 200);
      assert.equal((await http.waitJob(unknownExecution.value.jobId)).status, 'completed');
      state = await http.bootstrap();
      const unknown = state.operations.find(operation => operation.proposalId === proposal.value.id);
      assert.ok(unknown);
      assert.equal(unknown.status, 'unknown');
      assert.deepEqual(unknown.action.readbackEvidence?.baselineReplyIds, [`old-official-${unknown.action.itemId}`]);
      assert.equal(unknown.executeReceipt.account, account);
      assert.equal(unknown.executeReceipt.results[0].actionId, unknown.action.actionId);
      const unresolvedItem = state.items.find(item => item.id === unknown.itemId);
      const blockedFollowup = await http.api('/api/proposals', 'POST', {
        itemId: unresolvedItem.id, expectedRevision: unresolvedItem.revision,
        kind: 'reply_and_close', text: 'Must not bypass unresolved send', allowClosedReply: true,
      });
      assert.equal(blockedFollowup.status, 200, JSON.stringify(blockedFollowup.value));
      const blockedFollowupApproval = await http.api('/api/approvals', 'POST', {
        proposals: [{id: blockedFollowup.value.id, revision: blockedFollowup.value.revision}],
      });
      assert.equal(blockedFollowupApproval.status, 200, JSON.stringify(blockedFollowupApproval.value));
      const beforeBlockedFollowup = (await lines(effects)).length;
      assert.equal((await http.api(`/api/approvals/${blockedFollowupApproval.value.id}/execute`, 'POST', {})).status, 409);
      assert.equal((await lines(effects)).length, beforeBlockedFollowup, 'follow-up bypassed UNKNOWN quarantine');

      const blockedItem = state.items.at(-3);
      assert.equal(blockedItem.conversationKey, recoveryItem.conversationKey);
      const blockedProposal = await http.api('/api/proposals', 'POST', {
        itemId: blockedItem.id,
        expectedRevision: blockedItem.revision,
        kind: 'reply_and_close',
        text: 'Must be quarantined behind UNKNOWN',
        allowClosedReply: true,
      });
      assert.equal(blockedProposal.status, 200, JSON.stringify(blockedProposal.value));
      const blockedApproval = await http.api('/api/approvals', 'POST', {
        proposals: [{id: blockedProposal.value.id, revision: blockedProposal.value.revision}],
      });
      assert.equal(blockedApproval.status, 200, JSON.stringify(blockedApproval.value));
      const blockedExecution = await http.api(`/api/approvals/${blockedApproval.value.id}/execute`, 'POST', {});
      assert.equal(blockedExecution.status, 200, JSON.stringify(blockedExecution.value));
      assert.equal((await http.waitJob(blockedExecution.value.jobId)).status, 'completed');
      state = await http.bootstrap();
      const blocked = state.operations.find(operation => operation.proposalId === blockedProposal.value.id);
      assert.equal(blocked.status, 'stale', JSON.stringify(blocked));
      assert.equal(blocked.evidence.blockedByOperationId, unknown.id);
      assert.equal(blocked.evidence.providerCallAttempted, false);
      const attemptsBeforeRestart = (await lines(effects)).length;

      await terminate(server.child);
      const wrong = await launch({account: 'baw-russia', directory, scenario, effects, trace, concurrency, expectFailure: true});
      assert.equal(wrong.failed, true);
      assert.match(wrong.stderr(), /another account|separate data directory|does not match configured account/i);

      await writeFile(scenario, JSON.stringify({itemCount, executeDelayMs: 80}));
      server = await launch({account, directory, scenario, effects, trace, concurrency});
      http = client(server);
      state = await http.bootstrap();
      const preserved = state.operations.find(operation => operation.id === unknown.id);
      assert.equal(preserved.status, 'unknown');
      assert.deepEqual(preserved.executeReceipt, unknown.executeReceipt, 'execute receipt was not durable across restart');
      assert.equal((await lines(effects)).length, attemptsBeforeRestart, 'restart resent UNKNOWN operation');
      const reconciliation = await http.api(`/api/operations/${unknown.id}/reconcile`, 'POST', {});
      assert.equal(reconciliation.status, 200, JSON.stringify(reconciliation.value));
      assert.equal((await http.waitJob(reconciliation.value.jobId)).status, 'completed');
      state = await http.bootstrap();
      assert.equal(state.operations.find(operation => operation.id === unknown.id).status, 'succeeded');
      assert.equal((await lines(effects)).length, attemptsBeforeRestart, 'reconciliation resent UNKNOWN operation');
    }

    const traceRows = await lines(trace);
    assert.ok(traceRows.some(row => row.operation === 'caps' && row.account === account));
    assert.ok(traceRows.some(row => row.operation === 'scan' && row.account === account));
    assert.ok(traceRows.some(row => row.operation === 'assistant' && row.account === account));
    assert.ok(traceRows.some(row => row.operation === 'readback' && row.account === account));
    return {account, directory: root, effects: (await lines(effects)).length, maxParallel: probe.maxActive, sameConversationDisposition};
  } finally {
    await terminate(server.child);
  }
}

try {
  const results = [];
  results.push(await runAccount('likeavto', {exerciseRecovery: true}));
  results.push(await runAccount('baw-russia'));
  const cli = await runCliAcceptance();
  console.log(JSON.stringify({
    ok: true,
    test: 'engine-http-acceptance',
    externalEffects: 'loopback fake bridge only',
    checks: [
      'single-account-binding', 'both-account-lifecycles', 'engine-status', 'engine-job-status',
      'read-only-capabilities', 'read-only-scan', 'non-executable-export', 'engine-prepare-first-and-review', 'assistant-generation',
      'exact-approval', 'platform-action-gates', 'closed-reply-opt-in', 'default-100-parallel-batch',
      'item-conversation-serialization', 'pre-send-reply-baseline', 'receipt-reply-baseline',
      'strict-readback', 'confirmed-nonmutation-no-readback', 'conversation-quarantine',
      'wrong-account-startup-rejection', 'unknown-restart-preservation', 'reconcile-without-resend',
      'actual-cli-queue-stage-upgrade', 'actual-cli-repeat-resume-noop',
    ],
    accounts: results,
    cli,
  }));
} finally {
  await Promise.all([...children].map(terminate));
}

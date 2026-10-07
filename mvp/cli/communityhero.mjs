#!/usr/bin/env node
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { CommunityHeroClient, CliError, DEFAULT_REQUEST_TIMEOUT_MS, LOCAL_ADMISSION_TIMEOUT_MS, checkpointError, executeAdmissionResult, executeRejection, localAdmissionPayloadHash, loadSessionCookie, settleMaterialsImport, waitForJob } from './client.mjs';
import { generateProposals, readCheckpoint, runEditorialReview, runScan, runWorkflow } from './workflow.mjs';
import { runQueue } from './queue.mjs';
import { readReviewedReferences, runBulk } from './bulk.mjs';
import { runMaintenance, maintenanceError } from './maintenance.mjs';
import { nativeProgress, resultExitCode } from './read-observer.mjs';

function usage() {
  return `CommunityHero headless CLI

Usage: node mvp/cli/communityhero.mjs <command> --account LikeAvto [options]

Commands:
  maintenance --plan PATH --plan-sha SHA256 --action status|register-target|begin|checkpoint
              --invocation ID [--owner-epoch N] [--request-timeout-ms N]
              Native prestop only; account is the exact plan slug; timeout defaults to 600000 ms.
  health                         Check server liveness
  status                         Show engine binding and workspace summary
  capabilities                   Show provider capabilities
  search --query TEXT            Search current workspace comments
  export                         Export the current source-only engine snapshot
  backup                         Ask the Rust authority for a workspace backup
  materials [--wait]             Import account policy/materials through Rust
  history                        Show durable jobs and operation ledger
  sync [--mode open|closed] [--cursor VALUE] [--wait]
  context-refresh --item LOCAL_ID [--wait]  Refresh one saved target and branch
  context-refresh --item LOCAL_ID --job JOB_ID [--wait]  Inspect/poll without reposting
  scan --out PATH|--checkpoint PATH [--statuses CSV] [--resume PATH]
       [--page-size N] [--max-pages N] [--max-items N] [--max-elapsed-ms N]
  proposals [--status draft]     List proposal metadata
  inspect --proposal|--item|--approval|--operation ID
  prepare --item ID [...] [--checkpoint PATH] | --resume PATH
          [--instruction TEXT|--instruction-file PATH]
  propose --item ID --kind close|reply_and_close|hide|delete [--text TEXT|--text-file PATH]
          [--allow-closed-reply]
  approve --proposal ID@REV [...] [--execute] [--wait]
  editorial-review --proposal ID@REV [...] --checkpoint PATH | --resume PATH
  execute --approval ID --request-id ID [--wait]
          [--reevaluate --evaluation-id REJECTED_ID]  Requires the exact durable rejection
  readback [--operation ID]      Inspect operation outcomes (no mutation)
  reconcile --operation ID [--wait]
  run --item ID [...] [--instruction TEXT|--instruction-file PATH]
      [--autonomous] [--execute] [--workflow-mode prepare_review_only] [--checkpoint PATH] [--resume PATH]
  queue --checkpoint PATH [--resume PATH] [--batch-size N] [--max-cycles N]
        [--autonomous] [--execute] [--workflow-mode prepare_review_only] [--instruction TEXT|--instruction-file PATH]
  bulk --proposals-file PATH --checkpoint PATH | --resume PATH
       [--batch-size N<=100] [--execute] [--partial-admission]
       [--continuation-policy stop-on-mixed|continue-independent]

Global: --base-url URL (default http://127.0.0.1:4186), --account NAME,
--session-file PATH, --poll-ms N, --max-polls N (optional observation limit),
--request-timeout-ms N (1..3600000; default ${DEFAULT_REQUEST_TIMEOUT_MS}, allowlisted loopback admission POST ${LOCAL_ADMISSION_TIMEOUT_MS}).
Waiting follows the existing job until terminal status or Ctrl-C; it never resends a POST.
Durably admitted groups emit prepare.ready and update the checkpoint while other groups run.
Autonomous run drains ready batches through its existing approval/execution flags.
Session secrets are accepted only from --session-file or COMMUNITYHERO_SESSION.`;
}

function parse(argv) {
  const options = { item: [], proposal: [] };
  const positional = [];
  const boolean = new Set(['wait', 'execute', 'autonomous', 'reevaluate', 'allow-closed-reply', 'partial-admission', 'help']);
  const repeat = new Set(['item', 'proposal']);
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith('--')) { positional.push(token); continue; }
    const [raw, inline] = token.slice(2).split('=', 2);
    if (boolean.has(raw)) { options[raw] = inline === undefined ? true : inline !== 'false'; continue; }
    const value = inline ?? argv[++i];
    if (value === undefined || value.startsWith('--')) throw new CliError(`Missing value for --${raw}`, { code: 'USAGE' });
    if (repeat.has(raw)) options[raw].push(value); else options[raw] = value;
  }
  return { command: positional[0], options };
}

function numberOption(value, fallback, name) {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) throw new CliError(`--${name} must be a positive integer`, { code: 'USAGE' });
  return parsed;
}

function safeSummary(snapshot) {
  const count = (key, status) => (snapshot[key] || []).filter(row => !status || row.status === status).length;
  return {
    account: snapshot.account, workspaceVersion: snapshot.workspaceVersion, operator: snapshot.operator,
    sync: snapshot.sync, items: count('items'), draftProposals: count('proposals', 'draft'),
    runningJobs: (snapshot.jobs || []).filter(row => ['running', 'queued'].includes(row.status)).length,
    operations: Object.fromEntries(['dispatching', 'unknown', 'stale', 'succeeded'].map(status => [status, count('operations', status)])),
    externalWritesEnabled: snapshot.settings?.externalWritesEnabled === true,
    progress: nativeProgress(snapshot.progress)
  };
}

function exactRef(value) {
  const match = /^(.*)@(\d+)$/.exec(value);
  if (!match || !match[1]) throw new CliError(`Proposal reference must be ID@REV: ${value}`, { code: 'USAGE' });
  return { id: match[1], revision: Number(match[2]) };
}

function output(value) { process.stdout.write(`${JSON.stringify(value, null, 2)}\n`); }
function outcome(value) { process.exitCode = resultExitCode(value); output(value); }
function progress(value) { process.stderr.write(`${JSON.stringify({ at: new Date().toISOString(), ...value })}\n`); }

async function textOption(options) {
  if (options.text !== undefined && options['text-file']) throw new CliError('Use one of --text or --text-file', { code: 'USAGE' });
  return options['text-file'] ? readFile(options['text-file'], 'utf8') : options.text;
}

async function instructionOption(options) {
  if (options.instruction !== undefined && options['instruction-file']) throw new CliError('Use one of --instruction or --instruction-file', { code: 'USAGE' });
  return options['instruction-file'] ? readFile(options['instruction-file'], 'utf8') : options.instruction;
}

async function writeArtifact(path, value) {
  const target = resolve(path); await mkdir(dirname(target), { recursive: true });
  const tmp = `${target}.${process.pid}-${Date.now()}.tmp`;
  await writeFile(tmp, `${JSON.stringify(value, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  await rename(tmp, target);
}

async function main() {
  const { command, options } = parse(process.argv.slice(2));
  if (!command || options.help) { process.stdout.write(`${usage()}\n`); return; }
  const cookie = await loadSessionCookie({ sessionFile: options['session-file'] });
  if (command === 'maintenance') {
    try {
      const result = await runMaintenance({
        planPin: { path: options.plan, sha256: options['plan-sha'] },
        action: options.action, invocationId: options.invocation, account: options.account,
        baseUrl: options['base-url'], cookie,
        ownerEpoch: options['owner-epoch'] === undefined ? undefined : numberOption(options['owner-epoch'], undefined, 'owner-epoch'),
        timeoutMs: numberOption(options['request-timeout-ms'], undefined, 'request-timeout-ms')
      });
      output(result); if (result.status === 'unresolved') process.exitCode = 4;
    } catch (error) {
      output({ status: 'refused-or-incomplete', ...maintenanceError(error), stopAuthorized: false });
      process.exitCode = 1;
    }
    return;
  }
  const pollMs = numberOption(options['poll-ms'], 1000, 'poll-ms');
  const maxPolls = numberOption(options['max-polls'], undefined, 'max-polls');
  const controller = new AbortController();
  const client = new CommunityHeroClient({ baseUrl: options['base-url'], account: options.account, cookie, timeoutMs: numberOption(options['request-timeout-ms'], undefined, 'request-timeout-ms'), signal: controller.signal });
  process.once('SIGINT', () => controller.abort());
  const wait = async (jobId, waitOptions = {}) => waitForJob(client, jobId, { pollMs, maxPolls, signal: controller.signal,
    ...waitOptions, onPoll: job => progress({ event: 'job.poll', jobId, status: job.status }) });

  if (command === 'health') return output(await client.health());
  if (command === 'status') {
    const [engineResult, snapshot] = await Promise.all([client.engineStatus().catch(error => error.status === 403 ? null : Promise.reject(error)), client.bootstrap()]);
    return output({ engine: engineResult, workspace: safeSummary(snapshot) });
  }
  if (command === 'capabilities' || command === 'caps') return output(await client.request('/api/engine/capabilities', { mutation: false }));
  if (command === 'search') {
    if (!options.query) throw new CliError('search requires --query', { code: 'USAGE' });
    await client.bootstrap(); return output(await client.request(`/api/items/search?q=${encodeURIComponent(options.query)}`, { mutation: false }));
  }
  if (command === 'export') { await client.engineStatus(); return output(await client.request('/api/engine/export', { mutation: false })); }
  if (command === 'backup') return output(await client.mutate('/api/backup', {}));
  if (command === 'materials') {
    const launched = await client.importMaterials();
    progress(launched.jobId ? { event: 'materials.started', jobId: launched.jobId } : { event: 'materials.legacy-import-suppressed', authority: launched.authority });
    if (!launched.jobId || options.wait) {
      const settled = await settleMaterialsImport(client, launched, { pollMs, maxPolls, signal: controller.signal, onPoll: job => progress({ event: 'job.poll', jobId: job.id, status: job.status }) });
      return output(launched.jobId ? settled : launched);
    }
    return output(launched);
  }
  if (command === 'history') {
    const snapshot = await client.bootstrap();
    return output({ account: snapshot.account, historyMetadata: snapshot.historyMetadata, jobs: snapshot.jobs || [], operations: snapshot.operations || [] });
  }
  if (command === 'sync') {
    const body = {};
    if (options.mode) body.mode = options.mode;
    if (options.cursor) body.cursor = options.cursor;
    const result = await client.sync(body); progress({ event: 'sync.started', jobId: result.jobId });
    return output(options.wait ? await wait(result.jobId) : result);
  }
  if (command === 'context-refresh') {
    if (options.item.length !== 1) throw new CliError('context-refresh requires exactly one --item', { code: 'USAGE' });
    const itemId = options.item[0];
    const launched = options.job ? null : await client.refreshItemContext(itemId);
    if (launched) progress({ event: 'context-refresh.started', itemId, jobId: launched.jobId, deduplicated: launched.deduplicated });
    const jobId = options.job || launched.jobId;
    if (options.job) {
      const job = await client.contextRefreshJob(itemId, jobId);
      if (!options.wait) return output({ jobId, itemId, status: job.status, result: job.result || null });
    } else if (!options.wait) return output(launched);
    const settled = await waitForJob(client, jobId, { pollMs, maxPolls, signal: controller.signal,
      includeSnapshot: false, onPoll: job => progress({ event: 'job.poll', jobId: job.id, status: job.status }) });
    return output({ ...(launched || { jobId, itemId }), status: settled.job.status, result: settled.job.result || null });
  }
  if (command === 'scan') {
    if (!options.out && !options.checkpoint) throw new CliError('scan requires --out or --checkpoint so coverage and resume are durable', { code: 'USAGE' });
    const request = {
      pageSize: numberOption(options['page-size'], 100, 'page-size'),
      maxPages: numberOption(options['max-pages'], 8, 'max-pages'),
      maxItems: numberOption(options['max-items'], 800, 'max-items'),
      maxElapsedMs: numberOption(options['max-elapsed-ms'], 30_000, 'max-elapsed-ms')
    };
    if (options.statuses) request.statuses = [...new Set(options.statuses.split(',').map(value => value.trim()).filter(Boolean))];
    let checkpoint = null;
    if (options.resume) {
      const raw = JSON.parse(await readFile(options.resume, 'utf8'));
      if (raw?.kind === 'communityhero-provider-scan') checkpoint = raw;
      else request.resume = raw?.nextResume ?? raw?.result?.nextResume ?? raw;
    } else if (options.checkpoint) {
      try { await readFile(options.checkpoint, 'utf8'); throw new CliError('Checkpoint already exists; use --resume explicitly or choose another path', { code: 'USAGE' }); }
      catch (error) { if (error.code !== 'ENOENT' && error.code !== 'USAGE') throw error; if (error.code === 'USAGE') throw error; }
    }
    const result = await runScan(client, request, { checkpointPath: options.checkpoint, checkpoint, pollMs, maxPolls, signal: controller.signal, onProgress: progress });
    if (options.out) await writeArtifact(options.out, result);
    return output(result);
  }
  if (command === 'proposals') {
    const snapshot = await client.bootstrap();
    const rows = (snapshot.proposals || []).filter(row => !options.status || row.status === options.status).map(({ id, revision, itemId, kind, text, status, createdAt }) => ({ id, revision, itemId, kind, text, status, createdAt }));
    return output({ account: snapshot.account, proposals: rows });
  }
  if (command === 'inspect') {
    const selectors = ['proposal', 'item', 'approval', 'operation'].flatMap(kind =>
      (Array.isArray(options[kind]) ? options[kind] : options[kind] === undefined ? [] : [options[kind]])
        .map(id => ({ kind, id })));
    if (selectors.length !== 1 || !selectors[0].id.trim()) throw new CliError('inspect requires exactly one entity selector', { code: 'USAGE' });
    const { kind, id } = selectors[0];
    const snapshot = await client.bootstrap();
    const row = (snapshot[`${kind}s`] || []).find(value => value.id === id);
    if (!row) throw new CliError(`${kind} ${id} not found`, { code: 'NOT_FOUND' });
    return output({ account: snapshot.account, [kind]: row });
  }
  if (command === 'prepare') {
    if (options.resume && options.checkpoint && resolve(options.resume) !== resolve(options.checkpoint)) throw new CliError('Resume must update its original checkpoint', { code: 'USAGE' });
    const checkpoint = options.resume ? await readCheckpoint(options.resume) : null;
    if (checkpoint && options.item.length && JSON.stringify([...new Set(options.item)]) !== JSON.stringify(checkpoint.itemIds))
      throw new CliError('Resume item selection differs from its checkpoint', { code: 'USAGE' });
    const state = await generateProposals(client, checkpoint?.itemIds || options.item, { checkpoint, instruction: await instructionOption(options), checkpointPath: options.checkpoint || options.resume, pollMs, maxPolls, signal: controller.signal, onProgress: progress });
    return output({ mode: 'prepare-only', checkpoint: state });
  }
  if (command === 'propose') {
    if (options.item.length !== 1) throw new CliError('propose requires one --item', { code: 'USAGE' });
    const snapshot = await client.bootstrap(); const item = (snapshot.items || []).find(row => row.id === options.item[0]);
    if (!item) throw new CliError(`Item ${options.item[0]} not found`, { code: 'ITEM_NOT_FOUND' });
    const kind = options.kind || 'reply_and_close'; const supplied = await textOption(options); const text = kind === 'close' ? '' : (supplied ?? item.draft ?? '');
    return output(await client.createProposal({ itemId: item.id, expectedRevision: item.revision, kind, text, ...(options['allow-closed-reply'] ? { allowClosedReply: true } : {}) }));
  }
  if (command === 'editorial-review') {
    if (!options.resume && !options.proposal.length) throw new CliError('editorial-review requires --proposal or --resume', { code: 'USAGE' });
    return output(await runEditorialReview(client, options.proposal.map(exactRef), { checkpointPath: options.checkpoint,
      resumePath: options.resume, pollMs, maxPolls, signal: controller.signal, onProgress: progress }));
  }
  if (command === 'approve') {
    if (!options.proposal.length) throw new CliError('approve requires --proposal ID@REV', { code: 'USAGE' });
    const approval = await client.createApproval(options.proposal.map(exactRef));
    if (!options.execute) return output(approval);
    const launched = await client.execute(approval.id); progress({ event: 'execute.started', approvalId: approval.id, jobId: launched.jobId });
    return output(options.wait ? await wait(launched.jobId) : { approval, ...launched });
  }
  if (command === 'execute') {
    if (!options.approval || !/^[A-Za-z0-9_-]{1,160}$/u.test(options['request-id'] || ''))
      throw new CliError('execute requires --approval and exact --request-id', { code: 'USAGE' });
    const requestId = options['request-id']; let reevaluate;
    if (options.reevaluate) {
      if (!/^[A-Za-z0-9_-]{1,160}$/u.test(options['evaluation-id'] || ''))
        throw new CliError('Re-evaluation requires the rejected exact --evaluation-id', { code: 'USAGE' });
      const receipt = executeRejection(await client.localAdmission('execute', requestId), {
        approvalId: options.approval, requestId, payloadHash: localAdmissionPayloadHash({ approvalId: options.approval }), account: client.account });
      if (!receipt.reevaluationAvailable) throw new CliError('Local re-evaluation budget is exhausted', { code: 'REJECTED_LOCAL_ADMISSION', details: receipt });
      if (receipt.evaluationId !== options['evaluation-id']) throw new CliError('Re-evaluation must name the latest rejected evaluation', { code: 'USAGE' });
      reevaluate = { evaluationId: receipt.evaluationId, receiptSha256: receipt.receiptSha256 };
    } else if (options['evaluation-id']) throw new CliError('--evaluation-id requires --reevaluate', { code: 'USAGE' });
    const launched = executeAdmissionResult(await client.execute(options.approval, requestId, { reevaluate }), options.approval, requestId);
    return output(options.wait ? await wait(launched.jobId) : launched);
  }
  if (command === 'readback') {
    const snapshot = await client.bootstrap();
    const rows = (snapshot.operations || []).filter(row => !options.operation || row.id === options.operation);
    return output({ account: snapshot.account, operations: rows });
  }
  if (command === 'reconcile') {
    if (!options.operation) throw new CliError('reconcile requires --operation', { code: 'USAGE' });
    const snapshot = await client.bootstrap(); const op = (snapshot.operations || []).find(row => row.id === options.operation);
    if (!op) throw new CliError(`Operation ${options.operation} not found`, { code: 'NOT_FOUND' });
    if (op.status !== 'unknown') throw new CliError(`Operation ${op.id} is ${op.status}, not unknown`, { code: 'STALE_OR_CONFLICT' });
    const launched = await client.reconcile(op.id);
    return output(options.wait ? await wait(launched.jobId, { includeSnapshot: false }) : launched);
  }
  if (command === 'bulk') {
    if (!options.resume && !options['proposals-file']) throw new CliError('bulk requires --proposals-file or --resume', { code: 'USAGE' });
    const references = options['proposals-file'] ? await readReviewedReferences(options['proposals-file']) : [];
    return outcome(await runBulk(client, references, {
      checkpointPath: options.checkpoint, resumePath: options.resume,
      batchSize: numberOption(options['batch-size'], 100, 'batch-size'), execute: options.execute,
      ...(options['partial-admission'] === undefined ? {} : { partialAdmission: options['partial-admission'] }),
      ...(options['continuation-policy'] === undefined ? {} : { continuationPolicy: options['continuation-policy'] }),
      pollMs, maxPolls, signal: controller.signal, onProgress: progress
    }));
  }
  if (command === 'queue') {
    const result = await runQueue(client, {
      checkpointPath: options.checkpoint, resumePath: options.resume,
      batchSize: numberOption(options['batch-size'], 60, 'batch-size'),
      maxCycles: numberOption(options['max-cycles'], 1000, 'max-cycles'),
      instruction: await instructionOption(options), autonomous: options.autonomous, execute: options.execute, workflowMode: options['workflow-mode'],
      pollMs, maxPolls, signal: controller.signal, onProgress: progress
    });
    return outcome(result);
  }
  if (command === 'run' || command === 'drain') {
    if (!options.resume && !options.item.length) throw new CliError('run requires --item or --resume', { code: 'USAGE' });
    const result = await runWorkflow(client, options.item, { checkpointPath: options.checkpoint, resumePath: options.resume, instruction: await instructionOption(options), autonomous: options.autonomous, execute: options.execute, workflowMode: options['workflow-mode'], reconcileUnknown: command === 'drain', pollMs, maxPolls, signal: controller.signal, onProgress: progress });
    return outcome(result);
  }
  throw new CliError(`Unknown command: ${command}`, { code: 'USAGE' });
}

main().catch(error => {
  const value = error instanceof CliError ? error : new CliError(error.message || String(error), { code: 'UNEXPECTED' });
  process.stderr.write(`${JSON.stringify({ error: checkpointError(value) })}\n`);
  process.exitCode = value.code === 'USAGE' ? 2 : ['UNKNOWN_MUTATION_OUTCOME', 'REJECTED_LOCAL_ADMISSION', 'NETWORK_ERROR', 'READ_TIMEOUT', 'INVALID_RESPONSE', 'POLL_LIMIT', 'JOB_NOT_FOUND', 'INVALID_REVIEW_COVERAGE', 'INCOMPLETE_OPERATION_COVERAGE', 'INVALID_EXECUTION_CLOSURE', 'INVALID_PREPARE_JOB'].includes(value.code)
    || value.code === 'HTTP_ERROR' && value.status >= 500 ? 4 : value.code === 'STOPPED' ? 130 : 1;
});

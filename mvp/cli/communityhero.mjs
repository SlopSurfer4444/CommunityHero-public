#!/usr/bin/env node
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { CommunityHeroClient, CliError, loadSessionCookie, settleMaterialsImport, waitForJob } from './client.mjs';
import { generateProposals, readCheckpoint, runScan, runWorkflow } from './workflow.mjs';
import { runQueue } from './queue.mjs';

function usage() {
  return `CommunityHero headless CLI

Usage: node mvp/cli/communityhero.mjs <command> --account LikeAvto [options]

Commands:
  health                         Check server liveness
  status                         Show engine binding and workspace summary
  capabilities                   Show provider capabilities
  search --query TEXT            Search current workspace comments
  export                         Export the current source-only engine snapshot
  backup                         Ask the Rust authority for a workspace backup
  materials [--wait]             Import account policy/materials through Rust
  history                        Show durable jobs and operation ledger
  sync [--mode open|closed] [--cursor VALUE] [--wait]
  scan --out PATH|--checkpoint PATH [--statuses CSV] [--resume PATH]
       [--page-size N] [--max-pages N] [--max-items N] [--max-elapsed-ms N]
  proposals [--status draft]     List proposal metadata
  inspect --proposal|--item|--approval|--operation ID
  prepare --item ID [...] [--instruction TEXT|--instruction-file PATH]
  propose --item ID --kind close|reply_and_close|hide|delete [--text TEXT|--text-file PATH]
          [--allow-closed-reply]
  approve --proposal ID@REV [...] [--execute] [--wait]
  execute --approval ID [--wait]
  readback [--operation ID]      Inspect operation outcomes (no mutation)
  reconcile --operation ID [--wait]
  run --item ID [...] [--instruction TEXT|--instruction-file PATH]
      [--autonomous] [--execute] [--checkpoint PATH] [--resume PATH]
  queue --checkpoint PATH [--resume PATH] [--batch-size N] [--max-cycles N]
        [--autonomous] [--execute] [--instruction TEXT|--instruction-file PATH]

Global: --base-url URL (default http://127.0.0.1:4186), --account NAME,
--session-file PATH, --poll-ms N, --max-polls N, --request-timeout-ms N.
Session secrets are accepted only from --session-file or COMMUNITYHERO_SESSION.`;
}

function parse(argv) {
  const options = { item: [], proposal: [] };
  const positional = [];
  const boolean = new Set(['wait', 'execute', 'autonomous', 'allow-closed-reply', 'help']);
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
    externalWritesEnabled: snapshot.settings?.externalWritesEnabled === true
  };
}

function exactRef(value) {
  const match = /^(.*)@(\d+)$/.exec(value);
  if (!match || !match[1]) throw new CliError(`Proposal reference must be ID@REV: ${value}`, { code: 'USAGE' });
  return { id: match[1], revision: Number(match[2]) };
}

function output(value) { process.stdout.write(`${JSON.stringify(value, null, 2)}\n`); }
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
  const pollMs = numberOption(options['poll-ms'], 1000, 'poll-ms');
  const maxPolls = numberOption(options['max-polls'], 120, 'max-polls');
  const client = new CommunityHeroClient({ baseUrl: options['base-url'], account: options.account, cookie, timeoutMs: numberOption(options['request-timeout-ms'], 15_000, 'request-timeout-ms') });
  const controller = new AbortController();
  process.once('SIGINT', () => controller.abort());
  const wait = async jobId => waitForJob(client, jobId, { pollMs, maxPolls, signal: controller.signal, onPoll: job => progress({ event: 'job.poll', jobId, status: job.status }) });

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
    const kinds = ['proposal', 'item', 'approval', 'operation'].filter(key => options[key]);
    if (kinds.length !== 1) throw new CliError('inspect requires exactly one entity selector', { code: 'USAGE' });
    const snapshot = await client.bootstrap(); const id = Array.isArray(options[kinds[0]]) ? options[kinds[0]][0] : options[kinds[0]];
    const key = `${kinds[0]}s`; const row = (snapshot[key] || []).find(value => value.id === id);
    if (!row) throw new CliError(`${kinds[0]} ${id} not found`, { code: 'NOT_FOUND' });
    return output({ account: snapshot.account, [kinds[0]]: row });
  }
  if (command === 'prepare') {
    const state = await generateProposals(client, options.item, { instruction: await instructionOption(options), checkpointPath: options.checkpoint, pollMs, maxPolls, signal: controller.signal, onProgress: progress });
    return output({ mode: 'prepare-only', checkpoint: state });
  }
  if (command === 'propose') {
    if (options.item.length !== 1) throw new CliError('propose requires one --item', { code: 'USAGE' });
    const snapshot = await client.bootstrap(); const item = (snapshot.items || []).find(row => row.id === options.item[0]);
    if (!item) throw new CliError(`Item ${options.item[0]} not found`, { code: 'ITEM_NOT_FOUND' });
    const kind = options.kind || 'reply_and_close'; const supplied = await textOption(options); const text = kind === 'close' ? '' : (supplied ?? item.draft ?? '');
    return output(await client.createProposal({ itemId: item.id, expectedRevision: item.revision, kind, text, ...(options['allow-closed-reply'] ? { allowClosedReply: true } : {}) }));
  }
  if (command === 'approve') {
    if (!options.proposal.length) throw new CliError('approve requires --proposal ID@REV', { code: 'USAGE' });
    const approval = await client.createApproval(options.proposal.map(exactRef));
    if (!options.execute) return output(approval);
    const launched = await client.execute(approval.id); progress({ event: 'execute.started', approvalId: approval.id, jobId: launched.jobId });
    return output(options.wait ? await wait(launched.jobId) : { approval, ...launched });
  }
  if (command === 'execute') {
    if (!options.approval) throw new CliError('execute requires --approval', { code: 'USAGE' });
    const launched = await client.execute(options.approval);
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
    return output(options.wait ? await wait(launched.jobId) : launched);
  }
  if (command === 'queue') {
    const result = await runQueue(client, {
      checkpointPath: options.checkpoint, resumePath: options.resume,
      batchSize: numberOption(options['batch-size'], 60, 'batch-size'),
      maxCycles: numberOption(options['max-cycles'], 1000, 'max-cycles'),
      instruction: await instructionOption(options), autonomous: options.autonomous, execute: options.execute,
      pollMs, maxPolls, signal: controller.signal, onProgress: progress
    });
    return output(result);
  }
  if (command === 'run' || command === 'drain') {
    if (!options.resume && !options.item.length) throw new CliError('run requires --item or --resume', { code: 'USAGE' });
    const result = await runWorkflow(client, options.item, { checkpointPath: options.checkpoint, resumePath: options.resume, instruction: await instructionOption(options), autonomous: options.autonomous, execute: options.execute, reconcileUnknown: command === 'drain', pollMs, maxPolls, signal: controller.signal, onProgress: progress });
    return output(result);
  }
  throw new CliError(`Unknown command: ${command}`, { code: 'USAGE' });
}

main().catch(error => {
  const value = error instanceof CliError ? error : new CliError(error.message || String(error), { code: 'UNEXPECTED' });
  process.stderr.write(`${JSON.stringify({ error: { code: value.code, message: value.message, status: value.status, details: value.details } })}\n`);
  process.exitCode = value.code === 'USAGE' ? 2 : value.code === 'UNKNOWN_MUTATION_OUTCOME' ? 4 : value.code === 'STOPPED' ? 130 : 1;
});

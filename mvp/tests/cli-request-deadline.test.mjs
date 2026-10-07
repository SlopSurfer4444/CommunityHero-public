import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { CommunityHeroClient, UnknownMutationError } from '../cli/client.mjs';

const exec = promisify(execFile);
const cli = fileURLToPath(new URL('../cli/communityhero.mjs', import.meta.url));

test('default CLI and client admit a slow response beyond the old 15s deadline; explicit timeout never replays', { timeout: 35_000 }, async t => {
  const posts = new Map();
  const timers = new Set();
  const server = http.createServer((req, res) => {
    req.resume();
    const send = value => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
    if (req.url === '/api/engine/status') return send({ account: 'likeavto' });
    if (req.url === '/api/session') return send({ csrfToken: 'test-csrf' });
    if (req.method === 'POST' && req.url.endsWith('/execute')) {
      posts.set(req.url, (posts.get(req.url) || 0) + 1);
      const approvalId = req.url.split('/')[3];
      const timer = setTimeout(() => { timers.delete(timer); send({ jobId: 'delayed-job', approvalId,
        requestId: `${approvalId}-request`, replayed: false }); }, 16_000);
      timers.add(timer);
      res.on('close', () => { clearTimeout(timer); timers.delete(timer); });
      return;
    }
    res.writeHead(404); res.end();
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    for (const timer of timers) clearTimeout(timer);
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  });
  const baseUrl = `http://127.0.0.1:${server.address().port}`;
  const client = new CommunityHeroClient({ account: 'likeavto', baseUrl });
  const short = new CommunityHeroClient({ account: 'likeavto', baseUrl, timeoutMs: 1000 });
  await Promise.all([
    client.execute('client-default').then(result => assert.equal(result.jobId, 'delayed-job')),
    exec(process.execPath, [cli, 'execute', '--account', 'likeavto', '--base-url', baseUrl, '--approval', 'cli-default', '--request-id', 'cli-default-request'], { timeout: 30_000 })
      .then(({ stdout }) => assert.equal(JSON.parse(stdout).jobId, 'delayed-job')),
    assert.rejects(short.execute('client-short'), error => error instanceof UnknownMutationError && error.details.timeoutMs === 1000),
    assert.rejects(exec(process.execPath, [cli, 'execute', '--account', 'likeavto', '--base-url', baseUrl, '--approval', 'cli-short', '--request-id', 'cli-short-request', '--request-timeout-ms', '1000'], { timeout: 10_000 }),
      error => /UNKNOWN_MUTATION_OUTCOME/.test(error.stderr))
  ]);
  for (const name of ['client-default', 'cli-default', 'client-short', 'cli-short'])
    assert.equal(posts.get(`/api/approvals/${name}/execute`), 1, name);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

const app=await readFile(new URL('../workshop/app.js',import.meta.url),'utf8');
const style=await readFile(new URL('../workshop/style.css',import.meta.url),'utf8');
const surfaces=await readFile(new URL('../workshop/surface-system.css',import.meta.url),'utf8');

test('recipient and draft state live in a sibling footer outside the glass input surface',()=>{
  const start=app.indexOf('function composerHtml(item)');
  const end=app.indexOf('\nfunction assistantTopic()',start);
  const composer=app.slice(start,end);
  assert.ok(composer.includes('textarea id="draft" aria-describedby="draft-status"'));
  assert.match(composer,/<div class="composer-submit">\$\{sendButton[\s\S]*?<\/div><\/div>[\s\S]*?<\/div><footer class="composer-meta-footer"/);
  assert.ok(!composer.includes('class="composer-label"'));
  // Current preparation disposition is visible in the footer before generic
  // stale/draft wording; a canonical media hold retains first priority.
  assert.match(composer,/const draftStatus=mediaHold\?\.label\|\|disposition\?\.label\|\|\(stale\|\|staleGenerated\?'Нужна перепроверка':replyStatus\|\|'Черновик · не отправлен'\)/);
  assert.match(composer,/class="composer-recipient" title="\$\{esc\(recipient\)\}"/);
  assert.match(composer,/<span id="draft-status" data-media-hold="\$\{mediaHold\?'true':''\}" title="\$\{esc\(draftStatusTitle\)\}">\$\{esc\(draftStatus\)\}<\/span>/);
});

test('compact footer truncates only the recipient and lets status wrap without changing the input minimum',()=>{
  assert.match(style,/\.composer-meta-footer \{[^}]*min-height: 18px;[^}]*\}/);
  assert.match(style,/\.composer-recipient \{[^}]*white-space: nowrap;[^}]*overflow: hidden;[^}]*text-overflow: ellipsis;[^}]*\}/);
  assert.match(style,/\.composer-meta-footer #draft-status \{[^}]*white-space: normal;[^}]*overflow-wrap: anywhere;[^}]*\}/);
  assert.match(style,/\.input-surface \{[^}]*min-height: var\(--composer-min-height\);[^}]*\}/);
  assert.match(surfaces,/@container reading \(max-width:520px\) \{[\s\S]*?\.reply-actions \.composer-submit \{ flex-basis:auto; margin-top:0; \}/);
});

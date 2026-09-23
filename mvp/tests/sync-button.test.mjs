import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

const connectionSource=await readFile(new URL('../workshop/mvp-connection.js',import.meta.url),'utf8');
const workshopSource=await readFile(new URL('../workshop/app.js',import.meta.url),'utf8');

test('provider loading is owned by the background worker, without manual UI controls',()=>{
  assert.doesNotMatch(connectionSource,/id=["']mvp-(?:sync|sync-now|load-next)["']/);
  assert.doesNotMatch(connectionSource,/api\(['"`]\/api\/sync/);
  assert.match(connectionSource,/Обновление комментариев<\/dt><dd>Автоматически в фоне/);
  assert.match(connectionSource,/Связь временно недоступна\. Повторяем автоматически\./);
});

test('local list pagination remains available for already loaded comments',()=>{
  assert.match(workshopSource,/id="load-more">Показать ещё/);
  assert.match(workshopSource,/IntersectionObserver/);
  assert.match(workshopSource,/nextListWindow\(before,items\.length,height\)/);
});

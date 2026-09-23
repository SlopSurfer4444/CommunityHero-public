import test from 'node:test';
import assert from 'node:assert/strict';
import {commentMediaHtml,safeCommentMediaUrl} from '../workshop/comment-media.js';

test('comment photos and transparent stickers have their own viewable tiles',()=>{
  const html=commentMediaHtml([
    {type:'photo',url:'https://media.example/photo.jpg?size=large',title:'Кузов'},
    {type:'sticker',url:'https://media.example/sticker.png'}
  ]);
  assert.match(html,/data-comment-image="https:\/\/media\.example\/photo\.jpg\?size=large"/);
  assert.match(html,/data-comment-image-label="Фото · Кузов"/);
  assert.match(html,/class="comment-media-tile is-sticker"/);
  assert.doesNotMatch(html,/<iframe|autoplay/);
});

test('video uses a direct source only when supplied and otherwise retains its own preview/source link',()=>{
  const playable=commentMediaHtml([{type:'video',url:'https://cdn.example/clip.mp4',preview_url:'https://cdn.example/frame.jpg'}]);
  assert.match(playable,/<video controls playsinline preload="none" poster="https:\/\/cdn\.example\/frame\.jpg"/);
  assert.match(playable,/<source src="https:\/\/cdn\.example\/clip\.mp4"/);
  assert.match(playable,/<a href="https:\/\/cdn\.example\/clip\.mp4" target="_blank" rel="noopener noreferrer">Открыть видео<\/a>/);
  const sourceOnly=commentMediaHtml([{type:'video',source_url:'https://social.example/watch/1',preview_url:'https://cdn.example/frame.jpg'}]);
  assert.doesNotMatch(sourceOnly,/<video|<iframe/);
  assert.match(sourceOnly,/Открыть источник видео/);
  assert.match(sourceOnly,/rel="noopener noreferrer"/);
});

test('queue preview stays compact and reports attachment count',()=>{
  const html=commentMediaHtml([{type:'photo',url:'https://cdn.example/a.png'},{type:'sticker',url:'https://cdn.example/b.png'}],{compact:true});
  assert.match(html,/class="comment-media-queue" aria-label="Вложений: 2"/);
  assert.equal((html.match(/<img/g)||[]).length,1);
});

test('missing and untrusted locators never produce active media',()=>{
  for(const url of ['javascript:alert(1)','data:image/svg+xml,x','http://media.example/a','https://localhost/a','https://localhost./a','https://printer/a','https://printer.local/a','https://service.internal/a','https://home.lan/a','https://127.0.0.1/a','https://[::1]/a','https://media.example:8443/a','https://user:pass@media.example/a','https://media.example/a?access_token=secret']) {
    assert.equal(safeCommentMediaUrl(url),null);
  }
  assert.equal(safeCommentMediaUrl('https://media.example:443/a'),'https://media.example/a');
  const html=commentMediaHtml([{type:'photo',url:'javascript:alert(1)'},{type:'video',url:'https://localhost/a',source_url:'https://media.example/watch?cookie=secret'},{type:'unknown',url:'https://cdn.example/file'}]);
  assert.doesNotMatch(html,/<img|<video|<a |<iframe|javascript:|secret/);
  assert.match(html,/Фото недоступно/);
  assert.match(html,/Видео недоступно/);
  assert.equal(commentMediaHtml(undefined),'');
});

test('attribute text is escaped',()=>{
  const html=commentMediaHtml([{type:'photo',url:'https://cdn.example/a?x=1&y=2',title:'" onload="alert(1)'}]);
  assert.match(html,/x=1&amp;y=2/);
  assert.doesNotMatch(html,/" onload="/);
});

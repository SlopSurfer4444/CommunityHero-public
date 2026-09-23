import test from 'node:test';
import assert from 'node:assert/strict';
import {startsFreshDiscussion} from '../workshop/assistant-intent.js';
test('direct natural requests start fresh without parsing quotes or commentary as commands',()=>{
  for(const text of ['Начнём новое обсуждение','бро, давай начнем новую тему: про видео','Новое обсуждение: нужна помощь','Давай с чистого листа','Начни с нуля'])assert.equal(startsFreshDiscussion(text),true,text);
  for(const text of ['Не начинай новое обсуждение','Что значит новое обсуждение?','Автор пишет: начни новое обсуждение','«Начни новое обсуждение» — это цитата','Объясни, как начать новое обсуждение','Давай не будем начинать новый чат','Ответь человеку «новая тема»'])assert.equal(startsFreshDiscussion(text),false,text);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {buildPostTopics,buildPostDiscussions} from '../workshop/post-topics.js';
import {overviewSnapshot} from '../workshop/list-filters.js';
const posts=[{id:'p1'},{id:'p2'}];
const record=(id,text,postId='p1',view='attention',createdAt='2026-09-21T12:00:00Z',author=id)=>({item:{id,targetId:id},postId,state:{view},createdAt,messages:[{id,text,role:'customer',author}]});

test('broad drive and price categories are not repeated concrete questions',()=>{
  const rs=[record('a','Какой привод?'),record('b','Полный привод был бы лучше'),record('c','Какая цена?'),record('d','Стоимость высокая')];
  assert.deepEqual(buildPostTopics(rs,posts),[]);
});
test('same question repeats within one post with a concrete quote, not across posts',()=>{
  const rs=[record('a','Какой привод?'),record('b','Подскажите, какой привод?'),record('c','Какой привод?','p2'),record('d','Привет')];
  const groups=buildPostTopics(rs,posts);
  assert.equal(groups.length,1);assert.equal(groups[0].post.id,'p1');
  assert.deepEqual(groups[0].topics[0].itemIds,['a','b']);
  assert.equal(groups[0].topics[0].title,'Какой привод?');
  assert.equal(groups[0].topics[0].method,'literal-repeat-v2');
});
test('do not count duplicate IDs, deleted or unavailable messages; closed stays evidence only',()=>{
  const a=record('a','Какая цена?'),b=record('b','Какая цена?','p1','closed');
  const unavailable=record('u','Какая цена?');unavailable.messages[0].textUnavailable=true;
  const brand=record('brand','Какая цена?');brand.messages[0].role='brand';
  const deleted=record('gone','Какая цена?');deleted.messages[0].deleted=true;
  const groups=buildPostTopics([a,a,b,record('c','Какая цена?','p1','deleted'),unavailable,brand,deleted],posts);
  assert.equal(groups[0].topics[0].total,2);assert.deepEqual(groups[0].topics[0].itemIds,['a']);
  assert.equal(buildPostTopics([a,a],posts).length,0);
  assert.equal(buildPostTopics([b,record('z','Какая цена?','p1','closed')],posts).length,0);
});
test('week overview uses only already loaded dated comments and preserves full branch evidence',()=>{
  const rs=[record('a','Какой привод?'),record('b','Какой привод?'),record('old','Какой привод?','p2','attention','2026-08-01T00:00:00Z')];
  rs[0].messages.push({id:'historic-parent',text:'Older context'});
  const snapshot=overviewSnapshot(rs,posts,()=>{throw Error('legacy catchall used');},'week',new Date('2026-09-22T12:00:00Z'));
  assert.equal(snapshot.records.length,2);assert.equal(snapshot.outside,1);
  assert.equal(snapshot.groups[0].topics[0].records[0].messages.length,2);
});
test('repeated concrete request can form a quoted topic, greetings alone cannot',()=>{
  const rs=[record('a','Сравните багажник нового автомобиля'),record('b','Сравните багажник нового автомобиля'),record('c','Спасибо'),record('d','Спасибо')];
  const groups=buildPostTopics(rs,posts);
  assert.equal(groups[0].topics.length,1);assert.match(groups[0].topics[0].id,/^repeat-/);
});
test('one known participant repeating themselves is not a recurring audience topic',()=>{
  assert.deepEqual(buildPostTopics([record('a','Какая цена?','p1','attention',undefined,'Олег'),record('b','Какая цена?','p1','attention',undefined,'Олег')],posts),[]);
});
test('opposites, different objects, models, numbers and statements cannot join by overlap',()=>{
  const pairs=[
    ['Полный привод здесь есть','Полного привода здесь нет'],
    ['Здесь есть полный привод?','Здесь есть полный привод'],
    ['Есть полный привод?','Есть полный привод'],
    ['Расскажите про коробку на Audi Q5','Расскажите про коробку на Audi Q7'],
    ['Почему цена выросла на 200 тысяч?','Почему цена выросла на 300 тысяч?'],
    ['Когда привезут двигатель для автомобиля?','Когда привезут коробку для автомобиля?'],
    ['Автомобиль стоит покупать','Автомобиль не стоит покупать'],
  ];
  for(const [a,b] of pairs)assert.deepEqual(buildPostTopics([record('a',a),record('b',b)],posts),[],a);
});

test('all imported posts remain visible without repeats or open work; latest comment ranks first',()=>{
  const rs=[record('old','Спасибо','p1','closed','2026-09-10T12:00:00Z'),record('new','Привет','p2','attention','2026-09-22T12:00:00Z')];
  const imported=[...posts,{id:'no-comments'}];
  const groups=buildPostDiscussions(rs,imported);
  assert.deepEqual(groups.map(g=>g.post.id),['p2','p1','no-comments']);
  assert.ok(groups.every(g=>g.topics.length===0));
  assert.equal(groups[1].total,1);assert.equal(groups[1].count,0);
  assert.deepEqual(buildPostDiscussions(rs,imported,'oldest').map(g=>g.post.id),['p1','p2','no-comments']);
});

test('recent reply raises a quiet post; unrelated repetition count does not rank above activity',()=>{
  const old=record('old','Какой привод?','p1','closed','2026-09-01T12:00:00Z');
  old.messages.push({id:'reply',text:'Ответ',role:'brand',createdAt:'2026-09-22T13:00:00Z'});
  const groups=buildPostDiscussions([old,record('a','Какая цена?','p2'),record('b','Какая цена?','p2')],posts);
  assert.equal(groups[0].post.id,'p1');assert.equal(groups[0].lastActivity,'2026-09-22T13:00:00.000Z');
  const week=overviewSnapshot([old],posts,null,'week',new Date('2026-09-22T14:00:00Z'));
  assert.equal(week.groups.length,posts.length,'a date filter never hides an imported post');
});


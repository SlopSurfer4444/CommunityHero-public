import test from 'node:test';
test('attention queue omits its redundant status but keeps reasons and preparation failures',()=>{
  const item={autoPreparation:{status:'needs_attention'},triageTags:['needs_fact','question']};
  assert.deepEqual(queueTags({view:'attention'},item,{currentView:'attention'}).map(t=>t.label),['Нужен факт','Вопрос']);
  assert.equal(queueTags({view:'attention'},{autoPreparation:{status:'error'}},{currentView:'attention'})[0].label,'Ошибка разбора');
});
import assert from 'node:assert/strict';
import {outcomeCounts,queueTags,postThumbnail,postThumbnailCandidates,initializeOverviewPeriod,publicationTitle} from '../workshop/workspace-presentation.js';
import {filterList} from '../workshop/list-filters.js';

test('publication heading uses the real caption instead of an account placeholder',()=>{
  assert.equal(publicationTitle({title:'Публикация LikeAvto',excerpt:'Зачем нужна Нива? Suzuki Jimny #лайкавто #авто'}),'Зачем нужна Нива? Suzuki Jimny');
  assert.equal(publicationTitle({title:'Настоящий заголовок',text:'Длинное описание'}),'Настоящий заголовок');
  assert.equal(publicationTitle({title:'Video by likeavto_import',text:'Новая Mazda CX-5'}),'Новая Mazda CX-5');
  assert.equal(publicationTitle({title:'Публикация LikeAvto'}),'Пост без заголовка');
});

test('closed comments without a confirmed reply use the without-reply presentation',()=>{
  assert.equal(queueTags({view:'closed',closure:null},{})[0].label,'Без ответа');
  assert.equal(queueTags({view:'closed',closure:{outcome:'unknown'}},{})[0].label,'Без ответа');
  assert.equal(queueTags({view:'closed',closure:{outcome:'no_reply'}},{})[0].label,'Без ответа');
  assert.equal(queueTags({view:'closed',closure:{outcome:'reply'}},{})[0].label,'С ответом');
  assert.deepEqual(queueTags({view:'closed',closure:{outcome:'unknown'}},{},{currentView:'closed',currentOutcome:'no_reply'}),[]);
});

test('closed queue tabs partition imported unknown outcomes exactly and preserve source evidence',()=>{
  const row=(id,outcome,author='Alice')=>({item:{id,targetId:id},state:{view:'closed',closure:outcome?{outcome}:null},postId:'p',channel:'VK',createdAt:'2026-09-22T08:00:00Z',messages:[{id,author,text:'hello'}]});
  const records=[row('missing'),row('unknown','unknown'),row('reply','reply'),row('no-reply','no_reply'),row('excluded','unknown','Bob')];
  const before=structuredClone(records),options={view:'closed',query:'Alice',filters:{period:'all',postId:'p',channel:'VK'}};
  const counts=outcomeCounts(records,options);
  assert.deepEqual(counts,{all:4,reply:1,no_reply:3});
  const replies=filterList(records,{...options,outcome:'reply'}),withoutReplies=filterList(records,{...options,outcome:'no_reply'});
  assert.equal(replies.length,counts.reply);assert.equal(withoutReplies.length,counts.no_reply);
  assert.deepEqual(withoutReplies.map(r=>r.item.id).sort(),['missing','no-reply','unknown']);
  assert.equal(new Set([...replies,...withoutReplies].map(r=>r.item.id)).size,counts.all);
  assert.deepEqual(records,before);
});

test('prepared and closed outcome tabs omit only their redundant outcome tag',()=>{
  const prepared={view:'prepared',decision:'reply'};
  const closed={view:'closed',closure:{outcome:'no_reply'}};
  assert.deepEqual(queueTags(prepared,{}, {currentView:'prepared',currentOutcome:'reply'}),[]);
  assert.deepEqual(queueTags(prepared,{triageTags:['question']},{currentView:'prepared',currentOutcome:'reply'}).map(t=>t.label),['Вопрос']);
  assert.deepEqual(queueTags(closed,{triageTags:['complaint']},{currentView:'closed',currentOutcome:'no_reply'}).map(t=>t.label),['Жалоба']);
  assert.equal(queueTags(prepared,{}, {currentView:'prepared',currentOutcome:'all'})[0].label,'С ответом');
  assert.equal(queueTags(prepared,{}, {currentView:'prepared',currentOutcome:'no_reply'})[0].label,'С ответом');
  assert.equal(queueTags(prepared,{}, {stale:true,currentView:'prepared',currentOutcome:'reply'})[0].label,'Перепроверить');
});

test('outcome counts share search/post/channel filters and ignore selected outcome',()=>{
  const record=(id,decision,postId='p',channel='VK',author='Alice')=>({item:{id,targetId:id},state:{view:'prepared',decision},postId,channel,createdAt:'2026-09-22T08:00:00Z',messages:[{id,author,text:'hello'}]});
  const records=[record('a','reply'),record('b','no_reply'),record('c','reply','other'),record('d','reply','p','YouTube'),record('e','no_reply','p','VK','Bob')];
  assert.deepEqual(outcomeCounts(records,{view:'prepared',outcome:'no_reply',query:'Alice',filters:{period:'all',postId:'p',channel:'VK'}}),{all:2,reply:1,no_reply:1});
});

test('queue uses bounded structured tags and never copies long explanation',()=>{
  const reason='Объяснение '.repeat(100);
  const tags=queueTags({view:'attention'}, {reason,attentionLabel:reason,autoPreparation:{status:'needs_attention'},triageTags:['needs_fact','question','question','untrusted-tag']});
  assert.deepEqual(tags.map(t=>t.label),['Нужно участие','Нужен факт','Вопрос']);
  assert.ok(tags.every(t=>t.label.length<24));
  assert.equal(queueTags({view:'prepared',decision:'no_reply'},{} )[0].label,'Без ответа');
  assert.equal(queueTags({view:'attention'}, {reason:'Нужны подтверждённые данные по цене'})[1].inferred,true);
  assert.equal(queueTags({view:'attention'}, {reason:'Технический вопрос без явного решения'}).length,1);
});

test('thumbnail uses explicit photo/preview or validated public YouTube ID, never raw video',()=>{
  assert.equal(postThumbnail({attachments:[{type:'video',url:'https://cdn.example/clip.mp4'}]}),null);
  assert.equal(postThumbnail({attachments:[{type:'photo',url:'https://cdn.example/photo.jpg'}]}),'https://cdn.example/photo.jpg');
  assert.equal(postThumbnail({attachments:[{type:'video',previewUrl:'https://cdn.example/preview.jpg'}]}),'https://cdn.example/preview.jpg');
  assert.equal(postThumbnail({sourceUrl:'https://youtu.be/abcdefghijk'}),'https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg');
  assert.equal(postThumbnail({sourceUrl:'https://www.youtube.com/shorts/abcdefghijk'}),'https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg');
  assert.equal(postThumbnail({sourceUrl:'https://youtube.com.example/watch?v=abcdefghijk'}),null);
  assert.equal(postThumbnail({thumbnailUrl:'javascript:alert(1)'}),null);
  assert.equal(postThumbnail({sourceUrl:'https://example.org/post/1'}),null);
});

test('thumbnail candidates recover canonical YouTube previews from video attachment sources',()=>{
  const candidates=postThumbnailCandidates({attachments:[
    {type:'video',url:'https://cdn.example/clip.mp4',source_url:'https://youtu.be/abcdefghijk'},
    {type:'video',sourceUrl:'https://www.youtube.com/live/zyxwvutsrqp?feature=share'},
  ]});
  assert.deepEqual(candidates,[
    'https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg',
    'https://i.ytimg.com/vi/abcdefghijk/mqdefault.jpg',
    'https://i.ytimg.com/vi/zyxwvutsrqp/hqdefault.jpg',
    'https://i.ytimg.com/vi/zyxwvutsrqp/mqdefault.jpg',
  ]);
  assert.equal(postThumbnail({sourceUrl:'https://www.youtube-nocookie.com/embed/abcdefghijk'}),candidates[0]);
});

test('thumbnail candidates keep fallbacks and may reuse an equivalent sibling video',()=>{
  const selected={postKey:'vk-post',account:'LikeAvto',title:'Обзор Changan A06 #авто',attachments:[{type:'video',preview_url:'https://expired.example/preview.jpg'}]};
  const sibling={postKey:'youtube-post',account:'LikeAvto',title:'Обзор Changan A06',sourceUrl:'https://www.youtube.com/watch?v=abcdefghijk'};
  assert.deepEqual(postThumbnailCandidates(selected,[selected,sibling]),[
    'https://expired.example/preview.jpg',
    'https://i.ytimg.com/vi/abcdefghijk/hqdefault.jpg',
    'https://i.ytimg.com/vi/abcdefghijk/mqdefault.jpg',
  ]);
});

test('all posts migration runs once and preserves explicit/custom later choice',()=>{
  const implicitWeek={overviewPeriod:'week',overviewWeekDefaultVersion:1};initializeOverviewPeriod(implicitWeek);assert.equal(implicitWeek.overviewPeriod,'all');
  const explicitWeek={overviewPeriod:'week',overviewPeriodExplicit:true};initializeOverviewPeriod(explicitWeek);assert.equal(explicitWeek.overviewPeriod,'week');
  const formerDefault={overviewPeriod:'all'};initializeOverviewPeriod(formerDefault);assert.equal(formerDefault.overviewPeriod,'all');
  formerDefault.overviewPeriod='all';initializeOverviewPeriod(formerDefault);assert.equal(formerDefault.overviewPeriod,'all');
  const custom={overviewPeriod:'custom',overviewFrom:'2026-09-01'};initializeOverviewPeriod(custom);assert.equal(custom.overviewPeriod,'custom');
  const explicit={overviewPeriod:'all',overviewPeriodExplicit:true};initializeOverviewPeriod(explicit);assert.equal(explicit.overviewPeriod,'all');
});

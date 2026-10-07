import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
import {queueCoverageState,queueCoveragePresentation,loadedQueueCountLabel} from '../workshop/queue-coverage.js';

const accounting={version:1,trackedUnique:3,importedUnique:3,unresolvedUnique:0,unverifiedPages:0,overflow:false};
const covered={scope:'all-open',done:true,traversalComplete:true,contextComplete:true,coverageComplete:true,snapshotConsistent:false,accounting};

test('complete current coverage permits a completed pass on a mutable queue',()=>{
  assert.equal(queueCoverageState({openCoverage:covered}),'complete');
  const presentation=queueCoveragePresentation({openCoverage:covered});
  assert.equal(presentation.note,'Сверка очереди завершена.');
  assert.doesNotMatch(presentation.note,/все|всего|снимок/i);
  assert.equal(queueCoverageState({openCoverage:{...covered,skipped:2}}),'complete','recovered historical skips do not block a proved pass');
});

test('legacy, absent and partial evidence never borrow completeness from done or old frontiers',()=>{
  for(const sync of [{},{open:{coverage:{complete:true}}},{openFrontier:covered},{openCoverage:{done:true}},
    {openCoverage:{scope:'all-open',done:true,coverageComplete:true}},
    {openCoverage:{...covered,accounting:undefined}},{openCoverage:{...covered,contextComplete:undefined}},
    {openCoverage:{...covered,traversalComplete:undefined}},{openCoverage:{...covered,done:undefined}},
    {openCoverage:{...covered,scope:'recent'}},
    {openCoverage:{...covered,accounting:{...accounting,unverifiedPages:undefined}}}]) {
    assert.equal(queueCoverageState(sync),'unknown');
    assert.match(queueCoveragePresentation(sync).note,/пока не подтверждена/);
  }
});

test('pending, incomplete contexts, invalidated, unverified and contradictory observations never complete',()=>{
  for(const change of [{done:false,coverageComplete:false},{traversalComplete:false,coverageComplete:false},
    {contextComplete:false,coverageComplete:false},{invalidatedAt:'changed'},
    {unknownDates:1},{accounting:{...accounting,unresolvedUnique:1,importedUnique:2}},
    {accounting:{...accounting,unverifiedPages:1}},{accounting:{...accounting,overflow:true}},
    {accounting:{...accounting,importedUnique:99}}]) {
    assert.notEqual(queueCoverageState({openCoverage:{...covered,...change}}),'complete');
  }
  const sync={openCoverage:{...covered,coverageComplete:false},openFrontier:covered};
  assert.equal(queueCoveragePresentation(sync).note,'Сверка очереди продолжается.');
  assert.equal(queueCoverageState({openCoverage:covered,scan:{invalidatedAt:'changed'}}),'incomplete');
});

test('closed history is separate from the open queue and only claims its load period',()=>{
  const sync={openCoverage:covered,scan:{closed:{done:false,coverageComplete:false}}};
  assert.equal(queueCoveragePresentation(sync,'closed').note,'История загружается.');
  assert.equal(queueCoveragePresentation(sync,'deleted').state,'incomplete');
  assert.equal(queueCoveragePresentation({openCoverage:covered},'closed').state,'unknown');
  assert.match(queueCoveragePresentation({scan:{closed:covered}},'closed').note,/за период загрузки/);
});

test('counts remain loaded filter results in every coverage state; connection failures do not suggest active progress',()=>{
  assert.equal(loadedQueueCountLabel(0),'Загружено: 0');
  assert.equal(loadedQueueCountLabel(27,true),'Найдено среди загруженных: 27');
  for(const sync of [{},{openCoverage:covered},{openCoverage:{...covered,coverageComplete:false}}]) {
    assert.match(queueCoveragePresentation(sync).countTitle,/среди загруженных.*условий/);
  }
  for(const sync of [{status:'error'},{background:{state:'backoff'}}]) {
    assert.match(queueCoveragePresentation(sync).note,/Связь временно недоступна/);
    assert.doesNotMatch(queueCoveragePresentation(sync).note,/продолжается/);
  }
});

test('production list renders loaded provenance and current coverage beside the filtered count',async()=>{
  const source=await readFile(new URL('../workshop/app.js',import.meta.url),'utf8');
  const functions=source.slice(source.indexOf('function queueCoverage()'),source.indexOf('function navigationHtml()'))
    +source.slice(source.indexOf('function chipsHtml()'),source.indexOf('function refreshList('));
  for(const [sync,filtered,note] of [[{},false,'Полнота очереди пока не подтверждена.'],
    [{openCoverage:{...covered,done:false,coverageComplete:false}},true,'Сверка очереди продолжается.'],
    [{openCoverage:covered},false,'Сверка очереди завершена.']]) {
    const context={mvp:{queueCoverage:view=>queueCoveragePresentation(sync,view)},queueCoveragePresentation,loadedQueueCountLabel,
      saved:{view:'attention',search:filtered?'Alice':''},data:{posts:[]},filtersFor:()=>({period:'all'}),
      viewItems:()=>Array(4).fill({}),hasListConditions:()=>filtered,processingRecordsForView:()=>[{},{}],
      dateBasis:()=> 'created',basisLabels:{created:'По дате комментария'},icon:()=>'',esc:value=>String(value)};
    const html=vm.runInNewContext(`${functions}\nchipsHtml()`,context);
    assert.match(html,/class="list-count" role="status" title="Количество среди загруженных/);
    assert.ok(html.includes(loadedQueueCountLabel(4,filtered)));
    assert.ok(html.includes(`<p class="list-coverage" role="status">${note}</p>`));
    assert.match(html,/Готовим: 2/);
    assert.doesNotMatch(html,/всего|все комментарии загружены/i);
  }
});

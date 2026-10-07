import test from 'node:test';
import assert from 'node:assert/strict';
import {syncCoverageLabel} from '../workshop/mvp-connection.js';

const accounting={version:1,trackedUnique:3,importedUnique:3,unresolvedUnique:0,overflow:false,unverifiedPages:0};
const covered={scope:'all-open',done:true,traversalComplete:true,contextComplete:true,coverageComplete:true,snapshotConsistent:false,accounting};

test('background coverage uses openCoverage and scan.closed rather than manual pagination',()=>{
  const sync={open:{coverage:{complete:true}},closed:{coverage:{complete:true}},openCoverage:{...covered,traversalComplete:false,coverageComplete:false},
    scan:{closed:{...covered,contextComplete:false,coverageComplete:false,accounting:{...accounting,importedUnique:2,unresolvedUnique:1}}}};
  assert.match(syncCoverageLabel(sync,'open'),/ещё не завершён/);
  assert.match(syncCoverageLabel(sync,'closed'),/требуют проверки: 1/);
  assert.doesNotMatch(syncCoverageLabel(sync,'closed'),/Полнота контекстов подтверждена/);
});

test('missing accounting or new coverage flags never inherit legacy done as complete',()=>{
  for(const row of [{scope:'all-open',done:true,coverageComplete:true},{...covered,accounting:undefined},
    {...covered,traversalComplete:undefined},{...covered,contextComplete:undefined},{...covered,accounting:{...accounting,unverifiedPages:undefined}}]) {
    assert.doesNotMatch(syncCoverageLabel({openCoverage:row}),/Полнота контекстов подтверждена/);
  }
  assert.match(syncCoverageLabel({open:{coverage:{complete:true}}}),/пока не подтверждена/);
});

test('successful same-pass context retry may complete coverage despite historical skipped count',()=>{
  const label=syncCoverageLabel({openCoverage:{...covered,skipped:2}});
  assert.match(label,/Полнота контекстов подтверждена в этом проходе/);
  assert.match(label,/может меняться/);
  assert.doesNotMatch(label,/единый снимок|все комментарии загружены/);
});

test('unresolved, unverified, overflow, invalidated or dateless observations cannot claim complete coverage',()=>{
  for(const change of [{accounting:{...accounting,importedUnique:2,unresolvedUnique:1}},
    {accounting:{...accounting,unverifiedPages:1}},{accounting:{...accounting,overflow:true}},
    {unknownDates:1},{invalidatedAt:'changed'},{accounting:{...accounting,importedUnique:100}}]) {
    assert.doesNotMatch(syncCoverageLabel({openCoverage:{...covered,...change}}),/Полнота контекстов подтверждена/);
  }
  assert.match(syncCoverageLabel({openCoverage:{...covered,accounting:{...accounting,overflow:true}}}),/лимит учёта/);
});

test('closed history states the actual scan window and cannot certify all historical records',()=>{
  const sync={openCoverage:covered,scan:{closed:covered,window:{since:'2026-09-24T10:00:00Z',until:'2026-09-26T10:00:00Z'}}};
  const label=syncCoverageLabel(sync,'closed');
  assert.match(label,/Период:/);assert.match(label,/24\.09\.2026/);assert.match(label,/26\.09\.2026/);
  assert.match(label,/в этом проходе/);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {LIST_PAGE, LIST_MAX_ROWS, initialListWindow, listWindowAfterSelection, listWindowAfterConditionsChange, listWindowAfterWidthChange, nextListWindow, previousListWindow} from '../workshop/list-window.js';

test('long queue advances in 50-row pages while keeping the rendered window bounded',()=>{
  const total=100_000;
  let window=initialListWindow(total);
  assert.deepEqual([window.start,window.end],[0,LIST_PAGE]);
  for(let i=0;i<1_000;i++){
    const removedHeight=Math.max(0,Math.min(total,window.end+LIST_PAGE)-window.start-LIST_MAX_ROWS)*112;
    window=nextListWindow(window,total,removedHeight);
    assert.ok(window.end-window.start<=LIST_MAX_ROWS);
  }
  assert.equal(window.end,50_050);
  assert.equal(window.topHeight,window.start*112);
});

test('selection stays in the expanded window and history can reveal a distant selected row',()=>{
  let window=initialListWindow(3_000);
  for(let i=0;i<4;i++)window=nextListWindow(window,3_000,window.end>=150?LIST_PAGE*112:0);
  assert.deepEqual([window.start,window.end],[100,250]);
  assert.equal(listWindowAfterSelection(window,3_000,105),window);
  const selected=listWindowAfterSelection(window,3_000,2_876);
  assert.ok(selected.start<=2_876&&selected.end>2_876);
  assert.ok(selected.end-selected.start<=LIST_MAX_ROWS);
  assert.equal(listWindowAfterSelection(selected,3_000,2_876),selected);
});

test('changing sort or filters starts at the first result without changing the conversation selection',()=>{
  const state={selected:'comment-2876',window:listWindowAfterSelection(initialListWindow(3_000),3_000,2_876)};
  assert.ok(state.window.start>0);
  state.window=listWindowAfterConditionsChange(3_000);
  assert.deepEqual([state.window.start,state.window.end,state.window.topHeight],[0,LIST_PAGE,0]);
  assert.equal(state.selected,'comment-2876');
});

test('returning toward earlier rows keeps the measured spacer in sync',()=>{
  let window=initialListWindow(500);
  window=nextListWindow(window,500,0);
  window=nextListWindow(window,500,0);
  window=nextListWindow(window,500,5_700);
  assert.deepEqual([window.start,window.end,window.topHeight],[50,200,5_700]);
  window=previousListWindow(window,5_700);
  assert.deepEqual([window.start,window.end,window.topHeight],[0,150,0]);
});

test('a short final page trims one complete measured page',()=>{
  let window={start:100,end:250,topHeight:11_200};
  window=nextListWindow(window,260,5_600);
  assert.deepEqual([window.start,window.end,window.topHeight],[150,260,16_800]);
  window=previousListWindow(window,5_600);
  assert.deepEqual([window.start,window.end,window.topHeight],[100,250,11_200]);
});

test('width reflow adjusts measured hidden-page heights without changing the row range',()=>{
  const window={start:100,end:250,topHeight:12_000};
  const pageHeights=new Map([[0,6_000],[50,6_000]]);
  const adjusted=listWindowAfterWidthChange(window,pageHeights,120,150);
  assert.deepEqual(adjusted.window,{start:100,end:250,topHeight:15_000});
  assert.deepEqual([...adjusted.pageHeights],[[0,7_500],[50,7_500]]);
  assert.equal(window.topHeight,12_000);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {COLUMN_LIMITS, WORKSPACE_MINIMUM, maximumColumnWidth, resolveColumns, resizeColumns} from '../workshop/column-layout.js';

const closeTo=(actual,expected,tolerance=0.001)=>assert.ok(Math.abs(actual-expected)<=tolerance,`${actual} is not within ${tolerance} of ${expected}`);

test('each side consumes centre slack before pushing the opposite side',()=>{
  const options={navigationCollapsed:true,assistantOpen:true,widths:{list:520,workspace:748,assistant:580}};
  const list=resizeColumns(1920,'list',620,options);
  assert.deepEqual(list,{navigation:62,list:620,workspace:648,assistant:580,resizable:true});
  const assistant=resizeColumns(1920,'assistant',680,options);
  assert.deepEqual(assistant,{navigation:62,list:520,workspace:648,assistant:680,resizable:true});
  const pushed=resizeColumns(1920,'list',920,options);
  assert.deepEqual(pushed,{navigation:62,list:920,workspace:410,assistant:518,resizable:true});
});

test('shrinking either side at the centre floor grows the centre, not the far side',()=>{
  const options={navigationCollapsed:true,assistantOpen:true,widths:{list:520,workspace:410,assistant:918}};
  assert.deepEqual(resizeColumns(1920,'assistant',818,options),{navigation:62,list:520,workspace:510,assistant:818,resizable:true});
  assert.deepEqual(resizeColumns(1920,'list',420,options),{navigation:62,list:420,workspace:510,assistant:918,resizable:true});
});

test('drag uses the actual expanded-navigation layout and persists without a jump',()=>{
  for(const widths of [{},{list:520,workspace:748,assistant:580},{list:600,assistant:640}]){
    const options={navigationCollapsed:false,assistantOpen:true,widths};
    const before=resolveColumns(1920,options);
    const after=resizeColumns(1920,'assistant',before.assistant-60,options);
    closeTo(after.list,before.list);
    closeTo(after.workspace,before.workspace+60);
    const committed=resolveColumns(1920,{...options,widths:{...widths,list:after.list,workspace:after.workspace,assistant:after.assistant}});
    closeTo(committed.workspace,after.workspace);
    closeTo(committed.list,after.list);
    closeTo(committed.assistant,after.assistant);
  }
});

test('safe minima and proportional 1920 defaults preserve the intended three-pane composition',()=>{
  assert.equal(COLUMN_LIMITS.list.min,220);
  assert.equal(COLUMN_LIMITS.assistant.min,260);
  assert.equal(WORKSPACE_MINIMUM,410);
  const layout=resolveColumns(1920,{navigationCollapsed:true,assistantOpen:true});
  assert.equal(layout.navigation,62);
  closeTo(layout.list,536,1);
  closeTo(layout.workspace,715,1);
  closeTo(layout.assistant,597,1);
  closeTo(layout.navigation+layout.list+layout.workspace+layout.assistant,1910);
});

test('active list drag keeps its requested width and pushes an oversized assistant at the workspace floor',()=>{
  const layout=resolveColumns(1920,{
    navigationCollapsed:true,
    assistantOpen:true,
    widths:{list:778,workspace:410,assistant:760},
    priority:'list',
  });
  assert.deepEqual(layout,{navigation:62,list:778,workspace:410,assistant:660,resizable:true});
});

test('active assistant drag has the inverse priority and pushes an oversized list',()=>{
  const layout=resolveColumns(1920,{
    navigationCollapsed:true,
    assistantOpen:true,
    widths:{list:760,workspace:410,assistant:778},
    priority:'assistant',
  });
  assert.deepEqual(layout,{navigation:62,list:660,workspace:410,assistant:778,resizable:true});
});

test('active pane maxima depend on safe peer minima rather than their current widths',()=>{
  const options={navigationCollapsed:true,assistantOpen:true,widths:{list:900,workspace:410,assistant:900}};
  assert.equal(maximumColumnWidth(1920,'list',options),1178);
  assert.equal(maximumColumnWidth(1920,'assistant',options),1218);
  const listMax=resolveColumns(1920,{...options,widths:{...options.widths,list:1178},priority:'list'});
  assert.equal(listMax.list,1178);
  assert.equal(listMax.workspace,WORKSPACE_MINIMUM);
  assert.equal(listMax.assistant,COLUMN_LIMITS.assistant.min);
});

test('navigation expansion scales every pane slack proportionally and collapse restores preferences',()=>{
  const widths={list:520,workspace:748,assistant:580};
  const collapsed=resolveColumns(1920,{navigationCollapsed:true,assistantOpen:true,widths});
  const expanded=resolveColumns(1920,{navigationCollapsed:false,assistantOpen:true,widths});
  assert.deepEqual(collapsed,{navigation:62,list:520,workspace:748,assistant:580,resizable:true});
  const scale=(expanded.list-COLUMN_LIMITS.list.min)/(collapsed.list-COLUMN_LIMITS.list.min);
  closeTo((expanded.workspace-WORKSPACE_MINIMUM)/(collapsed.workspace-WORKSPACE_MINIMUM),scale);
  closeTo((expanded.assistant-COLUMN_LIMITS.assistant.min)/(collapsed.assistant-COLUMN_LIMITS.assistant.min),scale);
  closeTo(expanded.navigation+expanded.list+expanded.workspace+expanded.assistant,1910);
  assert.deepEqual(resolveColumns(1920,{navigationCollapsed:true,assistantOpen:true,widths}),collapsed);
});

test('desktop allocations conserve width and preserve every achievable visible minimum',()=>{
  const preferences=[{}, {navigation:-1e9,list:-1e9,assistant:-1e9}, {navigation:1e9,list:1e9,assistant:1e9}];
  for(const viewport of [821,1024,1150,1151,1280,1536,1920,4096]) {
    for(const assistantOpen of [false,true])for(const navigationCollapsed of [false,true])for(const widths of preferences) {
      const columns=resolveColumns(viewport,{assistantOpen,navigationCollapsed,widths});
      closeTo(columns.navigation+columns.list+columns.assistant+columns.workspace,viewport-10);
      assert.ok(columns.workspace>=0);
      assert.ok(columns.list===0||columns.list>=COLUMN_LIMITS.list.min);
      assert.ok(columns.assistant===0||columns.assistant>=COLUMN_LIMITS.assistant.min);
      const safeWorkspace=viewport-10-columns.navigation-(columns.list?COLUMN_LIMITS.list.min:0)-(columns.assistant?COLUMN_LIMITS.assistant.min:0);
      assert.ok(columns.workspace+0.001>=Math.min(WORKSPACE_MINIMUM,Math.max(0,safeWorkspace)));
    }
  }
});

test('responsive layout keeps hiding the list only while assistant is open through 1150',()=>{
  for(const viewport of [821,1024,1150]) {
    assert.equal(resolveColumns(viewport,{assistantOpen:true}).list,0);
    assert.ok(resolveColumns(viewport,{assistantOpen:false}).list>=COLUMN_LIMITS.list.min);
  }
  const desktop=resolveColumns(1151,{assistantOpen:true});
  assert.ok(desktop.list>=COLUMN_LIMITS.list.min);
  assert.ok(desktop.assistant>=COLUMN_LIMITS.assistant.min);
  assert.ok(desktop.workspace>=WORKSPACE_MINIMUM);
});

test('compact and desktop nav states always push into a positive horizontal remainder',()=>{
  for(const viewport of [456,820,1150,1920])for(const navigationCollapsed of [false,true]) {
    const closed=resolveColumns(viewport,{navigationCollapsed,assistantOpen:false});
    const open=resolveColumns(viewport,{navigationCollapsed,assistantOpen:true});
    assert.equal(closed.navigation,open.navigation);
    assert.ok(viewport-10-closed.navigation>0);
    assert.ok(closed.list>0);
    assert.ok(closed.workspace>0);
    assert.ok(open.assistant>0);
    assert.ok(open.workspace>0);
    closeTo(closed.navigation+closed.list+closed.workspace,viewport-10);
    closeTo(open.navigation+open.list+open.workspace+open.assistant,viewport-10);
  }
});

test('huge finite preferences stay finite before and after proportional fitting',()=>{
  const columns=resolveColumns(1536,{
    assistantOpen:true,
    widths:{navigation:Number.MAX_VALUE,list:Number.MAX_VALUE,workspace:Number.MAX_VALUE,assistant:Number.MAX_VALUE},
  });
  for(const value of Object.values(columns))if(typeof value==='number')assert.ok(Number.isFinite(value));
  closeTo(columns.navigation+columns.list+columns.assistant+columns.workspace,1526);
});

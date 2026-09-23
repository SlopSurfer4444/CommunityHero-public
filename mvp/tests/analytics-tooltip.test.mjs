import test from 'node:test';
import assert from 'node:assert/strict';
import {bindAnalyticsTooltips,tooltipPosition} from '../workshop/analytics-tooltip.js';

class Element extends EventTarget {
  constructor(rect={left:100,right:120,top:100,width:20,height:220}){super();this.rect=rect;this.hidden=true;this.style={};}
  getBoundingClientRect(){return this.rect;}
  querySelector(){return this.tooltip;}
  querySelectorAll(){return this.bars;}
}
function fixture(){
  const root=new Element();root.ownerDocument={defaultView:{innerWidth:1000,innerHeight:700}};
  root.bars=[new Element(),new Element({left:930,right:950,top:600,width:20,height:220})];
  for(const bar of root.bars)bar.tooltip=new Element({width:272,height:230});
  bindAnalyticsTooltips(root);return root;
}
const fire=(element,type)=>element.dispatchEvent(new Event(type));

test('hover reveals the day tooltip, moving between days leaves only one visible',()=>{
  const root=fixture(),[first,second]=root.bars;
  fire(first,'pointerenter');assert.equal(first.tooltip.hidden,false);assert.equal(first.tooltip.style.left,'132px');
  fire(first,'pointerleave');assert.equal(first.tooltip.hidden,true);
  fire(second,'pointerenter');assert.equal(second.tooltip.hidden,false);assert.equal(first.tooltip.hidden,true);
  assert.equal(second.tooltip.style.left,'646px');assert.equal(second.tooltip.style.top,'458px');
});

test('keyboard focus reveals tooltip, pointer leaving does not hide a focused day, Escape dismisses',()=>{
  const root=fixture(),bar=root.bars[0];
  fire(bar,'focus');assert.equal(bar.tooltip.hidden,false);
  fire(bar,'pointerenter');fire(bar,'pointerleave');assert.equal(bar.tooltip.hidden,false);
  const escape=new Event('keydown');escape.key='Escape';bar.dispatchEvent(escape);assert.equal(bar.tooltip.hidden,true);
  fire(bar,'blur');fire(bar,'focus');assert.equal(bar.tooltip.hidden,false);
  fire(bar,'blur');assert.equal(bar.tooltip.hidden,true);
});

test('scroll dismisses tooltips and clipped edge positions stay within viewport',()=>{
  const root=fixture(),bar=root.bars[0];fire(bar,'pointerenter');fire(root,'scroll');assert.equal(bar.tooltip.hidden,true);
  assert.deepEqual(tooltipPosition({left:0,right:20,top:-50},{width:272,height:230},{width:320,height:480}),{left:32,top:12});
});

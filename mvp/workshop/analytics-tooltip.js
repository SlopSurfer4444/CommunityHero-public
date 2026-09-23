export function tooltipPosition(anchor, size, viewport) {
  const gap=12, edge=12;
  const preferred=anchor.right+gap;
  const left=preferred+size.width<=viewport.width-edge ? preferred : anchor.left-size.width-gap;
  return {
    left:Math.max(edge,Math.min(left,viewport.width-size.width-edge)),
    top:Math.max(edge,Math.min(anchor.top+18,viewport.height-size.height-edge)),
  };
}

export function bindAnalyticsTooltips(root) {
  if(!root)return;
  const hideAll=[];
  for(const bar of root.querySelectorAll('[data-analytics-tooltip]')){
    const tooltip=bar.querySelector('.activity-tooltip');
    if(!tooltip)continue;
    let hovered=false,focused=false;
    const hide=()=>{tooltip.hidden=true;};
    const show=()=>{
      for(const hideOther of hideAll)hideOther();
      tooltip.hidden=false;
      const view=root.ownerDocument.defaultView;
      const position=tooltipPosition(bar.getBoundingClientRect(),tooltip.getBoundingClientRect(),{width:view.innerWidth,height:view.innerHeight});
      tooltip.style.left=position.left+'px';tooltip.style.top=position.top+'px';
    };
    hideAll.push(hide);
    bar.addEventListener('pointerenter',()=>{hovered=true;show();});
    bar.addEventListener('pointerleave',()=>{hovered=false;if(!focused)hide();});
    bar.addEventListener('focus',()=>{focused=true;show();});
    bar.addEventListener('blur',()=>{focused=false;if(!hovered)hide();});
    bar.addEventListener('keydown',event=>{if(event.key==='Escape'){hide();event.stopPropagation();}});
  }
  const hide=()=>hideAll.forEach(callback=>callback());
  root.addEventListener('scroll',hide,{capture:true});
}

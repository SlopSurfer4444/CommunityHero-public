export const LIST_PAGE = 50;
export const LIST_MAX_ROWS = 150;
export const ESTIMATED_ROW_HEIGHT = 112;

export function initialListWindow(total, selectedIndex = -1) {
  const start = selectedIndex >= LIST_PAGE ? Math.floor(selectedIndex / LIST_PAGE) * LIST_PAGE : 0;
  return {start, end:Math.min(total,start+LIST_PAGE),topHeight:start*ESTIMATED_ROW_HEIGHT};
}

export function listWindowAfterSelection(window, total, selectedIndex) {
  if(selectedIndex < 0 || selectedIndex >= window.start && selectedIndex < window.end)return window;
  return initialListWindow(total,selectedIndex);
}

export function listWindowAfterConditionsChange(total) {
  return initialListWindow(total);
}

export function listWindowAfterWidthChange(window, pageHeights, previousMean, currentMean) {
  const ratio=currentMean/previousMean;
  if(!Number.isFinite(ratio)||ratio<=0)return {window,pageHeights};
  return {
    window:{...window,topHeight:Math.max(0,Math.round(window.topHeight*ratio))},
    pageHeights:new Map([...pageHeights].map(([start,height])=>[start,height*ratio]))
  };
}

export function nextListWindow(window,total,removedHeight=0) {
  if(window.end>=total)return window;
  const end=Math.min(total,window.end+LIST_PAGE);
  const trim=end-window.start>LIST_MAX_ROWS ? Math.min(LIST_PAGE,end-window.start) : 0;
  return {start:window.start+trim,end,topHeight:window.topHeight+removedHeight};
}

export function previousListWindow(window,addedHeight=0) {
  if(window.start===0)return window;
  const start=Math.max(0,window.start-LIST_PAGE);
  const end=Math.min(window.end,start+LIST_MAX_ROWS);
  return {start,end,topHeight:Math.max(0,window.topHeight-addedHeight)};
}

// Hidden topic columns can report scrollTop=0 during a responsive layout or
// repaint. Only a visible pane can replace its remembered reading state.
export function captureTopicReading(panel,previous={top:0,open:[],sources:[]}) {
  const pane=panel?.querySelector('.topic-scroll');
  if(!pane?.clientHeight)return null;
  const details=[...panel.querySelectorAll('.topic-instructions details[data-rule-version]')];
  return {top:pane.scrollTop,open:details.length?details.filter(node=>node.open).map(node=>node.dataset.ruleVersion):previous.open,
    sources:details.length?details.filter(node=>node.querySelector('.instruction-source')?.open).map(node=>node.dataset.ruleVersion):previous.sources};
}

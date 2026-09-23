export const COLUMN_LIMITS = Object.freeze({
  navigation: Object.freeze({ min: 160, max: 280 }),
  list: Object.freeze({ min: 220, max: Number.MAX_SAFE_INTEGER }),
  assistant: Object.freeze({ min: 260, max: Number.MAX_SAFE_INTEGER }),
});

export const WORKSPACE_MINIMUM = 410;

const COLLAPSED_NAVIGATION_WIDTH = 62;
const CONTENT_NAMES = ['list', 'workspace', 'assistant'];
const DEFAULT_SLACK_WEIGHTS = Object.freeze({list: 300, workspace: 290, assistant: 320});
const finiteNumber = value => typeof value === 'number' && Number.isFinite(value);
const clamp = (value, minimum, maximum) => Math.max(minimum, Math.min(maximum, value));

function geometry(viewportWidth, {navigationCollapsed = false, assistantOpen = false, widths = {}} = {}) {
  const viewport = finiteNumber(viewportWidth) ? Math.max(0, viewportWidth) : 0;
  const available = Math.max(0, viewport - 10);
  const preferences = widths && typeof widths === 'object' ? widths : {};
  const resizable = viewport > 820;
  const navigationPreference = finiteNumber(preferences.navigation) ? preferences.navigation : 184;
  const navigation = navigationCollapsed
    ? COLLAPSED_NAVIGATION_WIDTH
    : clamp(navigationPreference, COLUMN_LIMITS.navigation.min, COLUMN_LIMITS.navigation.max);
  const visible = {
    list: !(assistantOpen && viewport <= 1150),
    workspace: true,
    assistant: assistantOpen,
  };
  const contentAvailable = Math.max(0, available - navigation);
  const sideMinimum = (visible.list ? COLUMN_LIMITS.list.min : 0)
    + (visible.assistant ? COLUMN_LIMITS.assistant.min : 0);
  const workspaceMinimum = resizable
    ? Math.min(WORKSPACE_MINIMUM, Math.max(0, contentAvailable - sideMinimum))
    : 0;
  return {available, preferences, resizable, navigation, visible, contentAvailable, workspaceMinimum};
}

function distribute(total, names, weights) {
  const result = Object.fromEntries(names.map(name => [name, 0]));
  const weightTotal = names.reduce((sum, name) => sum + Math.max(0, weights[name] || 0), 0);
  if(total <= 0 || !names.length)return result;
  const effectiveWeights = weightTotal > 0 ? weights : Object.fromEntries(names.map(name => [name, 1]));
  const effectiveTotal = names.reduce((sum, name) => sum + Math.max(0, effectiveWeights[name] || 0), 0);
  let remainder = total;
  names.forEach((name,index) => {
    const share = index === names.length - 1
      ? remainder
      : total * Math.max(0, effectiveWeights[name] || 0) / effectiveTotal;
    result[name] = share;
    remainder -= share;
  });
  return result;
}

function referenceDefaults(referenceContent) {
  const minimumTotal = COLUMN_LIMITS.list.min + WORKSPACE_MINIMUM + COLUMN_LIMITS.assistant.min;
  if(referenceContent < minimumTotal) {
    return {list:COLUMN_LIMITS.list.min, workspace:Math.max(0,referenceContent-COLUMN_LIMITS.list.min-290), assistant:290};
  }
  const extra = distribute(referenceContent - minimumTotal, CONTENT_NAMES, DEFAULT_SLACK_WEIGHTS);
  return {
    list: COLUMN_LIMITS.list.min + extra.list,
    workspace: WORKSPACE_MINIMUM + extra.workspace,
    assistant: COLUMN_LIMITS.assistant.min + extra.assistant,
  };
}

function fitContent(targets, minimums, visibleNames, contentAvailable, priority) {
  const columns = Object.fromEntries(visibleNames.map(name => [name, Math.max(minimums[name], targets[name])]));
  const difference = contentAvailable - visibleNames.reduce((sum,name) => sum + columns[name], 0);
  const adjust = (names, amount, grow) => {
    if(amount <= 0 || !names.length)return amount;
    const weights = Object.fromEntries(names.map(name => [name, Math.max(0, columns[name] - minimums[name])]));
    const available = names.reduce((sum,name) => sum + weights[name], 0);
    const applied = grow ? amount : Math.min(amount, available);
    const changes = distribute(applied, names, grow && available === 0 ? DEFAULT_SLACK_WEIGHTS : weights);
    for(const name of names)columns[name] += (grow ? 1 : -1) * changes[name];
    return amount - applied;
  };

  if(difference < 0) {
    let deficit = -difference;
    const activeSide = priority === 'list' || priority === 'assistant';
    // A dragged boundary borrows from the adjacent reading surface first.
    // The far side is pushed only after the centre reaches its safe minimum.
    if(activeSide)deficit = adjust(['workspace'], deficit, false);
    const otherNames = visibleNames.filter(name => name !== priority && (!activeSide || name !== 'workspace'));
    deficit = adjust(otherNames, deficit, false);
    if(deficit > 0 && visibleNames.includes(priority))adjust([priority], deficit, false);
  } else if(difference > 0) {
    // Releasing space from either side always gives it back to the centre.
    const growthNames = priority === 'list' || priority === 'assistant'
      ? ['workspace'] : visibleNames.filter(name => name !== priority);
    adjust(growthNames.length ? growthNames : visibleNames, difference, true);
  }
  return columns;
}

/** Start each drag from the visible allocation, not stale or absent preferences.
 * Navigation/window fitting remains proportional; side dragging is centre-first.
 */
export function resizeColumns(viewportWidth, name, width, options = {}) {
  const current = resolveColumns(viewportWidth, options);
  const widths = {...options.widths};
  for(const key of CONTENT_NAMES)if(current[key])widths[key] = current[key];
  widths[name] = clamp(width, COLUMN_LIMITS[name].min, maximumColumnWidth(viewportWidth, name, options));
  return resolveColumns(viewportWidth, {...options, widths, priority:name});
}

/** Resolve visible column allocations without changing saved preferences.
 * The optional priority names the pane being actively resized. It keeps the
 * requested pane stable while other panes yield their slack above safe minima.
 * Compact pane visibility remains a CSS concern and reports resizable:false.
 */
export function resolveColumns(viewportWidth, {navigationCollapsed = false, assistantOpen = false, widths = {}, priority = null} = {}) {
  const state = geometry(viewportWidth,{navigationCollapsed,assistantOpen,widths});
  const {available,preferences,resizable,navigation,visible,contentAvailable,workspaceMinimum} = state;
  const visibleNames = CONTENT_NAMES.filter(name => visible[name]);
  const minimums = {list:COLUMN_LIMITS.list.min, workspace:workspaceMinimum, assistant:COLUMN_LIMITS.assistant.min};
  const referenceContent = Math.max(0, available - COLLAPSED_NAVIGATION_WIDTH);
  const defaults = referenceDefaults(referenceContent);
  const hasContentPreference = CONTENT_NAMES.some(name => finiteNumber(preferences[name]));
  const targets = {};
  for(const name of visibleNames) {
    // No content pane can use more than the full reference surface. Bounding
    // hostile saved values here also keeps proportional arithmetic precise.
    const maximum = Math.max(minimums[name],referenceContent);
    targets[name] = finiteNumber(preferences[name])
      ? clamp(preferences[name], minimums[name], maximum)
      : defaults[name];
  }
  if(!hasContentPreference) {
    // Closing a pane releases its default share to the reading surface. The
    // remaining side pane keeps the same visual width instead of swelling to
    // half the viewport.
    if(!visible.list)targets.workspace += defaults.list;
    if(!visible.assistant)targets.workspace += defaults.assistant;
  }
  // Older saved layouts have list/assistant preferences but no workspace value.
  // Bind their residual centre width to the collapsed-navigation reference so
  // expanding navigation can scale every pane's available slack proportionally.
  if(hasContentPreference && !finiteNumber(preferences.workspace)) {
    const sideTotal = visibleNames
      .filter(name => name !== 'workspace')
      .reduce((sum,name) => sum + targets[name], 0);
    targets.workspace = Math.max(workspaceMinimum, referenceContent - sideTotal);
  }
  const content = fitContent(targets,minimums,visibleNames,contentAvailable,priority);
  return {
    navigation,
    list: visible.list ? content.list : 0,
    assistant: visible.assistant ? content.assistant : 0,
    workspace: Math.max(0,content.workspace),
    resizable,
  };
}

/** Maximum reachable width for a pane when every other visible pane is safe. */
export function maximumColumnWidth(viewportWidth, name, options = {}) {
  const state = geometry(viewportWidth,options);
  if(name === 'navigation')return COLUMN_LIMITS.navigation.max;
  if(!['list','assistant'].includes(name) || !state.visible[name])return 0;
  const otherSideMinimum = name === 'list'
    ? (state.visible.assistant ? COLUMN_LIMITS.assistant.min : 0)
    : (state.visible.list ? COLUMN_LIMITS.list.min : 0);
  return Math.max(COLUMN_LIMITS[name].min,state.contentAvailable-state.workspaceMinimum-otherSideMinimum);
}

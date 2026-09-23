// Local synthetic recovery only. This module never calls a connector or restores public content.
const DESTINATIONS = new Set(['attention', 'prepared', 'waiting']);
const hasOwn = (value, key) => Object.prototype.hasOwnProperty.call(value, key);

function recoveryReason(destination, automatic) {
  if (!automatic) {
    return {
      attention: 'Ассистент вернул комментарий в «Нужно участие» по выбранному направлению.',
      prepared: 'Ассистент вернул комментарий в «Подготовлено» по выбранному направлению.',
      waiting: 'Ассистент вернул комментарий в «Ждём» по выбранному направлению.',
    }[destination];
  }
  return {
    attention: 'Нет явной причины ожидания или актуального черновика — нужно решение оператора.',
    prepared: 'Сохранённый черновик соответствует текущему контексту — можно продолжить подготовку.',
    waiting: 'Сохранена явная причина ожидания — комментарий возвращён в «Ждём».',
  }[destination];
}

function resolveDestination(record, destination) {
  if (destination !== 'auto') return DESTINATIONS.has(destination) ? destination : null;
  const {state} = record;
  if (typeof state.waitingReason === 'string' && state.waitingReason.trim()) return 'waiting';
  if (typeof state.draft === 'string' && state.draft.trim() && state.draftContext === record.context) return 'prepared';
  return 'attention';
}

function workflowSnapshot(state) {
  return {
    view: state.view,
    localRecoveryPresent: hasOwn(state, 'localRecovery'),
    localRecovery: hasOwn(state, 'localRecovery') ? structuredClone(state.localRecovery) : null,
  };
}

function tokenFor(receipt) {
  return JSON.stringify([
    receipt.version,
    receipt.itemId,
    receipt.source,
    receipt.target,
    receipt.reason,
    receipt.at,
    receipt.beforeWorkflow,
    receipt.resultWorkflow,
  ]);
}

function sameWorkflow(state, expected) {
  if (!expected || state.view !== expected.view) return false;
  const present = hasOwn(state, 'localRecovery');
  if (present !== expected.localRecoveryPresent) return false;
  return !present || JSON.stringify(state.localRecovery) === JSON.stringify(expected.localRecovery);
}

function validReceipt(receipt) {
  return Boolean(receipt && receipt.version === 1 && typeof receipt.itemId === 'string'
    && receipt.source === 'deleted' && DESTINATIONS.has(receipt.target)
    && receipt.beforeWorkflow?.view === 'deleted'
    && receipt.resultWorkflow?.view === receipt.target
    && typeof receipt.token === 'string' && receipt.token === tokenFor(receipt));
}

export function restoreInPrototype(record, at, destination = 'auto') {
  if (!record?.item || typeof record.item.id !== 'string' || !record.item.id
    || !record.state || record.state.view !== 'deleted'
    || !Array.isArray(record.messages)
    || !record.messages.some(message => message?.id === record.item.targetId)) return null;

  const target = resolveDestination(record, destination);
  if (!target) return null;

  const state = structuredClone(record.state);
  const beforeWorkflow = workflowSnapshot(state);
  const localRecovery = {at, actor:'Ассистент', prototype:true};
  state.view = target;
  state.localRecovery = localRecovery;

  const receipt = {
    version: 1,
    itemId: record.item.id,
    source: 'deleted',
    target,
    reason: recoveryReason(target, destination === 'auto'),
    at,
    beforeWorkflow,
    resultWorkflow: workflowSnapshot(state),
  };
  receipt.token = tokenFor(receipt);
  state.events = [...(Array.isArray(state.events) ? state.events : []), {
    type: 'restored_local',
    at,
    actor: 'Ассистент',
    prototype: true,
    source: receipt.source,
    target,
    reason: receipt.reason,
    restoreToken: receipt.token,
  }];

  return {state, receipt};
}

export function undoPrototypeRestore(record, receipt, at) {
  if (!record?.item || record.item.id !== receipt?.itemId || !record.state
    || !validReceipt(receipt) || !sameWorkflow(record.state, receipt.resultWorkflow)) return null;

  const lastEvent = Array.isArray(record.state.events) ? record.state.events.at(-1) : null;
  if (lastEvent?.type !== 'restored_local' || lastEvent.restoreToken !== receipt.token) return null;

  const state = structuredClone(record.state);
  state.view = receipt.beforeWorkflow.view;
  if (receipt.beforeWorkflow.localRecoveryPresent) {
    state.localRecovery = structuredClone(receipt.beforeWorkflow.localRecovery);
  } else {
    delete state.localRecovery;
  }
  state.events = [...state.events, {
    type: 'restore_undone',
    at,
    actor: 'Ассистент',
    prototype: true,
    source: receipt.target,
    target: receipt.source,
    restoreToken: receipt.token,
  }];
  return state;
}

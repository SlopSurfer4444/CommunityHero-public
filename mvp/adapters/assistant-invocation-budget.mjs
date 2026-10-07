// Native-funded invocation slots are transport metadata, never model input,
// currency authority, a wire-request count, or an independent attempt ledger.
import {AsyncLocalStorage} from 'node:async_hooks';
import {createHash} from 'node:crypto';
import {runProcess} from './process.mjs';

export const INVOCATION_BUDGET_ENV = 'COMMUNITYHERO_INVOCATION_BUDGET';
export const INVOCATION_BUDGET_CONTRACT = 'communityhero-codex-invocation-budget-v1';
const context = new AsyncLocalStorage();
const usedEnvelopes = new Set();
const fail = code => Object.assign(new Error(code), {code});
const short = value => typeof value === 'string' && value.length > 0 && value.length <= 160 && value.trim() === value && !/[\u0000-\u001f\u007f]/u.test(value);
const hash = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
// Bind the original bridge stdin bytes before JSON parsing can erase numeric
// lexical forms. Identity is private transport state; cloned model requests
// remain descendants of the already admitted scope and cannot mint a scope.
const wireBindings = new WeakMap();
export function bindInvocationWireRequest(request, wire) {
  if (!request || typeof request !== 'object' || Array.isArray(request)
    || !(typeof wire === 'string' || Buffer.isBuffer(wire)) || Buffer.byteLength(wire) > 8 * 1024 * 1024) throw fail('INVOCATION_WIRE_INVALID');
  let parsed; try { parsed = JSON.parse(String(wire)); } catch { throw fail('INVOCATION_WIRE_INVALID'); }
  const snapshot = JSON.stringify(request);
  if (JSON.stringify(parsed) !== snapshot) throw fail('INVOCATION_WIRE_MISMATCH');
  const binding = {sha256:createHash('sha256').update(wire).digest('hex'), snapshot};
  const old = wireBindings.get(request);
  if (old && (old.sha256 !== binding.sha256 || old.snapshot !== snapshot)) throw fail('INVOCATION_WIRE_REBOUND');
  wireBindings.set(request, binding);
  return request;
}
export function invocationRequestSha256(request) {
  const binding = wireBindings.get(request);
  if (!binding || binding.snapshot !== JSON.stringify(request)) throw fail('INVOCATION_WIRE_UNBOUND_OR_CHANGED');
  return binding.sha256;
}
const fields = ['version','contract','account','companyId','connectorBindingSha256','queueEpoch','policyId','policyRevision','nativeJobId','originalJobId','requestSha256','reservationId','slotIds'];

export function parseInvocationEnvelope(raw, request) {
  if (raw === undefined || raw === null || raw === '') return null;
  if (typeof raw !== 'string' || Buffer.byteLength(raw) > 16 * 1024) throw fail('INVOCATION_BUDGET_INVALID');
  let value; try { value = JSON.parse(raw); } catch { throw fail('INVOCATION_BUDGET_INVALID'); }
  if (!value || Array.isArray(value) || Object.keys(value).sort().join('|') !== fields.slice().sort().join('|')
    || value.version !== 1 || value.contract !== INVOCATION_BUDGET_CONTRACT
    || !['baw-russia','likeavto'].includes(value.account) || request?.account !== value.account
    || value.companyId !== (value.account === 'baw-russia' ? 'BAW Russia' : 'LikeAvto')
    || !['assistant','assistant_research'].includes(request?.operation ?? request?.op)
    || !['queueEpoch','policyId','nativeJobId','originalJobId','reservationId'].every(key => short(value[key]))
    || !['connectorBindingSha256','requestSha256'].every(key => hash(value[key]))
    || !Number.isSafeInteger(value.policyRevision) || value.policyRevision < 1
    || !Array.isArray(value.slotIds) || !value.slotIds.length || value.slotIds.length > 8
    || value.slotIds.some(slot => !short(slot)) || new Set(value.slotIds).size !== value.slotIds.length
    || value.requestSha256 !== invocationRequestSha256(request)) throw fail('INVOCATION_BUDGET_INVALID');
  return Object.freeze({...value, slotIds:Object.freeze([...value.slotIds])});
}

function usageFromOutput(output) {
  let terminalEvents = 0, usage = {status:'unavailable', basis:'codex_turn_completed_event'};
  for (const line of String(output ?? '').split('\n')) {
    if (!line.trim()) continue;
    let event; try { event = JSON.parse(line); } catch { continue; }
    if (event?.type !== 'turn.completed') continue;
    terminalEvents++;
    if (terminalEvents > 1) { usage = {status:'ambiguous', basis:'codex_turn_completed_event'}; continue; }
    const raw = event.usage;
    if (raw == null) continue;
    const keys = ['input_tokens','cached_input_tokens','output_tokens'];
    if (typeof raw !== 'object' || Array.isArray(raw) || !['input_tokens','output_tokens'].every(key => Object.hasOwn(raw,key))
      || keys.some(key => Object.hasOwn(raw,key) && (!Number.isSafeInteger(raw[key]) || raw[key] < 0))) {
      usage = {status:'invalid', basis:'codex_turn_completed_event'}; continue;
    }
    usage = {status:'observed', basis:'codex_turn_completed_event', ...Object.fromEntries(keys.filter(key => Object.hasOwn(raw,key)).map(key => [key,raw[key]]))};
  }
  return {terminalEvents, usage};
}

export async function withInvocationBudget(request, invoke, {raw = process.env[INVOCATION_BUDGET_ENV]} = {}) {
  if (context.getStore()) throw fail('INVOCATION_BUDGET_NESTED_SCOPE');
  const envelope = parseInvocationEnvelope(raw, request);
  if (!envelope) return invoke(); // Native bridge owns independent interactive admission.
  if (usedEnvelopes.has(envelope.reservationId) || usedEnvelopes.size >= 64) throw fail('INVOCATION_BUDGET_REPLAY');
  usedEnvelopes.add(envelope.reservationId);
  const scope = {envelope, invocations:[]};
  return context.run(scope, async () => {
    const result = await invoke();
    if (!result || typeof result !== 'object' || Array.isArray(result)) throw fail('INVOCATION_BUDGET_RESULT_INVALID');
    // Overwrite only adapter-owned metadata AFTER the model result was validated.
    // A model cannot manufacture its own budget observation/refund entitlement.
    const observation = {version:1, contract:INVOCATION_BUDGET_CONTRACT, reservationId:envelope.reservationId,
      nativeJobId:envelope.nativeJobId, originalJobId:envelope.originalJobId, requestSha256:envelope.requestSha256,
      issuedSlotIds:[...envelope.slotIds], invoked:scope.invocations.map(row => ({...row, usage:{...row.usage}})),
      unit:'codex_process_invocation', hardTokenCeiling:false, billableWireRequests:null, refundAuthorized:false};
    return {...result, runMetadata:{...(result.runMetadata ?? {}), invocationBudget:observation}};
  });
}

export async function runCodexWithInvocationBudget(executable, args, options, {runProcessFn = runProcess, stage = 'primary'} = {}) {
  const scope = context.getStore();
  const childOptions = {...options, env:{...(options?.env ?? process.env)}};
  delete childOptions.env[INVOCATION_BUDGET_ENV];
  if (!scope) return runProcessFn(executable, args, childOptions);
  if (!['primary','url_verification','visual_followup'].includes(stage)) throw fail('INVOCATION_BUDGET_STAGE_INVALID');
  const slotId = scope.envelope.slotIds[scope.invocations.length];
  if (!slotId) throw fail('INVOCATION_BUDGET_EXHAUSTED');
  // Native already durably issued/consumed this exact slot before this child
  // existed. Arm locally BEFORE calling the actual spawn, including throws.
  const row = {slotId, ordinal:scope.invocations.length + 1, stage, state:'armed',
    terminalEvents:0, usage:{status:'unavailable', basis:'codex_turn_completed_event'}};
  scope.invocations.push(row);
  try {
    const result = await runProcessFn(executable, args, childOptions);
    Object.assign(row, usageFromOutput(result?.stdout), {state:'returned'});
    return result;
  } catch (error) {
    row.state = 'unknown'; // A thrown process/transport error is never a refund.
    throw error;
  }
}

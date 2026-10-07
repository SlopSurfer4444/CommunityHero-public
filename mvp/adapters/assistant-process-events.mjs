// Codex exec JSONL: turn.failed ends the turn; top-level error is an
// unrecoverable stream error. An error *item* is explicitly non-fatal and can
// describe transport fallback or a tool failure while the turn continues.
// Observe the event for bounded diagnostics before invoking this guard.
export function assertAssistantEventContinuation(event) {
  if(event?.type!=='turn.failed'&&event?.type!=='error')return;
  // Reuse the process failure route: the stdout observer asks runProcess to
  // terminate/reap its child before rejection. Never retain private messages,
  // infer a cause, retry the model, or admit a partial result here.
  throw Object.assign(new Error('Codex reported a terminal stream failure'),{code:'ADAPTER_PROCESS_FAILED'});
}

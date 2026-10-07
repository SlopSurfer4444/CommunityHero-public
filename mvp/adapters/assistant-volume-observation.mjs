// Counts for one adapter generation. These are UTF-8 bytes and observed CLI
// usage, not a tokenizer estimate, wire/image size, cost or cache-hit claim.
const STAGES = new Set(['first_pass', 'stronger_review', 'editorial_review', 'discussion', 'research']);
const USAGE_FIELDS = ['input_tokens', 'cached_input_tokens', 'output_tokens'];
const bytes = value => Buffer.byteLength(value, 'utf8');

// Uses actual admitted adapter shapes. Public research has no private payload.
export function assistantVolumeForPreparedRequest(prepared, {stdin, instructions, schema, research = false}) {
  const stage = research ? 'research' : prepared.editorial ? 'editorial_review'
    : prepared.review ? 'stronger_review' : prepared.triage ? 'first_pass' : 'discussion';
  return assistantVolumeObservation({input:prepared.input, stdin, instructions, schema, stage,
    itemCount:prepared.payload?.items?.length ?? 0});
}

export function assistantVolumeObservation({input, stdin, instructions, schema, stage, itemCount}) {
  if (![input, stdin, instructions, schema].every(value => typeof value === 'string')
    || !STAGES.has(stage) || !Number.isSafeInteger(itemCount) || itemCount < 0 || itemCount > 100)
    throw new TypeError('Volume observation requires existing strings and a bounded generation identity');
  // Retain no prompt, schema, output, ID, URL or arbitrary event fields.
  const request = {version:1, basis:'utf8_existing_strings', scope:'initial_adapter_generation_only', callCount:1, stage, itemCount,
    contextBytes:bytes(input), stdinBytes:bytes(stdin), instructionBytes:bytes(instructions), schemaBytes:bytes(schema)};
  let completions = 0, usage = {status:'unavailable', basis:'codex_turn_completed_event'}, finished;
  return {
    observe(event) {
      if (finished || event?.type !== 'turn.completed') return;
      completions++;
      if (completions > 1) {usage = {status:'ambiguous', basis:'codex_turn_completed_event'}; return;}
      const raw = event.usage;
      if (raw === undefined || raw === null) return;
      if (typeof raw !== 'object' || Array.isArray(raw)
        || !['input_tokens', 'output_tokens'].every(key => Object.hasOwn(raw, key))
        || USAGE_FIELDS.some(key => Object.hasOwn(raw, key) && (!Number.isSafeInteger(raw[key]) || raw[key] < 0))) {
        usage = {status:'invalid', basis:'codex_turn_completed_event'}; return;
      }
      usage = {status:'observed', basis:'codex_turn_completed_event',
        ...Object.fromEntries(USAGE_FIELDS.filter(key => Object.hasOwn(raw, key)).map(key => [key, raw[key]]))};
    },
    finish(rawOutput) {
      if (!finished) {
        if (typeof rawOutput !== 'string') throw new TypeError('Volume observation requires the unexpanded structured-output string');
        finished = {...request, rawStructuredOutputBytes:bytes(rawOutput), terminalEventCount:completions, usage:{...usage}};
      }
      return {...finished, usage:{...finished.usage}};
    }
  };
}

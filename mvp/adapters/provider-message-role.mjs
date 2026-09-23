// Explicit provider ownership flags, never display-name guesses.
export function providerOfficial(value) { return value === true || value === 1; }
export function messageRole(actor, fallback) { return providerOfficial(actor?.providerOfficial) ? 'brand' : fallback; }

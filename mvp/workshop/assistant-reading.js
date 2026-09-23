// Reading position belongs to a conversation, not its selected comment/topic.
export function captureChatReading(node) {
  if (!node || node.clientHeight <= 0) return null;
  const top = Math.max(0, node.scrollTop);
  return {top, follow: node.scrollHeight - node.clientHeight - top <= 32};
}

export function restoreChatReading(node, reading) {
  if (!node || node.clientHeight <= 0) return;
  node.scrollTop = reading?.follow !== false
    ? Math.max(0, node.scrollHeight - node.clientHeight)
    : Math.min(reading.top, Math.max(0, node.scrollHeight - node.clientHeight));
}

// A deliberately small text formatter: only paired bold markers are markup.
// User/provider HTML and links remain inert text.
export function assistantText(value) {
  const escape = text => text.replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
  return String(value ?? '').split(/(\*\*[^*\n]+\*\*)/g).map(part =>
    part.startsWith('**') && part.endsWith('**') && part.length > 4
      ? `<strong>${escape(part.slice(2,-2))}</strong>` : escape(part)
  ).join('');
}

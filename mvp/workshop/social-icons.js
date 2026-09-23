const PLATFORMS = {
  vk: {
    label: 'VK',
    aliases: ['vk', 'vkontakte', 'вк', 'вконтакте'],
    svg: '<path d="M3.1 7.1h2.2c.2 0 .4.1.5.4.6 1.8 1.5 3.1 2.6 4.2.2.2.4.3.5.3.2 0 .3-.2.3-.5V7.8c0-.5.2-.7.6-.7h1.8c.4 0 .6.2.6.7v2.6c0 .4.1.6.3.6.1 0 .3-.1.5-.3.9-1 1.6-2.1 2.1-3.3.1-.2.3-.3.6-.3h2.2c.6 0 .8.3.5.8-.6 1.2-1.4 2.3-2.4 3.4-.3.3-.4.6-.1.9.3.4.8.8 1.3 1.4.5.5.9 1.1 1.2 1.6.3.5.1.8-.5.8h-2.1c-.4 0-.7-.1-.9-.4l-1.8-2c-.2-.2-.4-.3-.5-.2-.2 0-.3.2-.3.5v1.5c0 .4-.2.6-.7.6h-.9c-1.3 0-2.6-.7-3.9-2.1-1.3-1.4-2.3-3.3-3-5.6-.1-.5.1-.7.6-.7Z"/>'
  },
  youtube: {
    label: 'YouTube',
    aliases: ['youtube', 'you tube', 'yt', 'ютуб'],
    svg: '<path d="M20.6 7.2c-.2-.8-.9-1.5-1.7-1.7C17.4 5.1 12 5.1 12 5.1s-5.4 0-6.9.4c-.8.2-1.5.9-1.7 1.7C3 8.7 3 12 3 12s0 3.3.4 4.8c.2.8.9 1.5 1.7 1.7 1.5.4 6.9.4 6.9.4s5.4 0 6.9-.4c.8-.2 1.5-.9 1.7-1.7.4-1.5.4-4.8.4-4.8s0-3.3-.4-4.8Z"/><path class="social-icon__cutout" d="m10.2 14.9 4.7-2.9-4.7-2.9v5.8Z"/>'
  },
  instagram: {
    label: 'Instagram',
    aliases: ['instagram', 'insta', 'инстаграм'],
    svg: '<rect x="4.2" y="4.2" width="15.6" height="15.6" rx="4.7" fill="none" stroke="currentColor" stroke-width="2.2"/><circle cx="12" cy="12" r="3.7" fill="none" stroke="currentColor" stroke-width="2.2"/><circle cx="17.4" cy="6.8" r="1.15"/>'
  },
  tiktok: {
    label: 'TikTok',
    aliases: ['tiktok', 'tik tok', 'тикток', 'тик ток'],
    svg: '<path d="M13.4 3.4h2.7c.2 1.5 1.1 2.8 2.5 3.6.7.4 1.5.6 2.3.6v2.8c-1.6 0-3.1-.5-4.4-1.4v5.6a5.4 5.4 0 1 1-4.7-5.3v2.9a2.6 2.6 0 1 0 1.6 2.4V3.4Z"/>'
  },
  telegram: {
    label: 'Telegram',
    aliases: ['telegram', 'tg', 'телеграм', 'телега'],
    svg: '<path d="M20.5 4.1 17.7 19c-.2 1.1-.8 1.4-1.7.9l-4.3-3.2-2.1 2c-.2.2-.4.4-.9.4l.3-4.4 8-7.2c.3-.3-.1-.5-.5-.2l-9.9 6.2-4.3-1.3c-.9-.3-.9-.9.2-1.3l16.7-6.4c.8-.3 1.5.2 1.3 1.1Z"/>'
  }
};

const escapeAttribute = value => String(value ?? '').replace(/[&<>"']/g, character => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
})[character]);

const normalize = value => String(value ?? '')
  .normalize('NFKC')
  .trim()
  .toLocaleLowerCase('ru-RU')
  .replace(/[._-]+/g, ' ')
  .replace(/\s+/g, ' ');

const genericSvg = '<circle cx="12" cy="7" r="2.2"/><circle cx="6.5" cy="16.5" r="2.2"/><circle cx="17.5" cy="16.5" r="2.2"/><path d="m10.9 8.9-3.3 5.7m5.5-5.7 3.3 5.7M8.7 16.5h6.6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>';

/** Returns an accessible, self-contained platform mark for channel metadata. */
export function channelBadge(channel) {
  const normalized = normalize(channel);
  const entry = Object.entries(PLATFORMS).find(([, platform]) => platform.aliases.includes(normalized));
  const [slug, platform] = entry || ['other', null];
  const label = platform?.label || String(channel ?? '').trim() || 'Канал';
  const safeLabel = escapeAttribute(label);
  const svg = platform?.svg || genericSvg;

  return `<span class="channel social-icon social-icon--${slug}" role="img" aria-label="${safeLabel}" title="${safeLabel}"><svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">${svg}</svg></span>`;
}


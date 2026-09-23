const escapeHtml = value => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));

// These URLs come from a connector, but may also be restored from saved browser state.
// Reject obvious private hosts and executable URLs. DNS resolution is outside
// this renderer, so a public-looking hostname cannot be proven public here.
export function safeCommentMediaUrl(value) {
  if (typeof value !== 'string' || value.length > 8192) return null;
  try {
    const url = new URL(value);
    const host = url.hostname.replace(/\.$/,'').toLowerCase();
    if (url.protocol !== 'https:' || url.username || url.password || url.port ||
        !host.includes('.') || host === 'localhost' ||
        /\.(localhost|local|internal|lan|home|arpa)$/.test(host) ||
        /^\d+\.\d+\.\d+\.\d+$/.test(host) || host.includes(':') ||
        [...url.searchParams.keys()].some(key => /^(access_token|refresh_token|authorization|password|cookie)$/i.test(key))) return null;
    url.hash = '';
    return url.toString();
  } catch { return null; }
}

function normalizedMedia(attachments) {
  if (!Array.isArray(attachments)) return [];
  return attachments.slice(0,20).map(raw => {
    const media = raw && typeof raw === 'object' ? raw : {};
    const type = media.type === 'image' ? 'photo' : media.type;
    return {
      type: ['photo','sticker','video'].includes(type) ? type : 'unsupported',
      url: safeCommentMediaUrl(media.url),
      preview: safeCommentMediaUrl(media.preview_url ?? media.previewUrl ?? media.thumbnailUrl),
      source: safeCommentMediaUrl(media.source_url ?? media.sourceUrl),
      title: typeof media.title === 'string' ? media.title.slice(0,120) : ''
    };
  });
}

const typeLabel = type => ({photo:'Фото',sticker:'Стикер',video:'Видео',unsupported:'Вложение'})[type];

export function commentMediaHtml(attachments,{compact=false}={}) {
  const media = normalizedMedia(attachments);
  if (!media.length) return '';
  if (compact) {
    const first = media.find(entry => entry.type === 'photo' || entry.type === 'sticker' || entry.type === 'video') ?? media[0];
    const preview = first.type === 'video' ? first.preview : first.url;
    return `<span class="comment-media-queue" aria-label="Вложений: ${media.length}">${preview ? `<img src="${escapeHtml(preview)}" alt="" loading="lazy" decoding="async" referrerpolicy="no-referrer">` : '<span class="comment-media-queue-icon" aria-hidden="true">▧</span>'}<span>${media.length > 1 ? `${media.length} вложения` : typeLabel(first.type)}</span></span>`;
  }
  return `<div class="comment-media" aria-label="Вложения комментария">${media.map(entry => {
    const label = typeLabel(entry.type);
    const title = entry.title ? ` · ${entry.title}` : '';
    if ((entry.type === 'photo' || entry.type === 'sticker') && entry.url) {
      return `<button type="button" class="comment-media-tile ${entry.type==='sticker'?'is-sticker':''}" data-comment-image="${escapeHtml(entry.url)}" data-comment-image-label="${escapeHtml(label+title)}" aria-label="Увеличить ${escapeHtml(label.toLowerCase()+title)}"><img src="${escapeHtml(entry.url)}" alt="${escapeHtml(label+title)}" loading="lazy" decoding="async" referrerpolicy="no-referrer"><span>${escapeHtml(label)}</span></button>`;
    }
    if (entry.type === 'video') {
      const poster = entry.preview ? ` poster="${escapeHtml(entry.preview)}"` : '';
      const player = entry.url ? `<video controls playsinline preload="none"${poster} aria-label="${escapeHtml(label+title)}"><source src="${escapeHtml(entry.url)}"></video>` : entry.preview ? `<img class="comment-media-preview" src="${escapeHtml(entry.preview)}" alt="Превью видео${escapeHtml(title)}" loading="lazy" decoding="async" referrerpolicy="no-referrer">` : '';
      const destination = entry.source || entry.url;
      const link = destination ? `<a href="${escapeHtml(destination)}" target="_blank" rel="noopener noreferrer">${entry.source?'Открыть источник видео':'Открыть видео'}</a>` : '';
      return `<div class="comment-media-video">${player || `<span class="comment-media-unavailable">Видео недоступно</span>`}<div class="comment-media-caption"><span>${escapeHtml(label+title)}</span>${link}</div></div>`;
    }
    return `<span class="comment-media-unavailable">${escapeHtml(label)} недоступно</span>`;
  }).join('')}</div>`;
}

export function bindCommentVideoErrors(root) {
  root.querySelectorAll('.comment-media-video video').forEach(video => {
    const unavailable = () => {
      if (!video.isConnected) return;
      const fallback=document.createElement('span');
      fallback.className='comment-media-unavailable';fallback.textContent='Видео недоступно';
      video.replaceWith(fallback);
    };
    video.addEventListener('error',unavailable);
    video.querySelectorAll('source').forEach(source=>source.addEventListener('error',unavailable));
  });
}

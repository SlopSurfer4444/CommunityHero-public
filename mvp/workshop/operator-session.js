// Access codes live only in the submitted request; the server owns the session cookie.
import {workspaceBasePath,workspacePath} from './workspace-path.js';
import {createWorkspaceGenerationFence} from './workspace-generation.js';
const sessionBasePath=workspaceBasePath();
function operatorFrom(payload,response) {
  const actor = payload?.operator ?? payload;
  if (!actor || typeof actor.id !== 'string' || !actor.id || typeof actor.name !== 'string'
      || typeof actor.role !== 'string') throw new Error('invalid-session');
  return {
    id: actor.id,
    name: actor.name,
    role: actor.role,
    csrfToken: typeof payload?.csrfToken === 'string' ? payload.csrfToken : actor.csrfToken ?? '',
    storageGeneration:createWorkspaceGenerationFence().observe(payload,response,{initial:true}),
  };
}

async function readSession() {
  const response = await fetch(workspacePath('/api/session',sessionBasePath), { credentials: 'same-origin', cache: 'no-store' });
  if (response.status === 401) return null;
  // Only the pre-auth local development server may use this staged-rollout fallback.
  if (response.status === 404 && globalThis.location?.hostname === '127.0.0.1') {
    return { id: 'local-owner', name: 'Владелец', role: 'owner', csrfToken: '' };
  }
  if (!response.ok) throw new Error('session-unavailable');
  return operatorFrom(await response.json(),response);
}

/** Resolve only after this browser has its own authenticated operator session. */
export async function requireOperator(shell) {
  let initialError = false;
  try {
    const actor = await readSession();
    if (actor) return actor;
  } catch {
    initialError = true;
  }
  return new Promise(resolve => {
    const document = shell.ownerDocument;
    const surface = document.createElement('section');
    surface.className = 'operator-session';
    surface.setAttribute('aria-labelledby', 'operator-session-title');
    surface.innerHTML = `
      <form class="operator-session-card">
        <p class="operator-session-brand">CommunityHero</p>
        <h1 id="operator-session-title">Ваше рабочее место</h1>
        <p class="operator-session-intro">Войдите с личным кодом. Обсуждение с ассистентом будет доступно только вам.</p>
        <label for="operator-access-code">Код доступа</label>
        <input id="operator-access-code" name="access-code" type="password" autocomplete="off" autocapitalize="none" spellcheck="false" required placeholder="Введите личный код">
        <button type="submit">Войти</button>
        <p class="operator-session-status" role="status" aria-live="polite"></p>
        <p class="operator-session-note">Очередь комментариев и рабочие черновики общие для команды.</p>
      </form>`;
    shell.replaceChildren(surface);
    const form = surface.querySelector('form');
    const input = surface.querySelector('input');
    const button = surface.querySelector('button');
    const status = surface.querySelector('.operator-session-status');
    if (initialError) status.textContent = 'Сервер пока недоступен. Попробуйте войти чуть позже.';
    input.focus();
    let busy = false;
    form.addEventListener('submit', async event => {
      event.preventDefault();
      if (busy || !input.value.trim()) return;
      busy = true;
      button.disabled = true;
      button.textContent = 'Входим…';
      status.textContent = '';
      form.setAttribute('aria-busy', 'true');
      let token = input.value.trim();
      input.value = '';
      try {
        const request = fetch(workspacePath('/api/session/login',sessionBasePath), {
          method: 'POST',
          credentials: 'same-origin',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ token }),
        });
        token = '';
        const response = await request;
        if (!response.ok) {
          status.textContent = response.status === 429
            ? 'Слишком много попыток входа. Подождите немного и попробуйте снова.'
            : response.status === 401 || response.status === 403
              ? 'Код не подошёл. Проверьте его и попробуйте ещё раз.'
              : 'Не удалось войти. Попробуйте чуть позже.';
          return;
        }
        const actor = await readSession();
        if (!actor) throw new Error('session-not-established');
        surface.remove();
        resolve(actor);
      } catch {
        status.textContent = 'Не удалось связаться с сервером. Проверьте соединение и попробуйте снова.';
      } finally {
        token = '';
        busy = false;
        button.disabled = false;
        button.textContent = 'Войти';
        form.removeAttribute('aria-busy');
        if (surface.isConnected) input.focus();
      }
    });
  });
}

/** Revoke this browser's cookie session without touching shared drafts. */
export async function revokeOperatorSession(operator) {
  const response = await fetch(workspacePath('/api/session/logout',sessionBasePath), {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'X-CSRF-Token': operator?.csrfToken ?? '' },
  });
  if (!response.ok && response.status !== 401) throw new Error('Не удалось выйти. Попробуйте ещё раз.');
}

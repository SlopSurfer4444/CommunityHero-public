// The route locates an engine; its bootstrap/account response supplies authority.
export function workspaceBasePath(pathname = globalThis.location?.pathname || '/') {
  if (pathname === '/' || typeof pathname === 'string' && pathname.length <= 66
    && /^\/[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\/$/.test(pathname)) return pathname;
  throw Error('Некорректный адрес рабочего места. Откройте аккаунт через страницу выбора.');
}

export function workspacePath(path, base = workspaceBasePath()) {
  workspaceBasePath(base);
  if (typeof path !== 'string' || !path.startsWith('/') || path.startsWith('//')
    || /(?:^|\/)\.{1,2}(?:\/|\?|$)/.test(path) || path.includes('#')) throw Error('Некорректный путь рабочего места.');
  return `${base}${path.slice(1)}`;
}

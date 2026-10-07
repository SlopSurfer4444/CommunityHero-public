// Navigation leaves the entire company page. API requests never use these URLs.
import {workspaceBasePath,workspacePath} from './workspace-path.js';
import {canonicalWorkspaceGeneration} from './workspace-generation.js';
const accountId = /^[A-Za-z0-9_-]{1,64}$/;
const loopback = hostname => ['localhost','127.0.0.1','[::1]'].includes(hostname);

export function accountNavigation(payload, currentUrl) {
  const current = new URL(currentUrl);
  const currentPath = workspaceBasePath(current.pathname);
  if (typeof payload?.account !== 'string' || !payload.account.trim() || !Array.isArray(payload.accounts)) throw Error('Не удалось определить аккаунт рабочего места.');
  if (payload.basePath !== undefined && payload.basePath !== currentPath) throw Error('Адрес не соответствует рабочему месту.');
  const ids = new Set(), destinations = new Set();
  const accounts = payload.accounts.map(row => {
    if (!accountId.test(row?.id || '') || typeof row.label !== 'string' || !row.label.trim() || typeof row.url !== 'string') throw Error('Некорректный список аккаунтов.');
    const url = new URL(row.url);
    const destination = `${url.origin}${workspaceBasePath(url.pathname)}`;
    if (url.href !== row.url || row.url !== destination || url.username || url.password || url.search || url.hash
      || url.origin !== current.origin
      || !(url.protocol === 'https:' || url.protocol === 'http:' && loopback(url.hostname) && loopback(current.hostname))
      || ids.has(row.id) || destinations.has(destination)) throw Error('Некорректный адрес аккаунта.');
    ids.add(row.id); destinations.add(destination);
    return Object.freeze({id:row.id,label:row.label,url:url.href});
  });
  if (accounts.length && accounts.filter(row => row.url === `${current.origin}${currentPath}`).length !== 1) throw Error('Аккаунт не соответствует адресу рабочего места.');
  return Object.freeze({account:payload.account,basePath:currentPath,accounts:Object.freeze(accounts)});
}

export async function readAccountNavigation(fetcher = globalThis.fetch, currentUrl = globalThis.location.href) {
  const response = await fetcher(workspacePath('/api/accounts',workspaceBasePath(new URL(currentUrl).pathname)),{credentials:'same-origin',cache:'no-store'});
  if (!response.ok) throw Error('Не удалось определить аккаунт рабочего места. Обновите страницу.');
  return accountNavigation(await response.json(),currentUrl);
}

export function accountStorageKey(account, operatorId, generation=null) {
  canonicalWorkspaceGeneration(generation);
  return `communityhero-account-${encodeURIComponent(account)}-operator-${encodeURIComponent(operatorId)}-v1${generation?`-workspace-${generation}`:''}`;
}

export function loadAccountState(storage, account, operatorId, generation=null) {
  const key = accountStorageKey(account,operatorId,generation);
  let saved;
  try { saved = JSON.parse(storage.getItem(key) || 'null'); } catch {}
  if (!saved || typeof saved !== 'object' || Array.isArray(saved)) {
    saved = null;
    // A legacy origin-wide key is not company evidence. Retain it untouched;
    // import only state explicitly bound to this account and verified actor.
    const legacyKey = generation?null:operatorId === 'local-owner' ? 'communityhero-mvp-original-workshop-v1' : `communityhero-operator-${operatorId}-v1`;
    try {
      const legacy = legacyKey?JSON.parse(storage.getItem(legacyKey) || 'null'):null;
      if (legacy && typeof legacy === 'object' && !Array.isArray(legacy)
        && legacy.mvpAccount === account && legacy.mvpActorId === operatorId) {
        saved = legacy;
      }
    } catch {}
  }
  if (!saved || typeof saved !== 'object' || Array.isArray(saved) || saved.mvpAccount && saved.mvpAccount !== account
    || saved.mvpActorId && saved.mvpActorId !== operatorId
    || generation&&saved.mvpWorkspaceGeneration!==generation
    || !generation&&saved.mvpWorkspaceGeneration) saved = {};
  saved.mvpAccount = account;
  if(generation)saved.mvpWorkspaceGeneration=generation;
  return {key,saved};
}

export function bindAccountNavigation(header, navigation, beforeNavigate = () => {}) {
  const document = header.ownerDocument;
  const control = document.createElement('label');
  control.className = 'account-navigation';
  const caption = document.createElement('span');
  caption.className = 'sr-only'; caption.textContent = 'Аккаунт компании';
  control.append(caption);
  if (navigation.accounts.length < 2) {
    const current = document.createElement('span');
    current.className = 'account-navigation-current'; current.textContent = navigation.account;
    control.append(current); header.append(control); return control;
  }
  const select = document.createElement('select');
  select.setAttribute('aria-label','Аккаунт компании');
  for (const account of navigation.accounts) {
    const option = document.createElement('option');
    option.value = account.id; option.textContent = account.label;
    option.selected = new URL(account.url).origin === globalThis.location.origin
      && new URL(account.url).pathname === workspaceBasePath();
    select.append(option);
  }
  control.append(select); header.append(control);
  const currentId = select.value;
  select.addEventListener('change',() => {
    const destination = navigation.accounts.find(account => account.id === select.value);
    if (!destination || destination.id === currentId) return;
    select.disabled = true;
    try { beforeNavigate(destination); globalThis.location.assign(destination.url); }
    catch (error) { select.disabled = false; select.value = currentId; throw error; }
  });
  return control;
}

export function bindAccountPageRestore(surface,{isLeaving,getConnection,reload,onError=()=>{}}) {
  surface.addEventListener('pageshow',event=>{
    if(!event.persisted)return;
    if(isLeaving()){reload();return;}
    const connection=getConnection();
    if(connection){connection.start();void connection.refresh({background:true}).catch(onError);}
  });
}

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Activity, ArrowUp, Archive, BookOpen, Check, CheckCheck, ChevronDown, ChevronLeft, Clock3, FileText, History, Inbox, Layers3, LoaderCircle, Menu, MessageCircle, PanelRightClose, PanelRightOpen, Play, Plus, RefreshCw, Search, Send, Settings2, ShieldCheck, Sparkles, Square, SquareCheck, X } from 'lucide-react';
import brand from './brand.svg';

type Workflow = 'attention' | 'prepared' | 'waiting' | 'closed';
type View = Workflow | 'overview' | 'materials' | 'history' | 'settings';
type AnyRow = Record<string, unknown>;
type Item = AnyRow & { id: string; branchId?: string; targetId?: string; postKey?: string; workflow: Workflow; draft: string; revision: number; waitingReason?: string; dueAt?: string | null; createdAt?: string; title?: string; preview?: string };
type Post = AnyRow & { id: string; title?: string; text?: string; postKey?: string };
type Branch = AnyRow & { id: string; messages?: AnyRow[] };
type Message = { id: string; role: 'user' | 'assistant'; text: string; createdAt?: string; sources?: unknown[] };
type Conversation = { id: string; title: string; itemIds: string[]; messages: Message[] };
type Proposal = { id: string; itemId: string; kind: 'reply_and_close' | 'close'; text: string; revision: number; status: string; sources?: unknown[] };
type Material = { id: string; title: string; text: string; kind: string; revision: number; updatedAt?: string; sourceUrl?: string; postKey?: string };
type Job = AnyRow & { id: string; status?: string; kind?: string; error?: string };
type Data = { csrfToken: string; account: string; items: Item[]; posts: Post[]; branches: Branch[]; conversations: Conversation[]; proposals: Proposal[]; operations: AnyRow[]; materials: Material[]; jobs: Job[]; settings: AnyRow; sync: AnyRow };

const EMPTY: Data = { csrfToken: '', account: 'LikeAvto', items: [], posts: [], branches: [], conversations: [], proposals: [], operations: [], materials: [], jobs: [], settings: {}, sync: {} };
const NAV: { id: View; label: string; icon: typeof Inbox }[] = [
  { id: 'attention', label: 'Нужно участие', icon: Inbox },
  { id: 'prepared', label: 'Подготовлено', icon: CheckCheck },
  { id: 'waiting', label: 'Ждём', icon: Clock3 },
  { id: 'closed', label: 'Закрыто', icon: Archive },
  { id: 'overview', label: 'Обзор', icon: Layers3 },
  { id: 'materials', label: 'Материалы', icon: BookOpen },
  { id: 'history', label: 'История', icon: History },
  { id: 'settings', label: 'Настройки', icon: Settings2 },
];
const PAGE = 30;
function str(value: unknown): string { return value === null || value === undefined ? '' : String(value); }
function displayDate(value: unknown): string { const d = new Date(str(value)); return Number.isNaN(d.getTime()) ? str(value) : new Intl.DateTimeFormat('ru-RU', { dateStyle: 'short', timeStyle: 'short' }).format(d); }
function dateInput(value: unknown): string { const d = new Date(str(value)); if (Number.isNaN(d.getTime())) return ''; const local = new Date(d.getTime() - d.getTimezoneOffset() * 60000); return local.toISOString().slice(0, 16); }
const plainCache = new Map<string, string>();
function plain(value: unknown): string {
  const raw = str(value); if (!raw) return '';
  const cached = plainCache.get(raw); if (cached !== undefined) return cached;
  const markup = raw.replace(/<\s*br\s*\/?\s*>/gi, '\n').replace(/<\s*\/\s*(?:p|div|li)\s*>/gi, '\n').replace(/<\/?[a-z][^>]*>/gi, ' ');
  const decoder = document.createElement('textarea'); decoder.innerHTML = markup;
  const result = decoder.value.replace(/\u00a0/g, ' ').replace(/[\t ]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
  if (plainCache.size > 5000) plainCache.clear(); plainCache.set(raw, result); return result;
}
function coverageLabel(value: unknown): string { if (!value || typeof value !== 'object') return 'покрытие неизвестно'; const row = value as AnyRow; return row.complete === true ? 'страницы прочитаны полностью' : `ограниченная выборка${typeof row.perObjectLimit === 'number' ? `, до ${row.perObjectLimit} на источник` : ''}`; }
function syncLabel(value: unknown): string { switch (str(value)) { case 'completed': return 'Данные обновлены'; case 'running': return 'Синхронизация…'; case 'failed': return 'Ошибка синхронизации'; case 'never': return 'Ожидает синхронизации'; default: return 'Локальное рабочее место'; } }
function statusLabel(value: unknown): string { const raw = str(value); return ({ queued: 'В очереди', running: 'Выполняется', pending: 'Ожидает', processing: 'Обрабатывается', completed: 'Завершено', succeeded: 'Подтверждено', failed: 'Ошибка', cancelled: 'Отменено', interrupted: 'Прервано', unknown: 'Исход неизвестен', inconclusive: 'Не подтверждено', dispatching: 'Отправляется', approved: 'Одобрено', draft: 'Черновик', never: 'Ещё не запускалось' } as Record<string, string>)[raw.toLowerCase()] || raw || 'Неизвестно'; }
function materialKind(value: unknown): string { return ({ knowledge: 'Знание', transcript: 'Расшифровка', ocr: 'Текст с изображения' } as Record<string, string>)[str(value).toLowerCase()] || str(value); }
function jobKind(value: unknown): string { return ({ sync: 'Синхронизация', assistant: 'Ответ ассистента', materials: 'Импорт материалов', media: 'Обработка медиа', execute: 'Выполнение действий', reconcile: 'Сверка результата' } as Record<string, string>)[str(value).toLowerCase()] || str(value); }
function settingName(value: string): string { return ({ provider: 'Источник комментариев', assistant: 'Ассистент', externalWrites: 'Внешние действия' } as Record<string, string>)[value] || value; }
function settingValue(value: unknown): string { const raw = str(value); return ({ 'local-codex': 'Локальный Codex', 'explicit-confirmation': 'После подтверждения оператором' } as Record<string, string>)[raw] || statusLabel(raw); }
function field(row: AnyRow | undefined, ...keys: string[]): string { if (!row) return ''; for (const key of keys) { const v = row[key]; if (typeof v === 'string' && v.trim()) return plain(v); } return ''; }
function loadUi<T>(key: string, fallback: T): T { try { const raw = localStorage.getItem('ch-mvp-' + key); return raw ? JSON.parse(raw) as T : fallback; } catch { return fallback; } }
function saveUi(key: string, value: unknown) { try { localStorage.setItem('ch-mvp-' + key, JSON.stringify(value)); } catch { /* browser storage optional */ } }
function errorText(error: unknown) { return error instanceof Error ? error.message : str(error); }

export default function App() {
  const [data, setData] = useState<Data>(EMPTY);
  const [ready, setReady] = useState(false);
  const [fatal, setFatal] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState('');
  const [view, setView] = useState<View>(() => loadUi('view', 'attention'));
  const [query, setQuery] = useState(() => loadUi('query', ''));
  const [postFilter, setPostFilter] = useState(() => loadUi('post-filter', ''));
  const [page, setPage] = useState(1);
  const [selectedId, setSelectedId] = useState<string>(() => loadUi('selection', ''));
  const [draft, setDraft] = useState('');
  const [draftBuffers, setDraftBuffers] = useState<Record<string, string>>({});
  const [waitingReason, setWaitingReason] = useState('');
  const [dueAt, setDueAt] = useState('');
  const [mobileDetail, setMobileDetail] = useState(false);
  const [assistantOpen, setAssistantOpen] = useState(() => loadUi('assistant-open', false));
  const [conversationId, setConversationId] = useState<string>(() => loadUi('conversation', ''));
  const [chatText, setChatText] = useState('');
  const [attachSelected, setAttachSelected] = useState(false);
  const [chosenItems, setChosenItems] = useState<string[]>([]);
  const [chosenProposals, setChosenProposals] = useState<string[]>([]);
  const [reviewOpen, setReviewOpen] = useState(false);
  const [batchKind, setBatchKind] = useState<'reply_and_close' | 'close'>('reply_and_close');
  const [materialId, setMaterialId] = useState('');
  const [materialSearch, setMaterialSearch] = useState('');
  const [materialTitle, setMaterialTitle] = useState('');
  const [materialText, setMaterialText] = useState('');
  const [materialUrl, setMaterialUrl] = useState('');
  const [materialDirty, setMaterialDirty] = useState(false);

  const refresh = useCallback(async () => {
    const response = await fetch('/api/bootstrap', { credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error(`Не удалось загрузить данные (${response.status})`);
    const value = await response.json() as Partial<Data>;
    setData({ ...EMPTY, ...value, items: value.items ?? [], posts: value.posts ?? [], branches: value.branches ?? [], conversations: value.conversations ?? [], proposals: value.proposals ?? [], operations: value.operations ?? [], materials: value.materials ?? [], jobs: value.jobs ?? [], settings: value.settings ?? {}, sync: value.sync ?? {} });
    setReady(true); setFatal('');
  }, []);
  useEffect(() => { refresh().catch(e => { setFatal(errorText(e)); setReady(true); }); }, [refresh]);
  useEffect(() => { if (!ready) return; const es = new EventSource('/api/events'); const onRefresh = () => { refresh().catch(e => setNotice(errorText(e))); }; es.addEventListener('refresh', onRefresh); es.onerror = () => { /* reconnects automatically */ }; return () => { es.removeEventListener('refresh', onRefresh); es.close(); }; }, [ready, refresh]);
  useEffect(() => { if (!ready) return; const timer = window.setInterval(() => refresh().catch(() => undefined), 15000); return () => window.clearInterval(timer); }, [ready, refresh]);
  useEffect(() => saveUi('view', view), [view]);
  useEffect(() => saveUi('query', query), [query]);
  useEffect(() => saveUi('post-filter', postFilter), [postFilter]);
  useEffect(() => saveUi('selection', selectedId), [selectedId]);
  useEffect(() => saveUi('assistant-open', assistantOpen), [assistantOpen]);
  useEffect(() => saveUi('conversation', conversationId), [conversationId]);
  useEffect(() => setPage(1), [view, query, postFilter]);
  useEffect(() => { const dirty = Object.entries(draftBuffers).some(([id, text]) => text !== (data.items.find(x => x.id === id)?.draft || '')); if (!dirty) return; const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; }; window.addEventListener('beforeunload', warn); return () => window.removeEventListener('beforeunload', warn); }, [draftBuffers, data.items]);

  const api = useCallback(async <T,>(path: string, method: 'POST' | 'PATCH', body: unknown): Promise<T> => {
    const response = await fetch(path, { method, credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': data.csrfToken }, body: JSON.stringify(body) });
    const result = await response.json().catch(() => ({})) as AnyRow;
    if (!response.ok) throw new Error(field(result, 'error') || `Ошибка ${response.status}`);
    return result as T;
  }, [data.csrfToken]);
  const run = useCallback(async (label: string, fn: () => Promise<unknown>, success?: string) => {
    setBusy(label); setNotice('');
    try { await fn(); await refresh(); if (success) setNotice(success); }
    catch (e) { setNotice(errorText(e)); await refresh().catch(() => undefined); }
    finally { setBusy(''); }
  }, [refresh]);

  const selected = data.items.find(x => x.id === selectedId);
  const selectedPost = selected ? data.posts.find(p => p.id === str(selected.postId) || p.postKey === selected.postKey || p.id === selected.postKey) : undefined;
  const selectedBranch = selected ? data.branches.find(b => b.id === selected.branchId) : undefined;
  const target = selectedBranch?.messages?.find(m => str(m.id) === selected?.targetId) ?? selected;
  const conversation = data.conversations.find(c => c.id === conversationId);
  const itemLabel = (item: Item) => field(item, 'author', 'authorName') || field(data.branches.find(b => b.id === item.branchId)?.messages?.find(m => str(m.id) === item.targetId), 'author', 'authorName') || plain(item.title) || 'Комментарий';
  const itemText = (item: Item) => field(item, 'text', 'body') || field(data.branches.find(b => b.id === item.branchId)?.messages?.find(m => str(m.id) === item.targetId), 'text', 'body') || plain(item.preview) || '';
  const postTitle = (item: Item) => field(data.posts.find(p => p.id === str(item.postId) || p.postKey === item.postKey || p.id === item.postKey), 'title', 'text') || field(item, 'postTitle') || 'Публикация';

  const filtered = useMemo(() => data.items.filter(item => {
    if (item.workflow !== view) return false;
    if (postFilter && ![item.postKey, str(item.postId), postTitle(item)].includes(postFilter)) return false;
    const haystack = [itemLabel(item), itemText(item), postTitle(item), item.id].join(' ').toLocaleLowerCase('ru');
    return haystack.includes(query.trim().toLocaleLowerCase('ru'));
  }), [data.items, data.posts, data.branches, view, query, postFilter]);
  const visible = filtered.slice(0, page * PAGE);
  const counts = NAV.reduce((result, nav) => { if (['attention', 'prepared', 'waiting', 'closed'].includes(nav.id)) result[nav.id] = data.items.filter(x => x.workflow === nav.id).length; return result; }, {} as Record<string, number>);
  const activeJobs = data.jobs.filter(j => ['queued', 'running', 'pending', 'processing'].includes(str(j.status).toLowerCase()));
  const filteredMaterials = data.materials.filter(m => `${plain(m.title)} ${plain(m.text)} ${plain(m.kind)}`.toLocaleLowerCase('ru').includes(materialSearch.trim().toLocaleLowerCase('ru')));
  const reviewRows = data.proposals.filter(p => chosenProposals.includes(p.id) && p.status === 'draft');
  const reviewProblem = reviewRows.length > 50 ? 'За один раз можно выполнить не больше 50 предложений.' : new Set(reviewRows.map(p => p.itemId)).size !== reviewRows.length ? 'Выбрано несколько действий для одного комментария. Оставьте по одному.' : reviewRows.length !== chosenProposals.length ? 'Часть предложений уже недоступна. Снимите их выбор.' : '';
  const syncOpen = (data.sync.open && typeof data.sync.open === 'object' ? data.sync.open : {}) as AnyRow;
  const syncClosed = (data.sync.closed && typeof data.sync.closed === 'object' ? data.sync.closed : {}) as AnyRow;
  const scopeSync = view === 'closed' ? syncClosed : syncOpen;
  const syncMode = view === 'closed' ? 'closed' : 'open';
  function loadNext(mode: 'open' | 'closed') { const scope = mode === 'open' ? syncOpen : syncClosed; if (scope.hasMore !== true || !str(scope.cursor)) return; run('sync-page', () => api('/api/sync', 'POST', { mode, cursor: str(scope.cursor) }), 'Загрузка следующей страницы запущена'); }

  useEffect(() => { if (!selected) return; setDraft(draftBuffers[selected.id] ?? selected.draft ?? ''); setWaitingReason(selected.waitingReason || ''); setDueAt(dateInput(selected.dueAt)); }, [selected?.id]);
  useEffect(() => { if (!materialId) { setMaterialTitle(''); setMaterialText(''); setMaterialUrl(''); setMaterialDirty(false); return; } const m = data.materials.find(x => x.id === materialId); if (m) { setMaterialTitle(m.title); setMaterialText(m.text); setMaterialUrl(m.sourceUrl || ''); setMaterialDirty(false); } }, [materialId]);

  function selectItem(id: string) { setSelectedId(id); setMobileDetail(true); setChosenItems([]); }
  function toggleChoice(id: string, current: string[], setter: (value: string[]) => void) { setter(current.includes(id) ? current.filter(x => x !== id) : [...current, id]); }
  function workflowChange(next: Workflow) { if (!selected) return; run('workflow', () => api(`/api/items/${encodeURIComponent(selected.id)}`, 'PATCH', { expectedRevision: selected.revision, workflow: next, waitingReason: next === 'waiting' ? waitingReason : '', dueAt: next === 'waiting' ? (dueAt || null) : null }), 'Статус сохранён'); }
  function saveDraft() { if (!selected) return; const id = selected.id; run('draft', async () => { await api(`/api/items/${encodeURIComponent(id)}`, 'PATCH', { expectedRevision: selected.revision, draft }); setDraftBuffers(old => { const next = { ...old }; delete next[id]; return next; }); }, 'Черновик сохранён'); }
  function makeProposal(item: Item, kind: 'reply_and_close' | 'close', text = item.draft) {
    if (kind === 'reply_and_close' && !text.trim()) { setNotice('Для ответа нужен сохранённый текст черновика.'); return; }
    run('proposal', async () => { await api('/api/proposals', 'POST', { itemId: item.id, kind, text: kind === 'close' ? '' : text, expectedRevision: item.revision }); setAssistantOpen(true); }, 'Предложение создано. Проверьте его перед выполнением.');
  }
  function createBatch() {
    const rows = data.items.filter(x => chosenItems.includes(x.id));
    if (!rows.length) return;
    if (rows.length > 50) { setNotice('Для одной группы выберите не больше 50 комментариев.'); return; }
    if (batchKind === 'reply_and_close' && rows.some(x => !x.draft?.trim())) { setNotice('У каждого выбранного комментария должен быть сохранённый черновик.'); return; }
    run('batch', async () => { for (const item of rows) await api('/api/proposals', 'POST', { itemId: item.id, kind: batchKind, text: batchKind === 'close' ? '' : item.draft, expectedRevision: item.revision }); setChosenItems([]); setAssistantOpen(true); }, 'Предложения созданы. Проверьте каждого адресата и текст.');
  }
  function sendChat() {
    const text = chatText.trim(); if (!text) return;
    run('chat', async () => {
      let id = conversationId;
      if (!data.conversations.some(c => c.id === id)) { const c = await api<Conversation>('/api/conversations', 'POST', { title: 'Обсуждение', itemIds: [] }); id = c.id; setConversationId(id); }
      await api(`/api/conversations/${encodeURIComponent(id)}/messages`, 'POST', { text, itemIds: attachSelected && selected ? [selected.id] : (conversation?.itemIds || []) });
      setChatText(''); setAttachSelected(false);
    }, 'Сообщение отправлено ассистенту');
  }
  function submitReview() {
    const proposals = reviewRows;
    if (!proposals.length) return;
    if (reviewProblem) { setNotice(reviewProblem); return; }
    run('execute', async () => {
      const approval = await api<{ id: string }>('/api/approvals', 'POST', { proposals: proposals.map(p => ({ id: p.id, revision: p.revision })) });
      await api(`/api/approvals/${encodeURIComponent(approval.id)}/execute`, 'POST', {});
      setReviewOpen(false); setChosenProposals([]);
    }, 'Выполнение запущено. Результат смотрите в истории.');
  }
  function saveMaterial() {
    if (!materialTitle.trim() || !materialText.trim()) { setNotice('Укажите название и содержание материала.'); return; }
    const current = data.materials.find(x => x.id === materialId);
    run('material', async () => {
      if (current) await api(`/api/materials/${encodeURIComponent(current.id)}`, 'PATCH', { expectedRevision: current.revision, title: materialTitle, text: materialText });
      else { const created = await api<Material>('/api/materials', 'POST', { title: materialTitle, text: materialText, kind: 'knowledge', ...(materialUrl.trim() ? { sourceUrl: materialUrl.trim() } : {}) }); setMaterialId(created.id); }
      setMaterialDirty(false);
    }, 'Материал сохранён');
  }

  if (!ready) return <div className="centered"><LoaderCircle className="spin"/> Загружаем рабочее пространство…</div>;
  if (fatal) return <div className="centered error-screen"><img src={brand} alt=""/><h1>Не удалось открыть CommunityHero</h1><p>{fatal}</p><button onClick={() => refresh().catch(e => setFatal(errorText(e)))}>Повторить</button></div>;

  return <div className="app">
    <header className="topbar"><div className="brand"><img src={brand} alt=""/><strong>CommunityHero</strong><span>LikeAvto</span></div><div className="top-actions"><span className="sync-state"><span className={`state-dot ${field(data.sync, 'status') === 'error' ? 'bad' : ''}`}/>{syncLabel(data.sync.status)}</span><button className="icon-button" title="Обновить данные" onClick={() => run('sync', () => api('/api/sync', 'POST', {}), 'Синхронизация запущена')} disabled={!!busy}><RefreshCw size={16} className={busy === 'sync' ? 'spin' : ''}/></button><button className="icon-button assistant-toggle" title={assistantOpen ? 'Скрыть ассистента' : 'Открыть ассистента'} onClick={() => setAssistantOpen(!assistantOpen)}>{assistantOpen ? <PanelRightClose size={17}/> : <PanelRightOpen size={17}/>}</button></div></header>
    {notice && <div className="notice" role="status"><span>{notice}</span><button className="plain" aria-label="Закрыть уведомление" onClick={() => setNotice('')}><X size={15}/></button></div>}
    <div className="body">
      <aside className="nav"><div className="nav-label">РАБОЧЕЕ ПРОСТРАНСТВО</div><nav aria-label="Основное меню">{NAV.map((n, index) => { const Icon = n.icon; return <button key={n.id} aria-label={n.label} title={n.label} className={`nav-row ${view === n.id ? 'active' : ''} ${index === 4 ? 'nav-divider' : ''}`} onClick={() => { setView(n.id); setMobileDetail(false); }}><Icon size={17}/><span>{n.label}</span>{counts[n.id] !== undefined && <small>{counts[n.id]}</small>}</button>; })}</nav><div className="nav-foot"><div className="nav-avatar">О</div><div><strong>Оператор</strong><small>Локальная сессия</small></div></div></aside>
      <main className={`workspace ${mobileDetail ? 'mobile-detail' : ''}`}>
        {(['attention', 'prepared', 'waiting', 'closed'] as View[]).includes(view) ? <>
          <section className="queue"><div className="queue-head"><div className="section-title"><h1>{NAV.find(n => n.id === view)?.label}</h1><span>{filtered.length}</span></div><label className="search"><Search size={15}/><input aria-label="Найти комментарий" value={query} onChange={e => setQuery(e.target.value)} placeholder="Найти комментарий"/></label><select aria-label="Фильтр по публикации" value={postFilter} onChange={e => setPostFilter(e.target.value)}><option value="">Все публикации</option>{data.posts.map(p => <option key={p.id} value={p.postKey || p.id}>{plain(p.title) || plain(p.text).slice(0, 60) || p.id}</option>)}</select><div className="queue-meta">Локально загружено: {filtered.length} · показано {visible.length}{chosenItems.length > 0 && <span> · выбрано {chosenItems.length}</span>}</div></div>
            <div className="queue-scroll">{visible.length === 0 ? <div className="empty">Здесь пока нет комментариев по выбранным условиям.</div> : visible.map(item => <div key={item.id} className={`queue-card ${selectedId === item.id ? 'selected' : ''}`}><button className="check-cell" title="Выбрать для группы" aria-label={`Выбрать ${itemLabel(item)}`} onClick={() => toggleChoice(item.id, chosenItems, setChosenItems)}>{chosenItems.includes(item.id) ? <SquareCheck size={17}/> : <Square size={17}/>}</button><button className="queue-card-main" onClick={() => selectItem(item.id)}><span className="queue-card-top"><strong>{itemLabel(item)}</strong><time>{displayDate(item.createdAt)}</time></span><span className="queue-card-text">{itemText(item) || 'Текст комментария недоступен'}</span><span className="queue-card-post">{postTitle(item)}</span>{item.draft && <span className="tiny-tag">Черновик</span>}</button></div>)}{visible.length < filtered.length && <button className="load-more" onClick={() => setPage(page + 1)}>Показать ещё {Math.min(PAGE, filtered.length - visible.length)}</button>}{scopeSync.hasMore === true && <button className="load-more" onClick={() => loadNext(syncMode)} disabled={!!busy}>Загрузить следующую страницу LikeAvto</button>}</div>
            {chosenItems.length > 0 && <div className="batch-bar"><select aria-label="Действие для группы" value={batchKind} onChange={e => setBatchKind(e.target.value as typeof batchKind)}><option value="reply_and_close">Ответить и закрыть</option><option value="close">Только закрыть</option></select><button disabled={!!busy} onClick={createBatch}>Создать предложения ({chosenItems.length})</button><button className="plain" onClick={() => setChosenItems([])}>Снять выбор</button></div>}
          </section>
          <section className="detail">{selected ? <><div className="detail-head"><button className="mobile-back icon-button" onClick={() => setMobileDetail(false)}><ChevronLeft size={18}/></button><div><div className="eyebrow">ПУБЛИКАЦИЯ</div><h2>{plain(selectedPost?.title) || postTitle(selected)}</h2><p>{plain(selectedPost?.text)}</p></div><span className="pill">{NAV.find(n => n.id === selected.workflow)?.label}</span></div><div className="detail-scroll"><div className="branch-note">Ветка обсуждения · выбранный комментарий выделен. Фильтр списка не ограничивает контекст ветки.</div>{selectedBranch?.messages?.length ? selectedBranch.messages.map((m, i) => <div className={`message-card ${str(m.id) === selected.targetId ? 'target' : ''} ${str(m.role).toLowerCase().includes('brand') || str(m.author).includes('LikeAvto') ? 'brand-reply' : ''}`} key={str(m.id) || i}><div className="message-top"><span className="avatar">{field(m, 'author', 'authorName').slice(0, 1) || '•'}</span><strong>{field(m, 'author', 'authorName') || 'Участник'}</strong>{str(m.id) === selected.targetId && <span className="tiny-tag violet">Выбранный комментарий</span>}<time>{displayDate(m.createdAt)}</time></div>{field(m, 'parentText', 'replyToText') && <div className="reply-ref">↳ {field(m, 'parentText', 'replyToText')}</div>}<p>{field(m, 'text', 'body') || 'Текст недоступен'}</p></div>) : <div className="message-card target"><div className="message-top"><span className="avatar">{field(target, 'author', 'authorName').slice(0, 1) || '•'}</span><strong>{itemLabel(selected)}</strong><span className="tiny-tag violet">Выбранный комментарий</span></div><p>{itemText(selected) || 'Ветка пока недоступна'}</p></div>}
              <div className="item-controls"><div className="control-row"><label>Статус<select value={selected.workflow} onChange={e => workflowChange(e.target.value as Workflow)} disabled={!!busy || selected.workflow === 'closed'}><option value="attention">Нужно участие</option><option value="prepared">Подготовлено</option><option value="waiting">Ждём</option><option value="closed" disabled>Закрыто у провайдера</option></select></label><span className="muted">Версия {selected.revision}</span></div>{selected.workflow === 'waiting' && <div className="wait-fields"><label>Причина ожидания<input value={waitingReason} onChange={e => setWaitingReason(e.target.value)} placeholder="Что ждём?"/></label><label>Напомнить<input type="datetime-local" value={dueAt} onChange={e => setDueAt(e.target.value)}/></label><button disabled={!!busy} onClick={() => run('waiting', () => api(`/api/items/${encodeURIComponent(selected.id)}`, 'PATCH', { expectedRevision: selected.revision, waitingReason, dueAt: dueAt || null }), 'Ожидание сохранено')}>Сохранить ожидание</button></div>}</div>
            </div><div className="composer"><div className="composer-top"><strong>Кому: {itemLabel(selected)}</strong><span>Черновик · не отправлен</span></div><textarea aria-label="Черновик ответа" value={draft} onChange={e => { setDraft(e.target.value); setDraftBuffers(old => ({ ...old, [selected.id]: e.target.value })); }} placeholder="Напишите точный ответ…"/><div className="composer-actions"><button onClick={saveDraft} disabled={!!busy || draft === (selected.draft || '')}>Сохранить черновик</button><div className="spacer"/><button onClick={() => makeProposal(selected, 'close')} disabled={!!busy || selected.workflow === 'closed'} title="Создать предложение закрыть без ответа">Закрыть без ответа…</button><button className="send-button" title="Подготовить ответ к проверке" aria-label="Подготовить ответ к проверке" onClick={() => makeProposal(selected, 'reply_and_close', draft)} disabled={!!busy || selected.workflow === 'closed' || !draft.trim() || draft !== selected.draft}><ArrowUp size={20}/></button></div>{draft !== selected.draft && <div className="composer-hint">Сохраните изменённый черновик перед созданием предложения.</div>}</div></> : <div className="detail-empty"><MessageCircle size={28}/><h2>Выберите комментарий</h2><p>Откройте ветку и проверьте контекст перед решением.</p></div>}</section>
        </> : <section className="wide-view">
          {view === 'overview' && <><div className="view-head"><div><div className="eyebrow">LIKEAVTO</div><h1>Обзор работы</h1><p>Сводка по загруженным записям, не общие показатели аккаунта.</p></div><button onClick={() => run('sync', () => api('/api/sync', 'POST', {}), 'Синхронизация запущена')} disabled={!!busy}><RefreshCw size={15}/> Обновить</button></div><div className="metrics">{NAV.slice(0, 4).map(n => <button key={n.id} onClick={() => setView(n.id)}><span>{n.label}</span><strong>{counts[n.id]}</strong></button>)}</div><div className="view-grid"><div className="panel"><h2>Публикации</h2>{data.posts.length ? data.posts.map(p => <article className="post-line" key={p.id}><FileText size={17}/><div><strong>{plain(p.title) || p.id}</strong><p>{plain(p.text)}</p></div><button onClick={() => run('media', () => api('/api/materials/process', 'POST', { postId: p.id }), 'Обработка публикации запущена')} disabled={!!busy}>Обработать медиа</button></article>) : <p className="muted">Публикаций пока нет. Запустите синхронизацию.</p>}</div><div className="panel"><h2>Работа сейчас</h2><p>{activeJobs.length ? `${activeJobs.length} задач выполняется` : 'Нет выполняемых задач'}</p>{activeJobs.map(j => <div className="job-line" key={j.id}><LoaderCircle className="spin" size={15}/><span>{jobKind(j.kind) || j.id}</span><button onClick={() => run('cancel', () => api(`/api/jobs/${encodeURIComponent(j.id)}/cancel`, 'POST', {}), 'Запрошена отмена')} disabled={!!busy}>Отменить</button></div>)}<div className="coverage"><p>Открытые: {str(syncOpen.observedCount || 0)} записей в последнем чтении · {coverageLabel(syncOpen.coverage)}</p><p>Закрытые: {str(syncClosed.observedCount || 0)} записей в последнем чтении · {coverageLabel(syncClosed.coverage)}</p><p className="muted">Последнее обновление: {displayDate(syncOpen.lastSyncedAt || syncClosed.lastSyncedAt) || 'ещё не было'}</p>{syncOpen.hasMore === true && <button disabled={!!busy} onClick={() => loadNext('open')}>Загрузить ещё открытые</button>}{syncClosed.hasMore === true && <button disabled={!!busy} onClick={() => loadNext('closed')}>Загрузить ещё закрытые</button>}</div></div></div></>}
          {view === 'materials' && <><div className="view-head"><div><div className="eyebrow">ИСТОЧНИКИ ДЛЯ ОТВЕТОВ</div><h1>Материалы</h1><p>Знания и обработанные публикации, доступные ассистенту.</p></div><div className="head-actions"><button onClick={() => { setMaterialId(''); setMaterialTitle(''); setMaterialText(''); setMaterialUrl(''); setMaterialDirty(false); }}><Plus size={15}/> Новый</button><button onClick={() => run('import', () => api('/api/materials/import', 'POST', {}), 'Импорт материалов запущен')} disabled={!!busy}><RefreshCw size={15}/> Импорт LikeAvto</button></div></div><div className="materials-layout"><div className="panel material-list"><label className="material-search"><Search size={14}/><input aria-label="Поиск материалов" placeholder="Поиск по названию и тексту" value={materialSearch} onChange={e => setMaterialSearch(e.target.value)}/></label>{filteredMaterials.map(m => <button key={m.id} className={m.id === materialId ? 'active' : ''} onClick={() => { if (materialDirty && !window.confirm('Несохранённые изменения будут потеряны. Продолжить?')) return; setMaterialId(m.id); }}><span className="material-kind">{materialKind(m.kind)}</span><strong>{plain(m.title)}</strong><span className="material-preview">{plain(m.text).slice(0, 80) || 'Нет текста'}</span><small>{displayDate(m.updatedAt)}</small></button>)}{!filteredMaterials.length && <p className="muted">{data.materials.length ? 'Ничего не найдено.' : 'Материалов пока нет.'}</p>}</div><div className="panel material-editor"><h2>{materialId ? 'Редактировать материал' : 'Новый материал'}</h2><label>Название<input value={materialTitle} onChange={e => { setMaterialTitle(e.target.value); setMaterialDirty(true); }}/></label><label>Ссылка на источник (для нового материала)<input value={materialUrl} onChange={e => { setMaterialUrl(e.target.value); setMaterialDirty(true); }} disabled={!!materialId} placeholder="https://…"/></label><label>Содержание<textarea value={materialText} onChange={e => { setMaterialText(e.target.value); setMaterialDirty(true); }}/></label><div className="control-row"><button className="primary" onClick={saveMaterial} disabled={!!busy || !materialDirty}>Сохранить</button>{materialId && <span className="muted">Версия {data.materials.find(m => m.id === materialId)?.revision}</span>}</div></div></div></>}
          {view === 'history' && <><div className="view-head"><div><div className="eyebrow">ФАКТИЧЕСКИЕ ИСХОДЫ</div><h1>История</h1><p>Операции и задачи. Неизвестный результат требует сверки.</p></div></div><div className="panel"><h2>Операции</h2>{data.operations.length ? [...data.operations].reverse().map((op, i) => <article key={str(op.id) || i} className="history-line"><div><strong>{field(op, 'kind', 'action') || 'Действие'}</strong><span className="status-chip">{statusLabel(field(op, 'status', 'outcome'))}</span></div><p>{field(op, 'itemId', 'targetId', 'recipient')} · {field(op, 'text', 'error', 'message')}</p><small>{displayDate(op.createdAt || op.updatedAt)}</small>{['unknown', 'inconclusive'].includes(field(op, 'status', 'outcome').toLowerCase()) && <button onClick={() => run('reconcile', () => api(`/api/operations/${encodeURIComponent(str(op.id))}/reconcile`, 'POST', {}), 'Сверка запущена')} disabled={!!busy}>Сверить результат</button>}</article>) : <p className="muted">Действий пока не было.</p>}</div><div className="panel"><h2>Задачи</h2>{data.jobs.length ? [...data.jobs].reverse().map(j => <div className="job-line" key={j.id}><span>{jobKind(j.kind) || j.id}</span><span className="status-chip">{statusLabel(j.status)}</span>{j.error && <span className="error-text">{j.error}</span>}{['queued', 'running', 'pending', 'processing'].includes(str(j.status).toLowerCase()) && <button onClick={() => run('cancel', () => api(`/api/jobs/${encodeURIComponent(j.id)}/cancel`, 'POST', {}), 'Запрошена отмена')} disabled={!!busy}>Отменить</button>}</div>) : <p className="muted">Задач пока нет.</p>}</div></>}
          {view === 'settings' && <><div className="view-head"><div><div className="eyebrow">ЛОКАЛЬНОЕ РАБОЧЕЕ МЕСТО</div><h1>Настройки и состояние</h1><p>Подключения показываются без учётных данных. Рабочее место рассчитано на одного оператора; параллельная работа с существующим конвейером не координируется.</p></div></div><div className="view-grid"><div className="panel"><h2>Состояние</h2><dl className="setting-list"><dt>Аккаунт</dt><dd>{data.account}</dd>{Object.entries(data.settings).map(([key, value]) => <div key={key} className="setting-row"><dt>{settingName(key)}</dt><dd>{typeof value === 'object' ? JSON.stringify(value) : settingValue(value)}</dd></div>)}</dl></div><div className="panel"><h2>Резервная копия</h2><p className="muted">Сохранить данные текущего локального рабочего места.</p><button onClick={() => run('backup', async () => { const result = await api<{ path: string }>('/api/backup', 'POST', {}); setNotice(`Копия создана: ${result.path}`); })} disabled={!!busy}><Archive size={15}/> Создать копию</button></div></div></>}
        </section>}
      </main>
      <aside className={`assistant-panel ${assistantOpen ? 'is-open' : ''}`} aria-hidden={!assistantOpen} inert={!assistantOpen}><div className="assistant-head"><div><MessageCircle size={19}/><strong>Ассистент</strong></div><button className="icon-button" title="Закрыть ассистента" onClick={() => setAssistantOpen(false)}><X size={16}/></button></div><div className="conversation-tools"><select value={conversationId} onChange={e => setConversationId(e.target.value)} aria-label="Обсуждение"><option value="">Новое обсуждение</option>{data.conversations.map(c => <option key={c.id} value={c.id}>{c.title || c.id}</option>)}</select><button title="Новое обсуждение" onClick={() => setConversationId('')}><Plus size={16}/></button></div><div className="chat-scroll">{conversation?.itemIds?.length ? <div className="context-banner"><BookOpen size={14}/> Контекст беседы: {conversation.itemIds.map(id => plain(data.items.find(x => x.id === id)?.title) || id).join(', ')}</div> : <div className="context-banner">Новая беседа без прикреплённого комментария</div>}{conversation?.messages?.length ? conversation.messages.map((m, i) => <div key={m.id || i} className={`chat-bubble ${m.role}`}><span>{m.role === 'assistant' ? 'Ассистент' : 'Вы'}</span><p>{m.text}</p>{m.sources?.length ? <small>Источники: {m.sources.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' · ')}</small> : null}<time>{displayDate(m.createdAt)}</time></div>) : <div className="assistant-empty"><Sparkles size={24}/><p>Обсудите ответ или попросите подготовить предложение. Ассистент не выполняет действия за вас.</p></div>}{activeJobs.some(j => ['assistant', 'chat'].includes(str(j.kind).toLowerCase())) && <div className="chat-pending"><LoaderCircle className="spin" size={15}/> Ассистент отвечает…</div>}
          <div className="proposals"><div className="proposal-head"><h3>Предложения</h3><span>{data.proposals.filter(p => p.status === 'draft').length}</span></div>{data.proposals.filter(p => p.status === 'draft').map(p => <ProposalCard key={p.id} proposal={p} item={data.items.find(x => x.id === p.itemId)} selected={chosenProposals.includes(p.id)} toggle={() => toggleChoice(p.id, chosenProposals, setChosenProposals)} save={text => run('proposal-edit', () => api(`/api/proposals/${encodeURIComponent(p.id)}`, 'PATCH', { expectedRevision: p.revision, text }), 'Предложение обновлено')} busy={!!busy}/>)}{!data.proposals.some(p => p.status === 'draft') && <p className="muted">Предложений пока нет.</p>}</div></div><div className="assistant-bottom">{chosenProposals.length > 0 && <button className="review-button" onClick={() => setReviewOpen(true)}><ShieldCheck size={16}/> Проверить {chosenProposals.length} и выполнить…</button>}<label className="attach-row"><input type="checkbox" checked={attachSelected} onChange={e => setAttachSelected(e.target.checked)} disabled={!selected}/><span>{selected ? `Прикрепить выбранный комментарий: ${itemLabel(selected)}` : 'Выберите комментарий для прикрепления'}</span></label><div className="chat-composer"><textarea value={chatText} onChange={e => setChatText(e.target.value)} onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendChat(); } }} placeholder="Спросите ассистента…" aria-label="Сообщение ассистенту"/><button className="send-button" title="Отправить" aria-label="Отправить ассистенту" onClick={sendChat} disabled={!!busy || !chatText.trim()}><ArrowUp size={20}/></button></div><small>Enter — отправить · Shift+Enter — новая строка</small></div></aside>
    </div>
    {reviewOpen && <div className="modal-backdrop" role="presentation" onMouseDown={e => { if (e.target === e.currentTarget) setReviewOpen(false); }}><div className="review-modal" role="dialog" aria-modal="true" aria-labelledby="review-title"><div className="modal-head"><div><div className="eyebrow">ПОСЛЕДНЯЯ ПРОВЕРКА</div><h2 id="review-title">Подтвердить действия LikeAvto</h2></div><button className="icon-button" onClick={() => setReviewOpen(false)} aria-label="Закрыть"><X size={18}/></button></div><p className="review-warning">Следующая кнопка утвердит точные тексты и адресатов и запустит внешние действия. Проверьте каждый пункт.</p>{reviewProblem && <p className="review-error">{reviewProblem}</p>}<div className="review-scroll">{reviewRows.map(p => { const item = data.items.find(x => x.id === p.itemId); return <div className="review-card" key={p.id}><div><strong>{item ? itemLabel(item) : p.itemId}</strong><span>{p.kind === 'reply_and_close' ? 'Ответить и закрыть' : 'Закрыть без ответа'}</span></div><p className="recipient-text">{item ? itemText(item) : 'Исходный комментарий недоступен'}</p>{p.kind === 'reply_and_close' && <blockquote>{p.text}</blockquote>}<small>Адресат LikeAvto: {str(item?.itemId) || p.itemId} · предложение {p.id} · версия {p.revision}</small></div>; })}</div><div className="modal-actions"><button onClick={() => setReviewOpen(false)}>Вернуться</button><button className="primary" onClick={submitReview} disabled={!!busy || !chosenProposals.length || !!reviewProblem}><ShieldCheck size={16}/> Подтвердить и выполнить {chosenProposals.length}</button></div></div></div>}
  </div>;
}

function ProposalCard({ proposal, item, selected, toggle, save, busy }: { proposal: Proposal; item?: Item; selected: boolean; toggle: () => void; save: (text: string) => void; busy: boolean }) {
  const [text, setText] = useState(proposal.text);
  useEffect(() => setText(proposal.text), [proposal.text, proposal.id]);
  return <div className={`proposal-card ${selected ? 'chosen' : ''}`}><button className="proposal-select" onClick={toggle} aria-label={`Выбрать предложение ${proposal.id}`}>{selected ? <SquareCheck size={16}/> : <Square size={16}/>}</button><div className="proposal-body"><div className="proposal-meta"><strong>{item ? field(item, 'author', 'authorName', 'title') || item.id : proposal.itemId}</strong><span>{proposal.kind === 'reply_and_close' ? 'Ответить и закрыть' : 'Закрыть без ответа'}</span></div>{proposal.kind === 'reply_and_close' && <><textarea value={text} onChange={e => setText(e.target.value)} aria-label="Текст предложения"/><button onClick={() => save(text)} disabled={busy || text === proposal.text || !text.trim()}>Сохранить правку</button></>}{proposal.sources?.length ? <small>Источники: {proposal.sources.map(x => typeof x === 'string' ? x : JSON.stringify(x)).join(' · ')}</small> : null}</div></div>;
}

# Правила CommunityHero: адресный выбор из PostgreSQL

2026-09-23. Получатель: владелец текущего архитектурного аудита. Область: офлайн-код и design; текущая компания — только LikeAvto. Native API и экран/мастер настроек остаются backlog. Живая PostgreSQL восстанавливается основной задачей: здесь не выполнялись SQL, HTTP, нагрузка, тесты, установка или изменение исходников/схемы. Единственная запись — этот документ. SQL ниже является конкретным проектом следующего среза, не применённой миграцией.

## Решение

Сохранить `knowledge_entries.current_version_id` единственным каноническим указателем. Сохранить все `knowledge_versions`, материалы, provenance и ручные правки. Добавить неизменяемую типизированную проекцию версии правила и маленький адресный индекс **только текущих областей**; запрос правил читает этот индекс и точные выбранные версии. Ни `App::read()`, ни полный каталог/JSON workspace на этом пути не нужны.

Целевая единица SQL-изоляции — workspace одной компании; в пилоте это LikeAvto в `local-pilot`. Это не готовая tenancy: перед размещением 100 компаний в общей БД нужны стабильные разные workspace IDs, membership/авторизация, scoped connection binding, worker/cache isolation. Не вводить сейчас BAW, native APIs или onboarding UI ради проверки структуры. PostgreSQL может хранить такой индекс для 100 компаний, но текущий hardcoded runtime ещё не обслуживает их.

## Что подтверждено текущим кодом

| Текущий механизм | Точная опора | Вывод |
|---|---|---|
| Entries, immutable versions, current head принадлежат workspace; есть `(workspace_id,entry_id,id)` UNIQUE и deferred current-version FK | `mvp/server/migrations/0002_knowledge.sql:3-21` | Основу версий не нужно заменять второй системой правил |
| Индекс versions — `(workspace_id,entry_id)`; status/trust/scope/validity находятся в JSON | тот же файл `:31-37` | Нельзя ожидать адресного SQL-выбора применимых правил от существующего каталога |
| Каталог SQL агрегирует все entries и все historical versions | `storage_reads.rs:100-113`; вызов `operator_http.rs:83` | GET-каталог — browse/history API, не query API для каждого комментария |
| Catalog::new проверяет hash каждой версии и строит индекс всех versions | `knowledge.rs:421-483` | Полезная проверка integrity, но неподходящая стоимость на каждый rule lookup |
| select_catalog обходит все current entries, проверяет аккаунт/alias/post/time/trust, отделяет customer cases и media reuse | `knowledge.rs:587-691` | Семантику нужно сохранить; перенести candidate selection в SQL, а не просто LIMIT текущий массив |
| EvidenceContext лениво переиспользует проверенный каталог внутри одной immutable-фазы | `prepare_bundle.rs:10-28` | Исправляет повторное хеширование в одном шаге, не устраняет исходное чтение полного workspace |
| Редактирование требует expectedVersionId, добавляет новую immutable version и передвигает head; restore создаёт новую версию | `knowledge.rs:778-880` | Миграция сохраняет CAS и rollback semantics; история не переписывается |
| Импорт сохраняет ручное/существующее знание и пишет receipt даже при сохранённом head | `company_knowledge_import.rs:136-207` | Последняя импортированная версия не обязательно текущая: запрещено выбирать MAX(createdAt) вместо current_version_id |
| Подготовка закрепляет knowledgeManifest и проверяет его изменение | `prepare_bundle.rs:177-198,347-352,505` | Новый rule bundle встраивается в существующий dependency contract |
| Материал теряет companyImport и тип импортированного списка в adapter projection | `mvp/adapters/assistant.mjs:559-571` | Индекс сам по себе не исправляет allow/deny semantics; нужен типизированный runtime payload |

Числа 247 entries / 417 versions / 31 active rules взяты из уже подготовленного `project/rules-inventory-2026-09-23.md` (его наблюдение 11:30–11:40 MSK). Это исторический снимок внутри задачи, здесь не перепроверенный после сбоя. На объём миграции нельзя опираться на эти числа без новой разрешённой сверки.

## Типы содержания и область

- `action_policy`: КОГДА/ЧТО — ответить, не отвечать, исследовать, уточнить, передать оператору. Не разрешает внешнюю операцию само по себе.
- `style_policy`: КАК — длина, тон, обращение, юмор, эмодзи после выбора действия.
- `hard_constraint`: явный запрет или allowlist, например `forbidden_substrings`, `allowed_reply_urls`, `forbidden_reply_prefixes`. Оператор и значения передаются явно; обязательные ограничения не выбираются semantic top-k.
- `legacy_mixed`: действующее принятое руководство, которое пока смешивает КОГДА и КАК. Полный исходный текст сохраняется и передаётся один раз. Название «стиль» не позволяет автоматически вырезать поведенческие условия. Разделение — новая предложенная редакция с явным человеческим применением.
- Facts, references, transcripts/OCR и customer cases — отдельный evidence retrieval; их текст не становится инструкцией. Контакт с ролью/аккаунтом/платформой — факт; условие предложить этот контакт — policy, ссылающаяся на факт.

Области — company, company+platform, exact post, exact post+topic. Platform — дополнительное измерение, а не другая компания. Отсутствие достоверной platform-метки не означает «все платформы» для списка маршрутов. Topic IDs сейчас не реализованы: topic-записи не активируются до появления точной привязки к посту. Для текущей миграции используются только уже принятые company/post scopes; новые platform/topic ограничения не выводятся автоматически из текста.

Legacy `postAliases` нельзя превратить в company-scope при пустом `scope.postKeys`: `company_knowledge_import.rs:105-117` требует исходный namespace и Angry.Space/account binding. Resolver превращает alias только в существующий локальный post ID после точной проверки. Не найденный/чужой alias остаётся unresolved с объяснением; его правило не становится глобальным. В пакет включается версия resolver/binding. Native mapping в этом срезе не создаётся.

## Минимальный SQL-кандидат

Имена и типы ниже предлагаются для Rust-схемы `communityhero.*`. Это не Drizzle skeleton. Канонические версии не меняются. Projection version=1 фиксирует алгоритм интерпретации; смысловая правка создаёт новую каноническую версию, а новый compiler format требует отдельной миграции/версии projection. `policy` содержит явные `when`, `effect`, `exceptions`, `operator/values`, исходные text/source refs; `scopes` — проверенный нормализованный массив `{scope_kind,scope_key,platform_key}`. Company имеет `scope_key=''`; `platform_key='*'` только при принятой общей области.

```sql
-- Candidate only; apply only through the existing offline migration protocol.
ALTER TABLE communityhero.knowledge_entries
  ADD CONSTRAINT knowledge_entry_current_identity
  UNIQUE (workspace_id, id, current_version_id);

CREATE TABLE communityhero.rule_version_specs (
  workspace_id text NOT NULL,
  entry_id text NOT NULL,
  version_id text NOT NULL,
  projection_version integer NOT NULL CHECK (projection_version = 1),
  source_hash text NOT NULL CHECK (source_hash ~ '^[0-9a-f]{64}$'),
  spec_hash text NOT NULL CHECK (spec_hash ~ '^[0-9a-f]{64}$'),
  policy_class text NOT NULL CHECK (policy_class IN
    ('action_policy','style_policy','hard_constraint','legacy_mixed')),
  status text NOT NULL CHECK (status IN ('active','pending_review','retired')),
  trust text NOT NULL CHECK (trust IN
    ('verified','imported_policy','unverified','source_only')),
  valid_from timestamptz,
  valid_until timestamptz,
  scopes jsonb NOT NULL CHECK (jsonb_typeof(scopes) = 'array'),
  policy jsonb NOT NULL CHECK (jsonb_typeof(policy) = 'object'),
  PRIMARY KEY (workspace_id, version_id),
  UNIQUE (workspace_id, entry_id, version_id),
  FOREIGN KEY (workspace_id, entry_id, version_id)
    REFERENCES communityhero.knowledge_versions(workspace_id, entry_id, id)
    DEFERRABLE INITIALLY DEFERRED,
  CHECK (valid_until IS NULL OR valid_from IS NULL OR valid_until > valid_from)
);

CREATE TRIGGER rule_specs_immutable
BEFORE UPDATE OR DELETE ON communityhero.rule_version_specs
FOR EACH ROW EXECUTE FUNCTION communityhero.reject_history_rewrite();
CREATE TRIGGER rule_specs_no_truncate
BEFORE TRUNCATE ON communityhero.rule_version_specs
FOR EACH STATEMENT EXECUTE FUNCTION communityhero.reject_history_rewrite();

-- Disposable/derived current-head lookup, never a second canonical head.
CREATE TABLE communityhero.rule_head_scopes (
  workspace_id text NOT NULL,
  entry_id text NOT NULL,
  version_id text NOT NULL,
  scope_kind text NOT NULL CHECK (scope_kind IN ('company','post','topic')),
  scope_key text NOT NULL,
  platform_key text NOT NULL CHECK (length(platform_key) > 0),
  eligible boolean NOT NULL,
  PRIMARY KEY (workspace_id,entry_id,scope_kind,scope_key,platform_key),
  FOREIGN KEY (workspace_id,entry_id,version_id)
    REFERENCES communityhero.knowledge_entries(workspace_id,id,current_version_id)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY (workspace_id,entry_id,version_id)
    REFERENCES communityhero.rule_version_specs(workspace_id,entry_id,version_id)
    DEFERRABLE INITIALLY DEFERRED,
  CHECK ((scope_kind = 'company' AND scope_key = '') OR
         (scope_kind IN ('post','topic') AND length(scope_key) > 0))
);
CREATE INDEX rule_scope_lookup ON communityhero.rule_head_scopes
  (workspace_id,scope_kind,scope_key,platform_key,entry_id,version_id)
  WHERE eligible;

CREATE TABLE communityhero.rule_catalog_state (
  workspace_id text PRIMARY KEY REFERENCES communityhero.workspaces(id),
  epoch bigint NOT NULL CHECK (epoch >= 0),
  projection_version integer NOT NULL CHECK (projection_version = 1),
  coverage_state text NOT NULL CHECK (coverage_state IN ('building','ready'))
);

CREATE TABLE communityhero.rule_changes (
  workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
  epoch bigint NOT NULL,
  entry_ids jsonb NOT NULL CHECK (jsonb_typeof(entry_ids) = 'array'),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (workspace_id,epoch)
);
```

Это проект с двумя слоями integrity: immutable typed version, и пересоздаваемый current index. FK не позволяет scope row указывать старый head, но **FK сам не гарантирует полноту индекса**. До использования SQL-selector обязателен deferred constraint trigger на head/spec/scope mutations либо эквивалентный единственный DB mutation API с закрытым прямым DML: для каждого затронутого текущего rule/policy проверять наличие spec, source_hash равен canonical version.hash, status/trust/time совпадают с version, множество нормализованных scope rows равно spec.scopes, eligible равно active AND trusted. `coverage_state='ready'` само по себе такой проверкой не является. В canonical payload нет принятой новой семантики — проекция только `legacy_mixed` или доказанное известное поле, не выдуманный effect.

Для понятного unresolved alias нельзя вставлять company row: spec хранит unresolved provenance и selector возвращает отдельную диагностику для затронутой области. Первоначальное `ready` выдаётся только после сравнения полного inventory heads с spec/index и списка сохранённых unresolved scopes. Активация нового обязательного правила с unresolved scope отклоняется. Для старого unresolved источника сохраняется прежнее неприменение, без тихого расширения.

### Индексированный запрос

Параметры получены из авторизованного server context: `$1` workspace, `$2` canonical post ID или NULL, `$3` validated topic ID или NULL, `$4` platform, `$5` timestamp, `$6` максимальное число metadata-кандидатов **плюс один** для обнаружения превышения. UI/model не выбирают чужой workspace или произвольные scope tokens. Сначала проверить `rule_catalog_state.coverage_state='ready'` и projection version.

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
SELECT epoch, projection_version, coverage_state
FROM communityhero.rule_catalog_state WHERE workspace_id = $1;

WITH targets(scope_kind, scope_key) AS (
  VALUES ('company'::text, ''::text), ('post', $2::text), ('topic', $3::text)
), selected AS (
  SELECT DISTINCT s.entry_id, s.version_id
  FROM targets t
  JOIN communityhero.rule_head_scopes s
    ON s.workspace_id = $1 AND s.scope_kind = t.scope_kind
   AND s.scope_key = t.scope_key
  WHERE t.scope_key IS NOT NULL AND s.eligible
    AND s.platform_key IN ('*', $4)
)
SELECT r.entry_id, r.version_id, r.spec_hash, r.source_hash,
       r.policy_class, r.valid_from, r.valid_until,
       octet_length(r.policy::text) AS policy_bytes
FROM selected c
JOIN communityhero.rule_version_specs r
  ON r.workspace_id = $1 AND r.entry_id = c.entry_id
 AND r.version_id = c.version_id
WHERE r.status = 'active' AND r.trust IN ('verified','imported_policy')
  AND (r.valid_from IS NULL OR r.valid_from <= $5)
  AND (r.valid_until IS NULL OR r.valid_until > $5)
ORDER BY r.entry_id, r.version_id
LIMIT $6;

-- After checking count/byte budgets, fetch precisely these version IDs.
SELECT r.entry_id, r.version_id, r.policy, r.spec_hash, r.source_hash
FROM communityhero.rule_version_specs r
WHERE r.workspace_id = $1 AND r.version_id = ANY($2::text[])
ORDER BY r.entry_id, r.version_id;
COMMIT;
```

Здесь `$2` второго prepared statement — массив выбранных version IDs, отдельный набор bind-параметров. Исторические versions и материалы не агрегируются. Индекс содержит только текущие scopes, поэтому рост history не увеличивает число candidate bindings. `octet_length(policy::text)` может распаковать выбранные JSONB, но не передаёт тела в Rust до проверки бюджета; при необходимости байты становятся проверяемой immutable колонкой в spec. Отдельный запрос метаданных ближайшей будущей `valid_from` и `valid_until` по тем же scope keys определяет cache deadline; его нельзя вычислять только из уже активных rows.

LIMIT не означает «первые N правил применены». Если кандидатов оказалось N+1 или сумма bytes превышает бюджет, сборка возвращает `policy_scope_over_budget`, не отправляет усечённый набор модели и не разрешает публикацию. Все обязательные rules/исключения выбранной области сохраняются. Natural-language WHEN, который нельзя доказуемо вычислить кодом, не отсекается машинно: передаётся как часть применимого набора компании/поста.

SQL возвращает адресных кандидатов; финальный compiler также проверяет сохранённые в immutable policy provenance legacy binding/namespace и resolver revision. Изменение коннектора не даёт права использовать старый alias даже при совпавшем post ID. Неизвестная/изменённая alias mapping означает явное неприменение либо необходимость review, а не автоматический перенос. Если переназначение alias действительно принято, оно требует новой версии/явной миграции соответствующей projection, а не UPDATE immutable spec.

Если требуется несколько posts, входной scope-set ограничен; использовать bound массив/VALUES, дедуплицировать по `(entry_id,version_id)`. Проверять head/schema generation в том же snapshot. EXPLAIN и индексы ещё не измерялись; пригодность плана должна подтверждаться позже на isolated fixture, не на восстанавливаемой БД.

## Неизменяемый bundle и cache

Bundle — канонически упорядоченные `{entryId,versionId,sourceHash,specHash,policyClass,matchedScope}` + compiler/selector/resolver versions + workspace/account/binding identity + контекст platform/post/topic. Его digest вычисляется над явной canonical serialization, не над случайным порядком JSON. Original source hash использует прежнюю Rust-семантику `version_hash` (`knowledge.rs:403-410`); её нельзя заменить PostgreSQL JSON-text hash и объявить тем же значением.

Для каждого подготовленного ответа сохраняются `ruleBundleDigest` и точные версии; текущий `knowledgeManifest` для facts/media сохраняется отдельно. Существующие старые proposals/approvals/UNKNOWN не переписываются. Их старый формат остаётся читаемым; новые approvals требуют fresh-check соответствующего supported bundle format.

Минимальное durable хранение можно добавить к существующему prepareBundle.request без новой таблицы. Если повторяемость больших rule payloads станет существенной, отдельная immutable таблица допустима:

```sql
CREATE TABLE communityhero.rule_bundles (
  workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
  digest text NOT NULL CHECK (digest ~ '^[0-9a-f]{64}$'),
  manifest jsonb NOT NULL CHECK (jsonb_typeof(manifest) = 'object'),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY(workspace_id,digest)
);
CREATE TRIGGER rule_bundles_immutable
BEFORE UPDATE OR DELETE ON communityhero.rule_bundles
FOR EACH ROW EXECUTE FUNCTION communityhero.reject_history_rewrite();
CREATE TRIGGER rule_bundles_no_truncate
BEFORE TRUNCATE ON communityhero.rule_bundles
FOR EACH STATEMENT EXECUTE FUNCTION communityhero.reject_history_rewrite();
```

Не создавать её только для удобства: уже закреплённый manifest достаточен для первого LikeAvto-среза. При отдельной таблице manifest подтверждается в одной транзакции с proposal; DB FK или отдельная validation проверяет точные version refs.

Cache key: `(database/dataset identity, workspace_id, account_id, connector_binding_id+revision, rule_epoch, projection_version, compiler_version, resolver_version, platform, sorted post/topic IDs)`. Dataset identity нужна после restore/clone: одинаковый `local-pilot` не делает две БД одним кэшем. Cache хранит immutable IDs/specs, а не общий mutable workspace. TTL истекает не позже ближайшей границы validity, в том числе будущей активации; в остальном TTL — оптимизация, не источник истины. Перед использованием читать маленький epoch или проверенную свежую generation-ссылку; потерянное уведомление не даёт вечного устаревшего cache hit.

Согласованный read snapshot может устареть сразу после чтения: это нормально для подготовки, но не для fresh approval. При сохранении/принятии результата повторно проверить актуальный scoped bundle fingerprint и source revisions под writer transaction; изменившаяся policy не становится принятой через сравнение только старого cached epoch.

Активация правила меняет tenant epoch атомарно, записывает rule_changes и invalidation event после commit. Простое NOTIFY — необязательный wake-up, не durable truth. Изменения company rule могут затронуть все ещё неопубликованные предложения компании; изменение post/topic затрагивает соответствующие предметные области. Проверять по bundle/scope fingerprints, чтобы чужой пост не становился stale из-за одного общего epoch. Ручной текст не перезаписывается: proposal теряет ready-state либо требует review, human draft сохраняется. UNKNOWN/send receipts не превращаются в retry.

Для исключений/condition conflicts нет правила «последняя строка победила»: hard constraints сохраняются; несовместимые инструкции активируются только с явным принятым разрешением конфликта или блокируют затронутое действие. Политика КАК не расширяет полномочия КОГДА/ЧТО, а оба слоя не заменяют connector capabilities и exact approval.

## Атомарное применение версии

Сохраняется существующий writer lease и workspace row lock. В первой реализации не менять одновременно concurrency model. В одной write transaction:

1. Взять workspace row FOR UPDATE и проверить account/binding. Прочитать ровно entry/current version и expectedVersionId, операторскую authority, идемпотентный request ID.
2. Создать новую каноническую knowledge_version по текущему hash/provenance/supersedes-контракту. Историю, старый source, старые drafts не менять.
3. Записать immutable rule_version_specs и все нормализованные scopes; проверить полноту/непротиворечивость shape, case examples и source semantics. Новая редакция по умолчанию pending, пока пользователь явно не применил её.
4. Удалить только derived rule_head_scopes этой entry; CAS-update current_version_id **и JSON payload.currentVersionId/status/scope/kind** в knowledge_entries, потом вставить текущие scope rows из spec. Нельзя обновить только relational head и оставить payload старым: общий storage read проверяет проекции.
5. Проверить deferred constraints и полный projection contract, увеличить rule_catalog_state.epoch ровно один раз, записать rule_changes/audit. Commit, затем cache wake-up. Ошибка любого шага откатывает всё.

Точная форма CAS для существующей entry:

```sql
UPDATE communityhero.knowledge_entries
SET current_version_id = $3, payload = $4::jsonb
WHERE workspace_id = $1 AND id = $2 AND current_version_id = $5;
-- Must affect exactly one row, otherwise rollback with version conflict.
UPDATE communityhero.rule_catalog_state SET epoch = epoch + 1
WHERE workspace_id = $1 AND coverage_state = 'ready'
RETURNING epoch;
-- Must affect exactly one row; append rule_changes and existing audit in same tx.
```

`$4` — проверенный полный новый entry payload, сохраняющий receipts/неизвестные поля. INSERT новой entry/version учитывает существующие deferred cyclic FK. Ordinal allocation остаётся под writer/workspace serialization. Rollback правила — новая версия, использующая выбранный исторический текст с прежней защитой `sourceHash/observedSourceRevision`, не возврат DB snapshot и не удаление истории.

## Миграция без потери текущих правил

1. Сначала восстановить сервис и получить отдельное разрешение на DB-изменения. В этом документе его нет. Принять актуальный backup/restore schema v3; старый recovery-inventory имеет отдельную известную v2-проверку и не служит автоматической приёмкой нового schema version.
2. На изолированной копии снять только метаданные identity/current heads/hash/scope/trust/status; сохранить контрольный manifest всех версий и ручных материалов. Не считать snapshot из rules-inventory текущим после recovery.
3. Добавить таблицы/индексы в следующей свободной schema migration, не назначать номер v4 без сверки других изменений. Для LikeAvto backfill **без смены current heads и без изменения canonical payloads**. История остаётся в существующих таблицах. Для текущих rule/policy versions создать точные typed specs; смешанные тексты legacy_mixed, известные списки явно allow/deny; неизвестная семантика остаётся candidate/diagnostic. Смысловая переработка не смешивается с storage migration.
4. Нормализовать existing direct postKeys/legacy aliases через scoped mapping. Сверить active set, exclusions, time windows, trust и source refs с прежним selector на ограниченных offline cases. Не распространять instructions на похожие заголовки/медиа; media reuse остаётся другим контрактом.
5. Реализовать узкий rule-query repository и отдельный immutable bundle builder. Убрать полный read именно из rule-query call path; оставить browse/history API отдельным с pagination. Если модельный запрос всё ещё строит полный workspace для других источников, честно отметить это отдельным долгом, а не выдавать rule-query fix за полное ускорение ассистента.
6. Все head writers должны одновременно обновлять index: manual instruction, revise/restore, company import, sync_catalog, background/media catalog update, migration/maintenance tools. Старый общий TABLES writer сейчас не знает новых derived таблиц. Переключать его нельзя до adapters/guard coverage; иначе selector получит stale/incomplete index. Установка deferred guard делает неподдержанный старый writer fail-closed, но означает, что старый binary не является безопасным rollback-планом после cutover.
7. Read-only shadow сравнение по LikeAvto с ограниченным числом случаев; при parity установить coverage ready и переключить query path атомарной release/config границей. До этой границы старый selector остаётся текущим, новый не подменяет выдачу. После cutover ошибки индекса не вызывают тихий fallback к полному workspace или предположение «правил нет».
8. Rollback среза: версия приложения, совместимая с индексом/guards, либо явный offline план отката schema/guard после сохранённого snapshot. Не перемещать heads назад для «ускоренного отката». Ручные edits и новые receipts всегда сохраняются.

## Приёмка с ограниченным ресурсным профилем

Никакой нагрузки сейчас. После восстановления и разрешения — offline fixtures, затем disposable PostgreSQL. Профили ниже — верхние границы эксперимента, не обещания production capacity.

| Профиль | Данные / параллельность | Что доказывает |
|---|---|---|
| Семантический LikeAvto | Один tenant; текущие принятые правила и отдельные synthetic allow/deny/time/alias cases; serial | Старые heads/история сохранены; КОГДА/КАК/факты не перепутаны; pending/foreign/unresolved не активировались |
| History independence | Один tenant, одинаковые current rules, 1x и 100x **синтетическая** история при заранее ограниченном размере fixture | Rule query не читает historical payloads; rows/bytes ответа и число запросов зависят от selected current rules |
| Tenant isolation | 100 синтетических tenants, например до 50 rules на tenant; сначала 1, затем максимум 4 одновременных read-запроса, короткий фиксированный запуск | Одинаковые IDs/text/scopes не пересекают workspace; SQL-план использует workspace/scope index; чужие компании не увеличивают выдаваемые bytes |
| Atomicity/failure | Два CAS edit одной entry; ошибка после head update/перед index/event commit | Побеждает одна редакция, нет промежуточного пустого набора, rollback полный, cache epoch соответствует commit |
| Cache/expiry | Временные границы до/после activation/expiry; потерянный NOTIFY; clone/restart | Новое применимое правило и истёкшее правило меняют bundle вовремя; cache не переживает ошибочно tenant/dataset generation |

Ограничить длительность/размеры заранее, остановить эксперимент при превышении бюджета; не масштабировать нагрузку автоматически. Измерять p50/p95, SQL rows/bytes, planning/execution time и peak memory; не задавать обещание миллисекунд без baseline. Acceptance rule-query: отсутствует `App::read()/Database::read()` на пути; ни одна строка posts/branches/jobs/conversations не нужна для чтения набора после point scope resolution; SELECT canonical payload только для chosen versions; integrity validation ограничена выбранными immutable specs и принятым schema/coverage proof.

Обязательные hostile cases: чужой workspace ID; совпавший legacy alias при другой binding revision; пустые postKeys при непустом postAliases; active source_only fact, который не является rule; retired/pending guidance; forbidden-prefix не равен запрету любого слова; URL allowlist не является указанием вставить URL; unsupported constraint не становится generic активным текстом; oversize обязательного набора не обрезается; после изменения policy старое prepared не получает new-version approval автоматически; человеческий draft не меняется; UNKNOWN не retarget/retry.

## Просмотренные источники и неизвестное

Просмотрены текущие migration 0002 и targeted sections `knowledge.rs`, `company_knowledge_import.rs`, `storage_reads.rs`, `prepare_bundle.rs`, `engine_prepare.rs`, `operator_http.rs`, `assistant.mjs`; связанные existing storage/account boundaries изучены в `project/architecture-audit-storage-2026-09-23.md`. Прочитаны `project/rules-inventory-2026-09-23.md`, `project/rules-research-2026-09-23.md`, `project/rules-settings-backlog-2026-09-23.md`. Исследование literature из соседнего отчёта не перепроверялось и не используется как гарантия PostgreSQL/продукта.

Не проверены текущая восстановленная БД, EXPLAIN, реальные размеры и задержки, DB roles/RLS, текущий deployed binary или performance100. DDL/query кандидат не компилировался/не исполнялся. Integrity guard, typed compiler, semantic parity cases и transaction writer integration — обязательные части будущей реализации, не готовые функции. Основной результат этого среза — конкретный небольшой путь к адресному правилу без нового canonical store и без полного JSON workspace на каждый запрос.

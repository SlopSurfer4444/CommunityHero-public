# Public development snapshot validation - 7 October 2026

The latest agreed BAW source generation is bound by SOURCE-ADMISSION-20261007.json and SOURCE-MANIFEST.json. Source copies matched the agreed pins before and after export. The original private Git history and handoff/evidence files were not copied. This is a development source snapshot with acceptance continuing, not an admitted release or installed BAW runtime.

Focused synthetic checks:

```sh
node --test --test-concurrency=2 --test-reporter=spec mvp/tests/materials-import-contract.test.mjs mvp/tests/active-instructions.test.mjs mvp/tests/headless-cli.test.mjs mvp/tests/workspace-delta.test.mjs mvp/tests/evaluation/evaluate.test.mjs mvp/tests/evaluation/model-review.test.mjs mvp/adapters/assistant-rule-semantics.test.mjs mvp/adapters/assistant-evidence-quality.test.mjs mvp/adapters/assistant-fact-followup.test.mjs mvp/adapters/assistant-native-media-policy.test.mjs
```

Result: **136 tests, 135 passed, 0 failed, 1 skipped**. No remote provider/model, production database or external social action was used. The owning evaluation check was rechecked after its label was changed to identify the retained synthetic replacement.

Public transformations and exclusions:

- Existing knowledge-real-cases fixture remains ten deterministic synthetic cases. It is not an actual quality benchmark or customer corpus.
- Company knowledge-import allowlist is empty: original company source IDs/provenance are excluded. Import/trust behaviour therefore differs from live BAW configuration.
- Two corporate compile fixtures are newly synthetic: sales-route instruction and shared moderation request. These preserve source contracts, not actual company policies.
- Authenticated model catalog is replaced by a minimal synthetic route fixture; private quality-assessment JSON is excluded.
- The private-only ignored Rust test `actual_baw_candidate_replays_in_memory` and its private activation-plan include are removed from this public copy. It is excluded coverage, not PASS.
- Three adapter CLI-wire tests needing an exact separately installed Codex binary were not run. Generated runtimes and account caches are absent.
- Three additional tests (`bridge-transport-stage.test.mjs`, `provider-permalink.test.mjs`, `provider-session-concurrency.test.mjs`) import the excluded private connector and cannot run in this public snapshot. Their coverage is unavailable, not PASS.
- Private Angry.Space connector/transport implementation is absent. New adapter wrappers remain source references; `dev.ps1` selects the fail-closed bridge, disables background work and external writes.

Not verified here: Rust build/check/test suite, full Node suite, React build, server startup, PostgreSQL fixture acceptance, provider/media/model integrations, deployment and live social replies. A competing Rust build was intentionally not started while the main BAW acceptance executor was building. Historical September checks below do not certify this generation.

# Historical validation - 23 September 2026

The following report is retained as historical evidence for the previous public version, available at commit b3af575a1234972511955e7fda0caaf06492bbb2.

# Проверка публичного снимка

Проверки выполнены 23 сентября 2026 года в отдельной копии исходников.

- `cargo check --offline --locked --manifest-path mvp/server/Cargo.toml --all-targets`: PASS. Использован существующий cache зависимостей и отдельный каталог сборки вне снимка. Остались предупреждения о неиспользуемом коде и одном неиспользованном результате в тесте. Это проверка компиляции, не выполнение Rust-тестов и не сборка release.
- На исходном снимке проверены шесть Node test-файлов: **65 passed, 0 failed**. Синтетические проверки правил, CLI, отображения каталога, оценочных контрактов и обновлений workspace.
- `node project/rules-normalization-candidate-2026-09-23/verify.mjs`: PASS — 41 правило, 3 ограничения, 48 синтетических сценариев. Проверено структурное покрытие; семантический модельный прогон, запрос текущих версий и активация не выполнялись.
- `dev.ps1`: PowerShell AST parse PASS; сам сервер через launcher не запускался.

После исходного снимка перенесено исправление CLI-контракта импорта материалов. Команда `node --test mvp/tests/materials-import-contract.test.mjs mvp/tests/headless-cli.test.mjs`, выполненная в публичной копии, дала **25 passed, 0 failed**. Проверены асинхронный job, синхронный ответ о сохранении CommunityHero authority, наличие политики своего аккаунта, отклонение повреждённых ответов и отсутствие повторного импорта при resume. Повторная компиляция Rust не выполнялась: Rust-исходники не менялись. Эти 25 тестов частично пересекаются с исходными 65; числа нельзя складывать как независимое покрытие.

Снимок собран по явному списку исходных каталогов и отдельных документов. Проверены пути и шаблоны приватных ключей, известных токенов и буквальных credential assignments: совпадений в опубликованном кандидате нет. Синтетические URL/email в тестах и хеши, похожие на телефоны, не считаются клиентскими контактами. Такая проверка не является доказательством отсутствия любого возможного секрета.

Исходная история Git не переносилась. В публикуемых файлах отсутствуют частный коннектор, исходные клиентские fixtures и результаты их оценивания, содержимое БД, runtime, сессии, логи и build outputs. Десять оценочных случаев заменены полностью синтетическими. Сохранены метаданные происхождения правил и их хеши; они не содержат исходные комментарии.

Не проверены: запуск сервера из нового checkout, полный Rust test suite, сборка отдельного React-клиента, PostgreSQL-миграция на новой БД, внешние интеграции, модели и публикации. Никаких runtime/provider запросов или операций с рабочей БД при подготовке снимка не выполнялось.

# StillWatch-Next — окончательный план исправлений и завершения разработки

**Редакция 2 · 14 ноября 2026 года · аудит ревизии 79da236df1b17a5e21306b8750a7bbacb47fdad0**

**Маршрут чтения:** §§1–4 — выводы и архитектурные решения; §9/T08 — замена 24/72 часов; §11 — последовательность работ; §§12–14 — полный критерий завершения. Детальные спецификации: §5 — R01–R08, §6 — F01–F18, §7 — P01–P13, §8 — C01–C08, §9 — T01–T08. Всего **55 карточек**; каждая связывает проблему, изменение и проверку.

## 1. Решение и границы готовности

Обязательные 24- и 72-часовые прогоны заменить программой **Fast RC v2**: детерминированная проверка времени и состояний, заранее состаренная база, многократные реальные отказы и восстановление, затем 60 минут непрерывной нагрузки на настоящих моделях. Плановый бюджет всей квалификации — **180 минут на один нативный профиль** после подготовки артефактов, моделей и записей. По одному выбранному профилю Windows и Linux на одном dual-boot компьютере потребуют около шести часов плюс переключение ОС; на двух независимых стендах могут проверяться параллельно. Дополнительный CPU/GPU профиль требует собственной квалификации. Обучение моделей, получение данных и первичная сборка среды в эти три часа не входят.

Это явное изменение прежнего §19.3 технического плана. В README, IMPLEMENTATION_STATUS и acceptance matrix старое обязательное ожидание заменить ссылкой на Fast RC v2; прежние soak-результаты оставить историческими результатами своих сборок. Симулированный возраст, часы исходного видео и физическое время работы учитывать раздельно.

Рабочий результат разработки — поддержанные конфигурации, для которых выполнен весь заявленный контракт. **«100% завершено» в этом документе означает закрытые требования и воспроизводимые доказательства, а не математическую гарантию отсутствия любого будущего бага или ошибки распознавания.** Новые проблемы, обнаруженные при исполнении плана, получают собственную regression-проверку и проходят тот же процесс закрытия.

Существующий Rust Core, изолированные native workers, SQLite/outbox, отдельный Companion и ORT сохраняются. Основные изменения нужны в переходах между этими компонентами, модельных профилях, расписании и оценке качества.

### Что проверено в этом аудите

Повторно подтверждён тот же HEAD. Изучена история из 51 commit и расширен предыдущий срез исходников: domain/vision/capture, storage/supervision, API, четыре SDK, Flutter notification lifecycle, Firebase worker, CI, packaging и acceptance matrix. Проверены первичные документы Tokio, SQLite, ORT, Firebase, Android/Apple, источники по slicing/temporal fire verification и условия интеграций.

Здесь выполнен анализ исходников, проверка сохранённых отчётов и логических условий. Нового обучения, Windows/GPU execution и физической мобильной квалификации не было. «Путь по коду» далее означает воспроизводимый сценарий, который требуется подтвердить regression-тестом; это не утверждение, что он уже наблюдался у пользователя. Числа прежних испытаний относятся к указанным в репозитории сборкам.

Опорные документы: [статус реализации](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/IMPLEMENTATION_STATUS.md), [матрица приёмки](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/docs/acceptance.md), [исходный контракт §23.1](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/StillWatch_Next_Rust_Technical_Implementation_Plan_RU.md#L1498-L1520).

### Как пользоваться планом

- **R** — восстановление, хранение, задания и протоколы доставки.
- **F** — огонь, дым, падения, треки и смысл событий.
- **P** — производительность, CUDA, capture и admission.
- **C** — клиенты, мобильные уведомления, Firebase и полный выпуск.
- **T** — измерения и короткая квалификация.

У каждой карточки есть причина, изменение и проверка завершения. P0 означает исправление, необходимое прежде полной квалификации; P1 — обязательную работу соответствующего заявленного режима. Исследовательская гипотеза принимается только после сравнения на development-наборе. Названия новых файлов, схем и команд — предложения к реализации, а не уже существующие возможности репозитория.

## 2. Что уточнилось относительно первого разбора

| Проблема | Установленный механизм | Основные карточки |
|---|---|---|
| Ранний огонь/дым | В прямом production inference около onset в 5/8 клипов нет post-threshold наблюдений; downstream нечего подтверждать | F01/F02 |
| Маленький огонь в темноте | Глобальное Poor очищает историю и запрещает Review, независимо от качества локального bright ROI | F03 |
| Водяной туман | Текущая policy подтверждает устойчивые ложные box; оценочный Indoor Fire Smoke набор включает nuisance aerosols; исходная training ontology отдельно не установлена | F04 |
| Ложные медицинские Review | Ветки controlled activity не покрыты существующим golden trace; часть логики зависит от числа кадров при разрешённом низком cadence | F11–F15/F17 |
| Устаревший evidence после DB pressure | Время expiry вычисляется заново при позднем persist исходного кадра | R03 |
| Потерянная semantic job | Claim коммитится до чтения packet; прежний attempt навсегда исключает packet из выборки | R04 |
| Камера не восстанавливается | Завершённый task с прежней revision остаётся в registry | R01 |
| HTTP работает, анализ остановился | Нет полного supervision критических задач; liveness не равен coverage | R02 |
| Зелёная cadence при отставании | Cadence считается по source-time, возраст кадра начинается после чтения в Core | P01/P04 |
| 16 камер ниже цели | Существующий CPU-профиль не выполняет выбранную нагрузку; scheduler и media конкурируют за ресурсы | P02/P03/P06–P08/P12/P13 |
| CUDA failure | Не прошла Pose/gray; активный runtime использует PoseBalanced+Fire | P09–P11 |
| Уведомление исчезает при потере связи/Home | Foreground poll останавливается вместе с удалением уже показанных native alerts | C01 |
| Фоновые mobile alerts | Реализован foreground polling; полноценного фонового транспорта в просмотренных исходниках нет | C02 |
| Потеря refresh response | SDK намеренно fail-closed; сервер не предоставляет ограниченного восстановления той же rotation | R08 |
| Большое число alerts | Top-128 feed без продолжения может вытеснять старые unseen | R07 |
| Долгая работа storage | Очередь ограничена, но running closure и некоторые maintenance SQL/file scans не ограничены тем же deadline | R05–R06 |
| Ошибочное «испытание прошло» | Collector считает метрики, но не выносит обязательный PASS/FAIL/INCOMPLETE | T01–T02 |

Подробные источники находятся в карточках. Это перечень обнаруженных механизмов, а не заявление, что каждый из них вызвал конкретный опубликованный пропуск.

## 3. История исправлений: что сохранить и чем дополнить

| Уже выполнено | Исторический commit | Остаточная работа |
|---|---|---|
| Рост огня сравнивается внутри одного класса | [4a0ed7d](https://github.com/mikhailbovt/StillWatch-Next/commit/4a0ed7d2d511f9c6223d3749deb9623d1a1ec16c) | Качество самого candidate, nuisance verifier, появление flame после smoke |
| Остановка анализа повторяющихся live pixels | [d3652c5](https://github.com/mikhailbovt/StillWatch-Next/commit/d3652c5a13bc2a8274c02f86978fece6eb222fb9) | Различить подозрение на freeze и доказанную потерю свежести; не накапливать одинаковый кадр как новые доказательства |
| Истечение stale preview | [2c6f89b](https://github.com/mikhailbovt/StillWatch-Next/commit/2c6f89b23374e2bf431e9400fdf23b99004c1418) | Decode/source lag и age всего pipeline, а не только времени после Core receive |
| DB metadata и WAL budgets | [f1b21aa](https://github.com/mikhailbovt/StillWatch-Next/commit/f1b21aa83ead5366eb790399878c807d1dd2caac) | Same-epoch stale evidence, camera recovery и bounded maintenance под живой нагрузкой |
| Большие incident snapshots | [ef70d7f](https://github.com/mikhailbovt/StillWatch-Next/commit/ef70d7ffb00f22af360d2f392efa4df96d689d48) | Отдельная pagination/fairness для alert feed |
| SDK reconnect/cursor recovery | [38dc59e](https://github.com/mikhailbovt/StillWatch-Next/commit/38dc59e23887ad2eb8b19b8dd9924f59d7b20df6) | Потерянный refresh response и cross-client изменения contracts |
| Проверка разрешений при resume | [b61c756](https://github.com/mikhailbovt/StillWatch-Next/commit/b61c756aa967a6e7698f868bb480287693dd42c5) | Native notification lifecycle и действительный background transport |
| Уничтожение media children при смерти Core | [da6af97](https://github.com/mikhailbovt/StillWatch-Next/commit/da6af97ebcde50418d9e40caab438a7883486d8f) | Supervision живого Core с погибшей управляющей задачей |
| Общий ORT pool внутри worker | [28ef0b0](https://github.com/mikhailbovt/StillWatch-Next/commit/28ef0b09d6955522e964d3f5602f9de3225135f4) | Фаза расписания, per-model budget, CPU/media contention |
| Приоритет камер и overdue Fire | [0558441](https://github.com/mikhailbovt/StillWatch-Next/commit/0558441e4b2a690bd39c30ce5aa481cbd563f78d) | Both возвращает результат после двух моделей; burst не создаёт отсутствующий candidate |
| Маски и отдельный detail stream | [6af9a47](https://github.com/mikhailbovt/StillWatch-Next/commit/6af9a472b3fe3a012a3a4abc8320bb5ced8e5b48), [79da236](https://github.com/mikhailbovt/StillWatch-Next/commit/79da236df1b17a5e21306b8750a7bbacb47fdad0) | Измеренная cadence при detail/viewer нагрузке и реальная геометрия физических камер |
| Atomic model activation | [b962cae](https://github.com/mikhailbovt/StillWatch-Next/commit/b962cae6864d1052df6856bbd8b58e36cba77a7a) | Новый model-profile contract и отдельная numeric/event квалификация |

Перенос baseline в Rust и качество baseline — разные проверки. Существующие parity results сохранять как доказательство переноса; intentional changes новых policies/моделей записывать отдельно. Успешная parity не превращает исходный false positive в правильное поведение.

## 4. Общие контракты, которые связывают исправления

### 4.1. Время и происхождение данных

Каждый результат должен сохранять camera id, source epoch, scene revision, FrameKey, model/profile/policy version и source timestamp. Время наблюдения, начало/конец inference, durable commit и receipt — разные поля. Retry не обновляет возраст evidence. Новая epoch не принимает старый worker result.

Absolute sensor capture age может быть неизвестен без синхронизированного источника. Тогда показывать измеренный pipeline age и относительное source-progress lag; не выдавать их за доказанную задержку от камеры. Для сквозного стенда использовать известный paced source/временную метку в изображении.

### 4.2. Владение состоянием

На актуальную revision камеры приходится один capture owner. Domain histories и durable incident identity остаются упорядоченными по камере. Stateless inference допускает отдельное распределение работы, но результат несёт scope и отвергается при устаревании. Перенос камеры между lanes не должен терять историю или позволять двум владельцам применять результаты.

### 4.3. События и доставка

Candidate, Review, Urgent, delivery receipt, operator ACK и resolve — отдельные состояния. Недоступность источника, модели, сети или UI не означает исчезновения опасности. Длительный incident переживает короткий detector track. Доставка работает с устойчивой идентичностью attention cycle и не снимает событие из-за неполного feed.

### 4.4. Профиль модели и вычислений

Единица квалификации — точные binary/native/model hashes плюс hardware/OS/driver/power mode, inputs/shapes, model roles, sampling, camera count, viewers и semantic route. Загрузка двух активных моделей не требует замалчивать failure третьей: профиль обязан явно запретить неподдержанную роль.

Номинальная частота и минимально допустимое покрытие остаются раздельными. Минимум выбирается вместе с quality evaluation; уменьшение частоты ради зелёного benchmark не является оптимизацией, пока не доказано сохранение событий.

## 5. Восстановление, хранилище и протоколы

### R01 — вышедшая camera task остаётся зарегистрированной навсегда

**Приоритет:** P0 для автономного мониторинга. **Подтверждение:** прямой путь в коде; нативно здесь не воспроизводился.

`run_camera` возвращается при ошибке `bump_camera_epoch`, а `reconcile` пропускает любой уже присутствующий task с той же ревизией. Завершившийся JoinHandle не извлекается. Достаточно временной невозможности записи DB в момент reconnect: после восстановления диска камера сама не вернётся. [registry.rs:656–680](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L656-L680), [761–788](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L761-L788)

**Изменить:** возвращать `Completed/Stopped/RetryableFailure/ConfigurationFailure`, получать завершения через JoinSet или явное reaping. Повторно запускать только актуальную enabled/admitted revision, с backoff+jitter и сбросом после стабильной работы. Перед новым child обязательно завершить владение старым процессом. Non-repeat replay с естественным Completed не перезапускать; неверные operator-managed настройки не превращать в цикл перезагрузок.

**Минимум тестов:** enabled RTSP → принудительный reconnect → отказ epoch write → восстановить DB → новая epoch и оба счётчика inference растут без PATCH/Core restart. Параллельно другая камера продолжает работать. Отдельно Completed replay и устаревшая revision.

**Готово:** все retryable исходы восстановлены автоматически; ни одного дублирующего child; один открытый health interval на outage, закрывается после реального capture+analysis progress. Recovery deadline задаётся как текущий максимальный backoff плюс startup budget: сейчас у capture backoff до 30 s, поэтому универсальное «всегда за 5 s» неправдоподобно. [registry.rs:810–820](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L810-L820)

### R02 — верхний supervisor и watchdog не следят за всей полезной работой

**Приоритет:** P0. Core создаёт задачи, но во время serving select ждёт только server/shutdown; vision замечает panic lane и пишет degraded, однако lane не возрождает, sampler в активный supervision не включён. [main.rs:588–687](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/main.rs#L588-L687), [vision.rs:227–254](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L227-L254)

Watchdog уже имеет timeout вокруг `store.healthy()`, однако healthy проверяет читаемость SQLite. Работающий HTTP+DB может сопровождаться умершим видеоконвейером. [main.rs:795–810](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/main.rs#L795-L810), [storage/lib.rs:239–240](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/lib.rs#L239-L240)

**Изменить:** supervisor владеет критическими задачами и их outcome; классифицированный subsystem restart либо controlled nonzero Core exit при невосстановимой внутренней поломке. In-memory progress counters для capture, model results, sampler и persistence; ожидание прогресса устанавливается только если есть соответствующая работа. Пустая очередь, все disabled cameras, offline источник, Completed replay и выключенная semantic route — законные состояния. Разделить process liveness, control readiness, per-camera coverage, durable output и delivery route.

**Не ломать существующий контракт:** readiness намеренно остаётся 200 при corrupt model/frozen camera — оператору нужен доступ для диагностики. Coverage должен иметь отдельный сигнал. Его внешний read path не должен зависеть от записи того же SQLite health event, который во время disk full может не сохраниться. [analysis_core.py:339,458](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tests/e2e/analysis_core.py#L339), [health.rs:149–157](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/health.rs#L149-L157)

**Тест:** управляемо завершить lane, sampler, registry, maintenance; зависнуть в fake operation; отключить все камеры и убедиться, что watchdog не создаёт restart storm. После настоящего сбоя восстановить canary вплоть до evidence/outbox/test receiver. Фиксировать actual loss-of-coverage interval, а не просто new PID.

### R03 — старый отложенный frame получает новую жизнь как свежий evidence

**Приоритет:** P0 для правильности live escalation. **Новая находка:** подтверждён путь в коде, нативный сценарий ещё не исполнен.

`PendingHazards` удерживает исходный `Arc<CameraFrame>` и повторяет запись без предельного возраста; `persist()` вычисляет `expires_at = utc_ms() +15s` непосредственно перед записью. `EvidenceSampler::select` кодирует удержанный старый frame без проверки `captured_at.elapsed()`. Semantic eligibility проверяет эту новую expiry, scope и version, но не реальный возраст кадра. [hazards.rs:369–418](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L369-L418), [425–515](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L425-L515), [evidence.rs:145–201](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/evidence.rs#L145-L201), [storage/semantic.rs:196–223](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/semantic.rs#L196-L223)

**Условие:** DB writer недоступен минуту, capture остаётся в прежней epoch и продолжает получать кадры, pending удерживает старый кандидат. После восстановления старый frame получает новую 15-секундную eligibility и может отправиться в semantic evaluation. Новая epoch предотвратила бы это; для находки важна именно неизменная epoch.

**Изменить:** неизменяемые времена `source_us`, `observed_host_utc`, монотонный возраст/дедлайн в текущем процессе; время durable commit отдельное. Eligibility к live escalation не обновляется при retry. Исторический факт и Review сохранить с признаком задержки/недостаточной свежести; для новой live Urgent/clearance требовать свежие наблюдения актуальной сцены. Не переписывать историческое `live=true` в ложное утверждение о replay: нужен отдельный признак пригодности для текущего уведомления. После перезапуска короткоживущая работа проходит повторную проверку времени/версии, а не получает новую TTL автоматически.

**Тест:** fake clock, удержать writer дольше evidence freshness budget, сохранить same epoch, разблокировать. Старый факт остаётся в history, не считается свежим и не вызывает разрешённую только свежим evidence эскалацию; новый кадр создаёт свежую оценку. Повторить при clock step и при новой epoch.

### R04 — semantic claim навсегда расходует packet при аварии

**Приоритет:** P1; влияет на optional semantic route и её способность эскалировать локальный Review.

Claim запрещает любой предыдущий attempt для packet, ограничивает attempts на incident тремя за весь срок, коммитит attempt и только после этого читает packet. Нет lease/reclaim состояния. Crash, отмена supervisor или ошибка чтения после commit оставляет unfinished attempt; этот packet больше не выбирается. Запись завершённого report также может не пройти из-за DB pressure, но повторного сохранения/claim нет. [storage/semantic.rs:70–108](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/semantic.rs#L70-L108), [migrations/002.sql:10–22](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/migrations/002.sql#L10-L22), [engine/semantic.rs:219–279](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/semantic.rs#L219-L279)

**Изменить:** отделить job (`queued/claimed/dispatched/completed/expired/unknown_outcome`) от attempts, записывать lease owner/token/deadline и immutable provider request id. Данные для отправки проверить до окончательного dispatch; до сети повторная lease безопасна. После неизвестного исхода сетевого запроса нужна provider idempotency/reconciliation либо явно ограниченная политика повторов с учётом возможного повторного billing. «Exactly once через сеть» не обещать. Persist response после успешного provider ответа должен иметь идемпотентный путь повторного сохранения. Lease takeover должен fencing token отклонять устаревший owner. Проверять consent, epoch, scene, evidence version и expiry снова перед dispatch и apply.

Лимиты 3attempts/incident и daily quota — существующая защита затрат, не ошибка сама по себе; определить, какие стадии её расходуют и какой статус видит пользователь после исчерпания. Нельзя просто снять ограничения. Ошибка claim сейчас попадает в `let Ok(Some(...))` и не меняет явно route health; различать `no work` и `storage failure`.

**Тесты:** crash после claim до packet load; после load до send; send succeeded/response lost; response получен/commit failed; cancelled supervisor; lease expired; consent revoked; midnight/day rollback; exhausted budget. Invariant: не применить stale report, не потерять известный результат, не тратить неограниченный бюджет и не показывать «route available» как доказательство обработанных jobs.

### R05 — ограничение очереди не ограничивает длительность одной storage operation

**Приоритет:** P1, риск latencies и общей coverage; длительный stall на реальном диске здесь не измерен.

`Store::call` проверяет 10 s deadline только перед началом closure; `result.await` ждёт запущенную операцию без собственного bounded completion. Caller cancellation не снимает queued job: он ещё может выполнить mutation. Blob verification, directory recount, запись и fsync происходят в этом единственном writer. [lib.rs:252–289](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/lib.rs#L252-L289), [evidence.rs:215–243](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/evidence.rs#L215-L243), [640–647](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/evidence.rs#L640-L647)

`PendingHazards::blocked` имеет полезный конечный лимит 2batch/192items. Его нельзя просто удалить ради FPS; нужна явная accounting потерянного покрытия. `pending.flush().await` выполняется до следующего inference dispatch. [hazards.rs:347–355](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L347-L355), [vision.rs:285–297](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L285-L297)

**Изменить минимально:** telemetry queue wait/operation time/oldest work/queue capacity; ограниченные партии maintenance; не делать дорогой повторный directory scan в hot path. Если profiling подтверждает длительный fsync, staging/hashing JPEG в bounded I/O worker, затем короткая metadata transaction с явным протоколом orphan cleanup. Не превращать каждый blob в независимый неограниченный task. Timeout caller означает «исход пока неизвестен», не «запись точно отменена»; повтор выполняется с прежним idempotency key. Защитить API diagnostic/coverage read от ожидания writer там, где можно безопасно использовать in-memory snapshot. Зависший ОС I/O нельзя гарантированно отменить обычным abort Tokio task; нужен escalation policy supervisor/host.

**Тест:** заполнить queue при задержанном writer, отменить await, освободить writer и повторить тот же command. Допустим один committed result, ни одного двойного outbox. Во время задержки coverage явно degraded, API не сообщает «записи точно нет». После снятия fault pipeline возвращается.

### R06 — retention нужно проверять на возрасте и объёме, не ждать возраста

**Приоритет:** P1 по границам ресурсов. Часть retention уже имеет LIMIT128/512, однако expiry outbox, silences и административных записей удаляется одним неограниченным SQL statement. Evidence maintenance сбрасывает cached blob_bytes после каждого прохода; следующий write заново обходит каталог. [alerts.rs:104–116](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/alerts.rs#L104-L116), [retention.rs:15–79](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/retention.rs#L15-L79), [evidence.rs:382–489](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/evidence.rs#L382-L489)

**Изменить:** выделить `maintenance_step(now, budget)` из 60 s timer loop; все потенциально большие удаления делать bounded batches с нужными индексами и продолжением в следующем шаге, контролировать debt/backlog и максимальную длительность транзакции. Для blob accounting — корректировать известный размер на успешные create/delete, отдельная ограниченная сверка после неизвестного результата I/O; не полагаться на бесконечный «раз в минуту пересчитаем весь диск». Startup integrity/reconciliation обязаны оставаться корректными; оптимизация проверки содержимого не должна молча принимать повреждённые JPEG. [main.rs:692–719](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/main.rs#L692-L719), [evidence.rs:492–590](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/evidence.rs#L492-L590)

**Тест:** подготовить DB у выбранной рабочей квоты и с timestamps до/после retention boundary; shared blob с одной живой и одной истёкшей ссылкой; pending cleanup после crash; open incident; ACK/idempotency record ещё внутри 7 days; накопленные expired receipts/sessions. Вызвать несколько step с фиксированным now, параллельно live camera+canary. Проверить referential integrity, quota accounting, SSE floor/generation и отсутствие удаления живых ссылок. Нужен dataset нужного объёма и количества файлов, а не только 10 строк с датами «месяц назад».

**Готово:** maintenance debt убывает при workload профиля, каждая партия помещается в его latency budget, активные кадры продолжают анализироваться; использование диска ограничено бюджетами. Чистка не требует ожидания 30 дней.

### R07 — alert feed обрезается до 128 без продолжения

**Приоритет:** P1 для перегрузки delivery; изменение согласовать с клиентским и API-контрактом.

`active_alerts` возвращает top128 и `AlertFeed { alerts }` без cursor/overflow. Сортировка urgent → unseen → newest может вытеснять старые unseen при непрерывном поступлении новых событий. Это условный путь starvation при >128 outstanding, не утверждение о потере обычного одиночного alert. [alerts.rs:12–68](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/alerts.rs#L12-L68)

**Изменить:** bounded pagination с устойчивым cursor либо durable per-device delivery queue с fairness и метрикой overflow/backlog; immutable identity `(core generation/instance, incident_id, attention_cycle)` для идемпотентной презентации. Приоритет urgent сохранить. Пропажу alert с первой страницы клиент не должен трактовать как разрешение инцидента.

**Тест:**129+active, concurrent arrivals, повторные receipts, lost receipt response, expiry/silence/revocation при paging; каждый eligible cycle достижим до своего TTL либо его невыполнимость явно отмечена. Не продлевать TTL бесконечно, чтобы замаскировать starvation.

### R08 — потерянный ответ refresh превращает легитимный повтор в отзыв устройства

**Приоритет:** P1 для автономной связи клиента; изменение согласовать с auth-протоколом и SDK.

`refresh_device` генерирует новые токены, переносит старый в retired_refresh_credentials и коммитит. Если ответ пропал, повтор того же подписанного запроса с прежним nonce видит retired credential и отзывает устройство. Nonce проверяется и подписывается, но нет хранения результата идентичного retry. [trust.rs:149–193](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/trust.rs#L149-L193)

Текущие Python/TypeScript/Rust SDK сознательно переходят в terminal/fail-closed до попытки refresh. Потерянный ответ лишает клиента пути автономного восстановления. Серверный retry receipt должен сопровождаться сохранённым pending refresh request во всех четырёх SDK, включая Dart; разные nonce при reuse по-прежнему отличать от одного и того же допустимого повтора.

**Изменить:** безопасный bounded idempotent refresh retry с возвратом того же результата в кратком окне. Receipt привязан к digest полного подписанного запроса, device и rotation, имеет ограниченный TTL и защищённое хранение возвращаемых secrets; перед повторной выдачей заново проверяются подпись/авторизация, revoke, expiry и текущая rotation. После revoke или следующей rotation receipt недействителен. Разные запросы повторно использованным credential по-прежнему вызывают предусмотренную реакцию. Для переживающего restart receipt возвращаемый secret envelope хранить зашифрованным ключом защищённого хранилища Core, не открытым токеном в audit/log/обычном result cache. Клиент сохраняет pending подписанный request до отправки, сериализует refresh и атомарно сохраняет новый credential set с завершением pending. Альтернатива протокола должна явно решать lost-response, а не просто увеличивать TTL.

**Тест:** commit→drop HTTP response→same request retry; два одинаковых parallel request; другой nonce со старым token; restart между commit/retry; access/refresh expiry boundary. Проверять доступ к alert feed после всех легитимных восстановлений.

## 6. Огонь, дым, падения и жизненный цикл событий

1. Раннее обнаружение ограничено уже выходом detector + postprocess. В повторном native pass без Core scheduler и incident policy пять из восьми MmodalFire-клипов не дали ни одного прошедшего порог наблюдения на выбранных кадрах; остальные три дали только семь слабых наблюдений. Это не доказывает отсутствие подпорогового сигнала. Источник: models/evaluations/mmodalfire-v1.md:18–27.
2. Стандартная FirePolicy не имеет обязательного ожидания 8 секунд или крупного минимального размера очага: минимум площади 1 пиксель; сильная детекция может дать Review сразу, слабая — после двух наблюдений, растянутых минимум на 300 мс. Источник: crates/sw-domain/src/fire.rs:169–177, 320–342. Поэтому просто сокращать таймеры как главное лечение нельзя.
3. Poor visibility является абсолютным запретом нового fire Review и стирает историю; тот же global quality удаляет все pose observations и исключает кадры из evidence ring. Это реальная архитектурная проблема локально яркого объекта в тёмном окружении, но без исходного KMU trace нельзя назвать её установленной причиной KMU miss.
4. При разрешённом pose_fps=3 стабильный поток физически не может удовлетворить текущую проверку ControlledPattern: нужно 5 поз в окне 1.2 секунды, а 3 fps дают максимум 4. Это точное свойство кода, отдельно от статистики false reviews.
5. Существующий PoseTracker уже содержит раздельное сопоставление сильных/слабых наблюдений и Hungarian assignment. Предложение «добавить ByteTrack» без разбора этого кода не является решением.
6. Fixture тестов содержит 88 сценариев и 1376 шагов. MotionEvidence: observing 891, following_concern 163, visibility_lost 33, recovered_activity 6; ordinary_activity отсутствует. Transition: none 909, fall_like 115, unknown_origin 69; slow_descent и controlled_pattern отсутствуют. MedicalRules тест сравнивает 1239 medical steps именно с этим fixture. Следовательно parity coverage не покрывает существенные causal branches.
7. Две локальные hazard factory всегда создают Concern → Review. Единственное production-присваивание ImmediateVisibleConcern находится в semantic fire projection; медицинская семантика также возвращает максимум Concern. Ноль Urgent в negative replay не является доказательством полезной автономной Urgent specificity.

| ID | Проблема | Тип доказательства | Приоритет |
| --- | --- | --- | --- |
| F01 | Не видно, где исчезает ранний сигнал | Измеренный bottleneck + диагностический пробел | P0 |
| F02 | Потеря малых объектов при 640 и substream | Код ограничения; причинность по клипам ещё измерить | P1 |
| F03 | Global Poor блокирует видимый локальный очаг / человека | Доказанная логика; синтетический repro | P0 |
| F04 | Mist и combustion smoke не разделены надёжно | Подтверждённые false reviews и смешанная разметка | P0 |
| F05 | Редкие слабые detections не поддерживают temporal evidence; burst поздний | Доказанная логика; эффект измерить | P1 |
| F06 | Track UUID используется как incident UUID | Доказанная архитектура; duplicate scenarios проверяемы | P0 |
| F07 | Два updates навсегда; пропуск нового flame / visibility | Доказанные условия фильтра | P0 |
| F08 | Controlled fire и рост bbox не равны физической опасности | Измеренные reviews; ограниченная семантика | P1 |
| F09 | Exact repeated pixels трактуются как недоступный источник | Доказанная политика; normal-static false hold ещё не измерен | P1 |
| F10 | Новые модели не помещаются в старый pack / decoder contract | Доказанное ограничение | P0 перед модельными заменами |
| F11 | Fall sensitivity получена вместе с большим ADL review burden | Измерено в двух наборах | P0 |
| F12 | 3 fps отключает ControlledPattern | Детерминированная несовместимость | P0 |
| F13 | Fragmentation / ambiguity / слабые люди | Защиты есть; конкретный полевой вклад не измерен | P1 |
| F14 | Полный motion reset после coverage gap теряет episode linkage | Доказанная логика; duplicate path проверить | P1 |
| F15 | OrdinaryActivity и recovery не проходят весь incident lifecycle | Доказанные cross-layer разрывы | P0 |
| F16 | Локальный автономный Urgent отсутствует | Доказанный отсутствующий production path | P0: решение по capability |
| F17 | Parity, per-frame и event quality смешаны; causal branches без fixture | Доказанные границы оценки | P0 |
| F18 | Evidence — редкие overview и один crop, не temporal ROI sequence | Доказанный формат и sampling | P1 |

Практический порядок: F01/F03/F07/F12/F15/F17 дают проверяемые изменения без новой модели; F10 создаёт совместимый путь для F02/F04/F11; F06/F14 решают incident lifecycle; после этого F16 можно квалифицировать на реальной цепочке. Обучение запускать параллельно после фиксации ontology/splits, но не использовать его как оправдание откладывания детерминированных дефектов.

### F01 — Разложить early misses по стадиям и открыть подпороговую диагностику

**Основные исходники.** [Ранние observations](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/mmodalfire-v1.md#L16-L27) · [Postprocess](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/postprocess.rs#L30-L85) · [Fire settings](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L122-L126).

**Зависимости / граница с другими карточками.** P01 (trace), T02 (evaluator); отдельный diagnostic protocol, без изменения legacy output.

**Свидетельства.** MmodalFire Windows: 0/8 в окне, поздние события через 31.16–68.92 секунды; недоступность/warmup максимум 0.34% выборок. Native dense pass: 84 samples на клип, примерно 8.33 fps, пять клипов без postprocessed detections, семь слабых detections в оставшихся трёх. models/evaluations/mmodalfire-v1.md:7–27. crates/sw-inference/src/postprocess.rs:30–49 выбирает max(smoke, fire) и выбрасывает score ≤ 0.20 до policy; crates/sw-inference/src/pack.rs:107–111 фиксирует порог. FirePolicy::new также не принимает confidence < 0.20: crates/sw-domain/src/fire.rs:122–126. tools/benchmark/fire_observations.py:65–79 сохраняет только уже отфильтрованные observations.

**Изменение.** Добавить отдельный диагностический режим worker с bounded top-K anchors по каждому классу и причины удаления: low score, invalid geometry, NMS, crop/transform, policy quality, continuity, controlled context. Записывать source PTS, input geometry, model/profile hashes, prefilter/postfilter counts, score и bbox при привязке к ручному ROI. Для новой generation разделить proposal_threshold, confirm_threshold и alert policy; legacy decoder и его строгую parity оставить неизменными.

**Минимальная проверка.** Один синтетический tensor на score ниже, равном и выше 0.20 плюс overlapping boxes показывает все причины и не меняет legacy output. На тех же сохранённых кадрах составить per-episode trace: first visual onset → first useful raw proposal → first accepted observation → first Review → committed incident → intent. Один и тот же набор кадров использовать для A/B, иначе latest-frame scheduler меняет состав входа.

**Gate.** Для каждого miss есть воспроизводимый stage attribution, включая unknown если нет исходного media; runtime diagnostics bounded по RAM/объёму. Нельзя объявлять, что «ещё один FSM» исправит кадры, где пригодного proposal нет.

**Развилка.** Если подпороговые true ROIs отделяются от negatives — калибровать proposal stage и verifier. Если scale помогает — F02. Если полезного сигнала нет и на качественном ROI — обучать/менять detector по F04/F10. Если источнику не хватает пикселей, менять анализируемый поток или явно ограничивать поддержанный минимальный объект; увеличение уже размытого кадра информацию не возвращает.

### F02 — Качественный источник и малые очаги: отдельный ресурсный профиль

**Основные исходники.** [Shapes/640 limit](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/pack.rs#L13-L65) · [Preprocessing](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/preprocess.rs#L41-L63) · [Small-object evaluation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/indoor-fire-smoke-images-v1.md#L5-L12).

**Зависимости / граница с другими карточками.** F01/F10; P03/P07/P11 для budget, shape inventory и capacity.

**Свидетельства.** crates/sw-inference/src/pack.rs:14, 57–65 ограничивает обе стороны тензора до 640; crates/sw-inference/src/preprocess.rs:41–63 уменьшает/увеличивает изображение с long side 640. Rectangular graph уже имеет dynamic H/W: crates/sw-inference/src/pack.rs:24–38, поэтому новый ONNX export не автоматически обязателен, но ограничение runtime и квалификация профиля должны измениться. models/evaluations/indoor-fire-smoke-images-v1.md:5–12: flame 190/425 boxes, small 43/117; mixed smoke 33/294, small 0/38. Это transfer/localisation evidence, не чистая оценка combustion smoke и не onset benchmark. Текущий main/detail stream для просмотра не становится автоматически входом inference.

**Изменение.** Сравнить на одинаковых исходных кадрах: текущий 640; увеличенный supported shape; 640 на ограниченных tiles/ROIs исходного разрешения; отдельный более подходящий detector. Сохранить source→crop→letterbox affine transform и class-aware dedup в координатах источника. В scheduler добавить отдельный bounded refinement task с лимитом ROIs/камеру/время и общей квотой, чтобы refine не отнимал базовые fire/pose slots. Предусмотреть редкий независимый scan высокодетального изображения: refinement, запускаемый только уже успешным detector, не поможет нулевым detections.

**Минимальная проверка.** Геометрия для вертикального/горизонтального кадра, padding, ROI у границы, overlapping tiles и два разных очага; одна real small-positive и matched nuisance sequence для сравнений. Короткий native load на целевом железе с включённым refine измеряет базовую cadence и возраст результата.

**Gate.** Увеличивается timely episode recall/first-proposal coverage при заранее ограниченном nuisance/cost budget; подтверждённая геометрия; не деградирует базовый coverage. Нельзя принимать улучшение только по одной картинке.

**Развилка.** Если high-resolution profile не помещается по ресурсам — меньше камер для этого профиля, ROI quota или обучение более эффективного detector. Если исходный substream уничтожил признаки — выбрать лучший analysis stream/геометрию. Не включать 960/1280 на всех 16 камерах без измерения.

### F03 — Global visibility перестаёт быть абсолютным запретом локального события

**Основные исходники.** [Scene quality](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/scene.rs#L17-L64) · [Fire decision](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L279-L342) · [Evidence exclusion](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/evidence.rs#L85-L89).

**Зависимости / граница с другими карточками.** F18 для quality-aware evidence; согласовать privacy mask geometry и IPC metadata.

**Свидетельства.** crates/sw-engine/src/scene.rs:17–64 использует 640 sparsely sampled pixels: Poor при ≥95% dark, ≥95% bright, малом contrast либо mean<12. crates/sw-domain/src/fire.rs:158–162 очищает историю на Poor; :301–306 не добавляет её; :332 запрещает Review. crates/sw-engine/src/hazards.rs:90–94 заменяет все person detections пустым списком; crates/sw-engine/src/evidence.rs:85–89 не сохраняет кадры. bins/stillwatch-worker/src/capture.rs:51,138–147 сначала закрашивает privacy masks чёрным; scene quality считает эти нули обычной частью кадра.

Выполненная ранее Python-транскрипция: 640×360, чёрный фон,40×40 ярко-оранжевый ROI (0.694% площади), валидная Fire observation score0.99. Scene Poor и решение Observing, тот же bbox при Good даёт Review. Это точный логический counterexample, не запуск весов и не доказательство конкретного KMU miss.

**Изменение.** Разделить transport/source validity, scene overview quality, visible-unmasked fraction и candidate ROI quality. Privacy masks исключить из знаменателя качества, не раскрывая их пиксели. Сильный локально видимый flame/person должен сохранять candidate/evidence даже при тёмном фоне; решение помечается limited по реально отсутствующей информации. Глобально тёмный кадр не превращать в положительный сигнал сам по себе. Реально пустой, закрытый, испорченный или невалидный кадр не должен подтверждать temporal evidence.

**Минимальная проверка.** Native test матрица: small bright flame in dark; локально освещённый человек; весь чёрный/белый кадр; masked95% с корректным оставшимся ROI; ROI внутри маски; краткая смена exposure. Проверка проходит policy→evidence: одного исправления crates/sw-engine/src/scene.rs недостаточно.

**Gate.** Валидный ROI не отсекается только global mean, маски не дают ложную недоступность неприкрытого ROI, пустые/испорченные кадры не порождают concern и не auto-clear. KMU и новые night cases измеряются заново, без обещания, что этот фикс решит отсутствующий detector signal.

### F04 — Mist / smoke: новая ontology и проверка короткой последовательности

**Основные исходники.** [Mist evaluation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/mmodalfire-v1.md#L5-L14) · [Evaluation label ontology](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/indoor-fire-smoke-images-v1.md#L5-L12) · [Fire policy](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L308-L342).

**Зависимости / граница с другими карточками.** F01/F10/F18; F07 для появления нового evidence после nuisance verdict.

**Свидетельства.** Water mist дал 7 Windows reviews/1755 s и 5 в повторе Ubuntu с другой policy/host; один общий lab setup. KMU lookalikes:13reviews/10clips,5 affected clips; urgent0. crates/sw-domain/src/fire.rs:308–342 считает persist/growth/strong smoke достаточным для Review и не имеет семантики пара. В оценочном Indoor Fire Smoke наборе класс smoke включает stage fog/humidifier vapor: models/evaluations/indoor-fire-smoke-images-v1.md:5. Это не утверждение о разметке исходных training data действующей модели. При будущем использовании этого набора для обучения нельзя без переразметки трактовать все такие labels как опасный дым.

**Изменение.** Разметить как минимум flame, combustion smoke, nuisance vapor/mist/fog, reflection/display/light, uncertain; допустима отдельная nuisance head или отдельный verifier, не обязательны все классы в одном detector. Dataset manifest хранит origin episode, room/camera, source dataset, crop ancestry, onset interval, true negatives/unknown. Training split по источникам/сценам, а не соседним кадрам. Обучить bounded temporal verifier на ROI appearance + short track geometry + timestamps/quality; выход hazard / nuisance / unknown с отдельно калиброванной уверенностью. Nuisance verdict не должен быть вечным veto области: новый flame/spread/scene change отменяет его, неизвестное не выдаётся за безопасность.

**Минимальная проверка.** Пары похожих smoke/mist, translucent smoke on bright/dark backgrounds, aerosol near real flame, mist followed by real flame; verifier должен видеть честные missingness/time gaps. Сравнение detector-only, rules-only, verifier-only и combined на одном наборе.

**Gate.** Снижение event-level nuisance Review burden при сохранении заданного timely-positive sensitivity на замороженном независимом наборе; порог выбирается на development/calibration, финальный holdout не используется для выбора. Реальный дым нельзя подавлять только из-за white colour или near-humidifier location.

**Развилка.** Если temporal model не превосходит простой baseline — не усложнять production; использовать лучшие подтверждённые признаки/профиль и сохранять Unknown/Review для неразличимых случаев. Если RGB не содержит различающей информации — честно ограничить capability этой сцены; дополнительные сигналы камеры/датчика могут стать отдельным поддержанным вариантом. Гарантировать идеальное различение визуально одинаковых аэрозолей одной архитектурой нельзя.

### F05 — Continuity и early burst должны обслуживать слабое подозрение до подтверждённого Review

**Основные исходники.** [Candidate trigger](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L123-L147) · [Burst scheduling](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L551-L652) · [Growth continuity](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L366-L399).

**Зависимости / граница с другими карточками.** F01; P02/P03 для расписания; F06 для bounded candidate identity.

**Свидетельства.** FirePolicy history continuity1.5 s, история 5 s, maxquiet8 s: crates/sw-domain/src/fire.rs:8–12,151–162,279–306. Corroboration≥2samples spanning300 ms; growth≥4samples spanning1.2 s, same-class: :320–324,366–399. crates/sw-engine/src/hazards.rs:137–144 выставляет fire_candidate только для Observing, notPoor и not-contained-flame-only. crates/sw-engine/src/vision.rs:331–339 запускает burst только для live после выполненного fire; :27–30 задаёт 3 fps/2 s/cooldown10 s; :592–652 имеет более низкий dispatch rank. Для отсутствующих proposals этот путь вообще не запускается; replay не проверяет live burst.

**Изменение.** Явный candidate event от proposal stage с временем/качеством/ROI ещё до полноценного Review. Небольшой burst для weak proposal с общим budget и guaranteed base coverage; отдельный редкий independent scan для zero-proposal случая. Temporal accumulator хранит positive/negative/unobserved и elapsed time, ограничивает допустимую паузу, не выдаёт predicted bbox за реально наблюдённый. Weak observations могут поддерживать уже существующий кандидат при строгой spatial/appearance association; не создавать Urgent сами по себе.

**Минимальная проверка.** Одна sequence с isolated weak positives, subthreshold matches, пропуском кадров и настоящими negative observations; сравнить 2 fps/1 fps/burst/jitter. Live-style virtual scheduler harness подтверждает arm→priority→executed frames, а не только existence of boolean.

**Gate.** Защита от непроизводительного burst storm, bounded CPU, непрерывные базовые fire slots для других камер, улучшение time-to-useful-observation на early dev cases без evidence padding. Старый 1.5 s continuity нельзя просто растянуть до бесконечности ради прохождения одного ролика.

### F06 — Разделить spatial region track, hazard episode и durable incident

**Основные исходники.** [Region identity](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L137-L252) · [Publication identity](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L150-L183) · [Incident ID](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L233-L269).

**Зависимости / граница с другими карточками.** F07/F14; storage incident scope contract. Не заменяет R07 (доставка всех уже созданных incidents).

**Свидетельства.** crates/sw-domain/src/fire.rs:180–252 жадно кластеризует соседние boxes и сопоставляет каждый cluster одному старому region; unmatched получает новый UUID. Region удаляется после 8 s без observation (:156–157). Этот UUID становится Incident ID: crates/sw-engine/src/hazards.rs:246–248; published удаляется вместе с region (:150–157). В то же время unresolved incident в storage не исчезает из-за исчезновения detector box. Значит split/merge/missed detection/reconnect способны создавать новые cards одного физического события; это не решается одним outbox dedup.

**Изменение.** Ввести track_id для краткоживущей геометрии и incident_id/episode_id для устойчивого наблюдаемого происшествия. FireEpisodeManager хранит связи tracks↔episode, visible_start, current evidence_segment_start, last_observed, uncertainty и explicit split/merge links. Стабильная ассоциация учитывает same camera/scope, spatial course и time, а не глобальный cooldown. После scene revision/epoch нельзя механически объединять temporal evidence; можно хранить явную uncertain linkage к ранее незакрытому incident. Два независимых очага должны оставаться различимыми. Решение о новом episode после operator resolution задаётся явно: новый onset/существенное новое развитие, а не бесконечное переоткрытие неизменного box.

**Минимальная проверка.** Box→2boxes→box; flame↔smoke box geometry;8.1 s detector dropout с явным gap; два пространственно отдельных очага; reconnect с тем же и другим видом; resolve then genuinely new ignition. Проверять не только UUID, но число initial Review intents, evidence lineage и запрет чужого auto-clear.

**Gate.** Один подтверждённый непрерывный incident сохраняет identity; ambiguity не сливает разные опасности; новая опасность не подавляется дедупом. Counterexamples можно прогнать за миллисекунды source-time traces.

### F07 — Обновлять evidence по состоянию эпизода, а не максимум два раза

**Основные исходники.** [Update cap](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L158-L183) · [Material change](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L281-L311).

**Зависимости / граница с другими карточками.** R03 (freshness), R04 (semantic retry), P03 (budget); это другой дефект, чем stale TTL.

**Свидетельства.** crates/sw-engine/src/hazards.rs:158–183 после initial publication допускает максимум 2updates и минимум 500 ms. material_change (:281–310) видит только false→true fire growth/smoke growth/escape/smoke_visible; не видит flame_visible false→true, visibility improvement, progressive severity, region change и отсутствие подходящего текущего пакета. Medical update видит только onset_observed и переход identity_uncertain в true. Следовательно smoke-only Review может не получить evidence с появившимся flame; packet, снятый на плохом/неудачном кадре, может остаться единственным. Это также ограничивает subsequent semantic Urgent projection.

**Изменение.** Заменить cap на per-incident state/version coalescing и token bucket. Два потока: дешёвый local observation ledger и ограниченные тяжёлые image packets. Принудительное обновление на новой flame, escape, meaningful severity, восстановлении видимости/coverage, новой фазе motion и устаревшем/неудавшемся packet; периодическое bounded refresh для unresolved active incident. Давать приоритет последнему материалу, не копить старые пакеты. Budget ограничивает rate/bytes/in-flight work, а не число существенных изменений за всю жизнь эпизода.

**Минимальная проверка.** Smoke→smoke growth→flame growth→escape как минимум 4 material changes; smoke-only→flame без growth; Limited→Good; semantic failure→fresh packet. Проверить ограниченный поток при flickering flags и отдельно ни одной потери severity increase.

**Gate.** Актуальный episode имеет свежий reviewable packet; material evidence не теряется после третьего изменения; RAM/disk/request bounds соблюдены. Согласовать с storage freshness/deadline из R03/R04: новый TTL не должен делать старый captured frame свежим.

### F08 — Controlled fire: ожидаемое пламя как контекст, не бессрочное разрешение

**Основные исходники.** [Controlled-flame evaluation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/controlled-flame-v1.md) · [Controlled/growth policy](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/fire.rs#L325-L399) · [Scene context](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/scene.rs#L75-L154).

**Зависимости / граница с другими карточками.** F04/F06; scene revision contract. Сохраняет уже исправленный same-class growth.

**Свидетельства.** models/evaluations/controlled-flame-v1.md показывает 27→15review при зонах, а повторные A/B после same-class growth fix не дают устойчивой победы на всех fireplace clips. crates/sw-domain/src/fire.rs:325–337 suppresses только contained+Good+noSmoke+noGrowth; growth — median bbox area increase>1.55 и≥0.0005frame (:366–399). Геометрический bbox growth может происходить от detector jitter/flicker/merge. crates/sw-engine/src/scene.rs:93–154 использует polygon overlap; source/mode не участвуют в логике. Scene fingerprint8×5 инвалидирует контекст при большой пространственной разнице≥16cells на 750 ms (crates/sw-engine/src/scene.rs:75–90;crates/sw-engine/src/registry.rs:872–888); этого недостаточно для универсального camera-motion proof.

**Изменение.** Сохранять имеющийся same-class fix. Вводить class-stable robust track features, motion-compensated ROI growth и проверку новых признаков за пределами expected area. Source/mode либо подключить к документированным prior/expected envelope, либо ясно считать только annotations; не обещать разные режимы при одинаковой обработке. Scene context имеет revision, качество alignment и suspend condition. Любой новый smoke/escape/несогласованный вид возвращает проверку; ожидаемый дым от готовки требует temporal/semantic nuisance evidence, не blanket mask.

**Минимальная проверка.** Bbox jitter у камина без physical spread; реальный выход пламени за zone; smoke-only; камера сдвинулась на небольшую величину; человек перекрыл зону; exposure shift; один класс кратко пропал. Видео с независимой spread/hazard truth, а не только flame box labels.

**Gate.** Сократить лишние Review на действительно benign controlled episodes без подавления escape/spread. FURG flame annotations сами по себе не дают truth «опасное/безопасное барбекю».

### F09 — Freeze detector: отсутствие новой визуальной информации не равно доказанному отказу камеры

**Основные исходники.** [Repeat detector](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L989-L1016) · [Analysis hold](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L866-L937) · [RTSP repeat test](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tests/e2e/analysis_core.py#L409-L493).

**Зависимости / граница с другими карточками.** R01/R02/P04 для source health; T03 для независимых часов. Не подменяет transport stall detection.

**Свидетельства.** crates/sw-engine/src/registry.rs:35–37 задаёт 650 ms sampling и 6 s hold. RepeatedFrames сначала сравнивает scene fingerprint, затем full decoded RGB hash (:989–1016). На отличающихся decoded pixels static-looking сцена остаётся доступна — это уже предусмотрено, поэтому нельзя обвинять код в срабатывании на любое отсутствие движения. Но exact-repeat encoder output, полностью/почти закрытая mask и true freeze одинаковы по этому наблюдению. После hold usable_for_analysis=false, CaptureState::Unavailable (:866–871,900–930), resumed bumps source epoch (:888–892). Тесты проверяют специально losslessly repeated RTSP pixels: tests/e2e/analysis_core.py:409–493,crates/sw-engine/src/registry.rs:1104–1131.

**Изменение.** Разделить transport stalled/native clock invalid и visual novelty unconfirmed. На exact repeats оставить health warning и bounded probing; повторённые кадры НЕ увеличивают temporal evidence/visible duration и не подтверждают recovery. Сильное содержимое уникального кадра остаётся наблюдаемым и incident не исчезает. Если есть camera-specific heartbeat/encoder metadata — использовать как дополнительное подтверждение, не обещая universal freeze classifier. Политика epoch reset должна отражать реальную потерю временной непрерывности, а не любую смену hash после warning.

**Минимальная проверка.** Advancing PTS + exact static valid source; source frozen with old image; independent camera heartbeat; changing-noise static scene; masked scene; появление маленького flame после повторов; resumed image. Проверить recoveries, not just UI unavailable.

**Gate.** Нельзя гарантировать различение двух битово одинаковых потоков только RGB. Система честно сообщает неопределённость, не создаёт искусственную persistence, продолжает проверять новые данные и не лишает активный incident evidence без нужды.

### F10 — Версионированный model contract и корректный export audit

**Основные исходники.** [Pack validation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/pack.rs#L175-L238) · [Model reconstruction/export](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/model-export/export.py#L94-L115) · [PyTorch/ONNX comparison](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/model-export/export.py#L167-L198).

**Зависимости / граница с другими карточками.** P09/P10/P11 для численной/shape qualification; training provenance.

**Свидетельства.** crates/sw-inference/src/pack.rs:84–115 знает только Pose/PoseBalanced/Fire,56/6channels; :175–238 проверяет жёсткие baseline revision, FP32/opset, classes, thresholds, shapes, runtime. Текущие Square и dynamic Rectangular — два профиля, batch1. Просто положить новый ONNX рядом недостаточно для multiclass nuisance detector/temporal input/batch profile. Model identity_activations=[model.9.cv1.act] и smoke0/fire1 не являются найденной ошибкой: original baseline mikhailbovt/stillwatch@b0d20a20d38a5bbc22178d150999e57cc9e7bfb3:perception.py восстанавливает Identity, проверяет module-type digest и strict state; docs/models.md того же baseline описывает Identity в original checkpoint. Dataset labels flame0/smoke1 — отдельная mapping, не ошибка model map.

**Изменение.** Новый schema/profile generation: explicit model role, source provenance/hash, input tensors, allowed shapes/batch, decoder family, class map, normalization/letterbox, candidate thresholds, numeric qualification, policy compatibility. Поддержать legacy decoder отдельно. Новый training artifact требует собственного qualification набора и provenance, не изменения старых hashes или baseline ожидаемых результатов. Для контроля исходной модели сделать тройное сравнение original checkpoint→safe reconstruction→ONNX на одних ранних/negative frames, если веса доступны: tools/model-export/export.py:94–115 и 167–198 в настоящее время сравнивает ONNX с reconstructed fused PyTorch.

**Минимальная проверка.** Старый pack загружается неизменно; несовместимые classes/shapes/decoder отклоняются; новый versioned pack проходит geometry/observation equivalence; искусственная class-map/activation ошибка ловится fixture. Нужна проверка границ score/NMS, а не только средняя tensor MAE.

**Gate.** Rollback на проверенный generation, reproducible training/export metadata, event regression на heldout; file hash correctness отдельно от semantic quality. Не «чинить» корректный Identity случайной заменой SiLU.

### F11 — Fall / ADL: отделить наблюдаемое движение от медицинского смысла

**Основные исходники.** [GMDCSA results](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/gmdcsa24-v2.1.md#L7-L15) · [Pilot ADL results](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/heldout-pilot.md#L5-L15) · [Medical candidates](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/medical.rs#L220-L319).

**Зависимости / граница с другими карточками.** F12/F13/F15/F18; сначала исправленный rules baseline, затем сравнение classifier.

**Свидетельства.** models/evaluations/gmdcsa24-v2.1.md:7–13:12/12staged falls,1additionalmatch,10reviews/16ADL на 134.84 s. models/evaluations/heldout-pilot.md:7–13:9/9staged falls,5reviews/10ADL на 111.8 s, ошибки при kneeling/sitting/hopping/picking. crates/sw-domain/src/medical.rs:220–298 быстрые геометрические условия; :302–319 persistent low после 2 s; crates/sw-domain/src/pose.rs:128–181 выводит low/posture из 2D geometry. Это намеренно чувствительные признаки, не медицинский диагноз; уверенность COCO pose не равна вероятности падения.

**Изменение.** Два явных уровня: motion proposal (rapid descent / low posture / lost after suspicion) и episode interpretation (fall-like / controlled descent / ordinary activity / uncertain). Сохранить ранний candidate, добавить короткую causal последовательность pose + masked ROI + missingness; lightweight classifier сначала сравнить с исправленным rule baseline. Разметить ADL phases и transition onset, включая bed/floor lying, bending, kneeling, sitting, assisted movement, occlusion, two people. Scene furniture используется как context, не veto: падение может произойти и у кровати. Уже лежащего при входе камеры человека отмечать как unknown-origin posture concern при достаточных основаниях, не выдумывать observed fall.

**Минимальная проверка.** Paired sequences с похожими конечными позами, но разным переходом: fall vs deliberate recline; pick-object vs collapse; person already lying; occluded descent; fall plus helper; rapid fall without stillness. На прежних 21fall clips сохранить регрессию, но финальный claim проверить на новых scenes/subjects.

**Gate.** Меньше ADL initial reviews/duplicate incidents и лучше interpretation при фиксированном допустимом ухудшении timely sensitivity; никакого uplift только по frame accuracy. Отличать время первого локального concern и время более уверенного подтверждения. Для уже видимого быстрого падения не добавлять обязательную многосекундную паузу.

**Развилка.** Если classifier не превосходит time-correct rules — не добавлять его в production. Если ambiguity остаётся — Review с фактическим reason, а не автоматическое подтверждение диагноза. Negative Urgent0 при недостижимом Urgent path не засчитывать как успех.

### F12 — Исправить математическую несовместимость ControlledPattern с 3 fps

**Основные исходники.** [Allowed sampling](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-protocol/src/cameras.rs#L235-L262) · [Controlled pattern](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/motion.rs#L584-L630).

**Зависимости / граница с другими карточками.** P01/P02 и T03; без новых weights и без долгого native soak.

**Свидетельства.** CameraSampling::valid допускает pose_fps3–5: crates/sw-protocol/src/cameras.rs:235–262. MotionClassifier::controlled требует sequence≥8, подготовленную seated sequence≥5, reference-first≥0.8 s и рассматривает только prepared samples за последние 1.2 s: crates/sw-domain/src/motion.rs:584–630. При 3 fps четыре временных интервала для пяти samples занимают 1.333 s, следовательно prepared.len≥5 никогда не выполняется на равномерном nominal cadence. Снижение FPS было добавлено позже статических motion правил; IMPLEMENTATION_STATUS.md:351–353 прямо оставляет качество low-rate profiles открытым.

**Изменение.** Перевести evidence requirement на elapsed duration + coverage + минимум независимых наблюдений, согласованный с поддержанным cadence. Не подменять реальные samples интерполяцией; timestamps входят в модель/правила. Для profile3 fps можно выбрать другое число samples только после сохранения минимального времени и требований continuity, а не просто убрать все guards. Пересмотреть аналогичные fixed sample/fixed gap условия (reobserved0.4 s, lost_track0.6 s, drop_window0.8 s) как явную таблицу применимости профиля.

**Минимальная проверка.** Одна и та же gold causal trace resampled3/4/5 fps, плюс phase offsets/jitter/skips. Controlled recline должен оставаться controlled при достаточной видимости; rapid fall, lost landing и ambiguous identity не должны ошибочно уходить в OrdinaryActivity. Проверять motion output и durable incident.

**Gate.** Каждый разрешённый sampling profile имеет semantic acceptance, а не только измеренный frame counter. Если 3 fps не позволяет надёжно решить задачу — capability профиля явно ограничивается либо настройка запрещается для этого режима; нельзя показывать прежнее качество по умолчанию.

### F13 — Сохранить правильные защиты identity, улучшить fragmentation без ложного recovery

**Основные исходники.** [Tracker retention/confirmation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/tracking.rs#L72-L177) · [Association stages](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/tracking.rs#L189-L244) · [Association cost/guards](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/tracking.rs#L309-L506).

**Зависимости / граница с другими карточками.** F14, F01-образная association диагностика; существующие fixture guards сохраняются.

**Свидетельства.** crates/sw-domain/src/tracking.rs:72–80 confirmed retention3 s, tentative0.6 s; :105–138 confirmation после 3hits или score≥0.5; :189–244 два confidence stages и confirmed priority с Hungarian. Cost имеет descent exception и scale/appearance/ambiguity guards (:309–367,432–506). crates/sw-inference/src/postprocess.rs:47 отбрасывает pose≤0.15; :126–130 limited height<40source px/truncated3px. crates/sw-domain/src/motion.rs:333–355 при gap/ambiguity очищает основы recovery и sticky identity_uncertain для concern. Эти защиты уже покрывают много legacy counterexamples.

**Изменение.** Сначала записывать association diagnostics: unmatched reason, chosen/runner-up cost, score/keypoint reliability, ambiguity, disappearance length. Сравнить failures по pose loss vs association vs interpretation. Подпороговые существующие track observations при необходимости вести отдельным weak-support channel; они не создают нового человека/падение без подтверждения. Incident linkage не должно требовать выдуманной уверенности о личности: новый track может иметь uncertain relation к старому incident, но не право его auto-clear. Не объединять людей только по IoU или цвету одежды.

**Минимальная проверка.** Реальные/fixture sequences: два пересекающихся человека, lying person+runner, partial occlusion, clothing exposure shift, low-score consistent person, weak false transient, person reappears after3 s. Измерять association continuity и event duplicates вместе с ID switches; повторить 3/5 fps.

**Gate.** Меньше split episodes без роста identity mix/чужих recoveries; сохранены имеющиеся adversarial fixtures. Если улучшение tracking не доказано, оставить conservative ambiguity и работать через episode linkage/F14 вместо снятия guards.

### F14 — Coverage gap сбрасывает motion evidence, но не должен терять историю открытого incident

**Основные исходники.** [Coverage reset](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L81-L117) · [Full medical reset](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/medical.rs#L85-L90) · [Episode creation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/motion.rs#L86-L107).

**Зависимости / граница с другими карточками.** F06/F15; R01/R02 lifecycle restart. Не переносить motion evidence через scope change.

**Свидетельства.** crates/sw-engine/src/hazards.rs:81–89 при pose gap>1.5 s полностью вызывает medical.reset(); тот сбрасывает tracker, motion и subjects (crates/sw-domain/src/medical.rs:85–90). UUID motion episodes генерируется заново (crates/sw-domain/src/motion.rs:86–107). published затем удаляет старые эпизоды (:150–157). При продолжающейся low posture после паузы новый track/episode может создать ещё один Review, а старый durable incident остаётся unresolved. Не доказано, что именно этим вызваны опубликованные duplicates; это проверяемый маршрут.

**Изменение.** Развести reset kinematics и durable concern memory. После coverage gap запрещён перенос скорости/падения и auto-recovery, но episode manager хранит unresolved incident, last known geometry/time/identity uncertainty и связи с новым observation segment. Перезапуск Core/source/scene обрабатывается явными правилами scope. Новое независимое падение после наблюдённого восстановления создаёт новый episode; отсутствие наблюдений не считается recovery.

**Минимальная проверка.** Fall→1.6 s gap→same low person; fall→gap→different upright person; gap→new independent fall; source epoch replacement. Проверить duplicate Review intents, observed onset attribution и запрет восстановления по чужому человеку.

**Gate.** Сохраняется честная неопределённость без бессмысленного потока duplicate incidents; нельзя требовать seamless motion continuity через реальный gap. Время в observed_interval должно разделять incident age и current evidence segment.

### F15 — Довести recovery и controlled activity до согласованного lifecycle

**Основные исходники.** [Recovery/publication logic](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L105-L196) · [Pending result handling](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L369-L419) · [Recovery eligibility](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/incident.rs#L324-L352).

**Зависимости / граница с другими карточками.** F07; R03/R05 для актуальности/commit outcomes; отдельный ACK recovery, не общий storage retry.

**Свидетельства A.** crates/sw-domain/src/motion.rs:674–699 OrdinaryActivity может иметь clear_eligible=true. crates/sw-engine/src/hazards.rs:193–196 лишь перестаёт публиковать такие новые candidates; automatic recovery queue формируется только для RecoveredActivity+onset_observed+certainty+current last_seen (:105–117). Ранний Review поэтому не закрывается просто при позднем подтверждённом ControlledPattern/OrdinaryActivity. Это может быть осознанным conservative choice, но тогда clear_eligible и UI semantics вводят в заблуждение; целевой контракт должен быть явным.

**Свидетельства B.** recovery_queued=true ставится до storage ack (:113–117). Incident::record_recovery требует evidence_state=Persisted (crates/sw-domain/src/incident.rs:324–337). При Partial/Unavailable возвращает Stale; flush выбрасывает Stale из pending (:402–407). Флаг не сбрасывается, так что последующее успешное evidence не повторяет recovery для этого episode. Это воспроизводимый cross-layer dead path, а не утверждение о зарегистрированном полевом случае.

**Изменение.** Ввести typed recovery intent с basis, source timestamp, motion/evidence revision и outcome acknowledgement. Pending/retryable/inapplicable/completed различать; новые доказательства могут повторить ранее неприменимый переход. OrdinaryActivity получает отдельную semantics: verified benign course способен пересмотреть неacknowledged nonUrgent Review при достаточном наблюдённом onset, certainty и видимости либо оставляет incident для оператора, но без ложного clear_eligible. Сохранить запреты auto-resolve Urgent/acknowledged incidents и восстановления по исчезновению человека.

**Минимальная проверка.** Initial Review→ControlledPattern→OrdinaryActivity; Partial evidence→recovery rejected→Persisted fresh evidence→recovery succeeds; stale scope; acknowledged/urgent; unknown onset; identity uncertain; same person recovers then falls again. Это одна компактная integration suite с виртуальными timestamps.

**Gate.** Recovery либо применяется один раз по валидному текущему basis, либо явно остаётся unavailable/retryable; технический race не делает его навсегда невозможным. Никакого auto-clear по одному кадру, низкой score или отсутствии detection.

### F16 — Явно закончить локальный автономный уровень срочности

**Основные исходники.** [Local assessments](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L193-L269) · [Semantic projection](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/semantic.rs#L528-L585) · [Attention reducer](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/incident.rs#L439-L473).

**Зависимости / граница с другими карточками.** F03/F04/F07/F11/F15; R07 и клиентская alarm state machine для реальной доставки.

**Свидетельства.** Hazard::medical и Hazard::fire присваивают Concern: crates/sw-engine/src/hazards.rs:210–213,246–248. Semantic project присваивает ImmediateVisibleConcern только likely uncontrolled fire при local spread/escape, good semantic visibility, flameYes и spreadYes: crates/sw-engine/src/semantic.rs:541–563. Medical classifications возвращают Concern: :575–583. Incident reducer переводит ImmediateVisibleConcern в Urgent (:464–465), но сам его не выводит. Поэтому без semantic route нынешний local hazard pipeline даёт Review, а medical не получает Urgent и через этот route.

**Изменение.** Сначала явно определить продуктовый контракт capability: какие наблюдаемые ситуации требуют автономной сирены, а какие — Review. Если нужен offline Urgent, добавить отдельный versioned UrgencyPolicy поверх проверенных локальных temporal facts/ROI verifier, с evidence basis, confidence/coverage, reason, resource/failure behaviour. Fire и medical критерии разные. Не использовать detector score≥0.65 как самостоятельный emergency threshold. Неопределённость, недоступность semantic route или переименование Concern не являются основанием Urgent: без квалифицированного независимого локального основания остаются Review/Unknown и соответствующий health status. Semantic route остаётся дополнительной оценкой с freshness check.

**Минимальная проверка.** Полная offline live chain: реальные representative positive frames→local decision→Incident Urgent→durable intent→delivery; negative confusers; insufficient visibility; semantic absent/late/contradictory; already acked Review→genuine escalation. Synthetic notification tests подтверждают доставку, но не ML criterion.

**Gate.** Separate timely Urgent sensitivity/false Urgent metrics для каждого supported scenario и зафиксированного профиля. Если качество не доказано — capability честно остаётся Review-only; нельзя засчитывать zero urgent на negatives как завершённую квалификацию.

### F17 — Быстрая содержательная проверка вместо многосуточного ожидания

**Основные исходники.** [Medical fixture test](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-domain/src/medical.rs#L486-L531) · [KMU observation limits](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/kmu-fire-smoke-v1.md) · [Event scoring](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/evaluate.py#L134-L172).

**Зависимости / граница с другими карточками.** T01–T05; уникальное содержание здесь — ML branches, ontology и causal episode truth.

**Свидетельства.** crates/sw-domain/src/medical.rs:486–531 и crates/sw-domain/src/tracking.rs:551–584 проверяют parity по одному tests/fixtures/observations/legacy-rules-v2.jsonl.gz. Фактический разбор fixture дал ноль slow_descent/controlled_pattern/ordinary_activity states. Существующие fire tests в crates/sw-domain/src/fire.rs полезны для sameclass growth/gap/controlled context, но crates/sw-engine/src/hazards.rs не содержит module tests, значит его publication/recovery/incident interactions нельзя считать покрытыми этими policy tests. GMDCSA scoring считает первое same-category событие в temporal window, без person identity/spatial grounding: models/evaluations/gmdcsa24-v2.1.md:13.

**Изменение.** Три независимых уровня:

- Pure causal traces с virtual source/monotonic time: качество веток F03/F05–F09/F12–F15, skipped/repeated frames, restart/epoch, bounded queues. Секунды и часы source-time проходят без wall-clock sleep.
- Deterministic observation replay: один frozen observation tensor/box/keypoint stream для old/new policy; одинаковый selected-frame ledger, sampling profile, hashes. Раздельно detector regression и policy regression.
- Короткие native end-to-end trials на CPU/GPU и целевом live/replay path: реальные кадры, decode/PTS, inference, intent. Новый независимый scene/subject holdout с timeliness, Review burden, duplicate count, abstentions и source-hours exposure.

В frozen annotation manifest добавить onset interval (last negative/first positive), left/right censoring, object/person match, class ontology, episode grouping, scene/subject lineage. Разделить timely hit, miss при полностью наблюдённом допустимом окне, right-censored follow-up и invalid/lost coverage. Invalid coverage проваливает end-to-end qualification; её нельзя молча убрать из знаменателя и получить лучший результат модели. Для KMU daytime onset left-censored: отдельно измеряется время реакции на уже видимый при старте сигнал. Lowlight после 34 s имеет лишь 5.33 s доEOF: отсутствие события — right-censored для полного 8 s критерия, а не доказанный full-window miss и не success. Исторический отчёт 0/2 сохраняется с объяснением методики; восемь полных MmodalFire окон остаются 0/8. Уже изученные Mmodal/KMU использовать как regression/dev knowledge, не называть их untouched final holdout после настроек по этим failures.

**Gate.** Набор покрывает все фактические production branches и внешние contracts, исполняется быстро; качество измеряется на новой независимой информации. Многосуточные source-time циклы можно ускорить, но нельзя математически превратить повтор одного ролика в новые независимые negative camera-hours или реальные unscripted falls. Нельзя обещать надёжность на бесконечном поле из любого конечного теста.

### F18 — Evidence для интерпретации должно сохранять ход события

**Основные исходники.** [Evidence selection](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/evidence.rs#L145-L227) · [Coverage limitation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L478-L481) · [Evidence sampling settings](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-protocol/src/cameras.rs#L235-L262).

**Зависимости / граница с другими карточками.** F03/F07; R03 freshness и P06 resource budget. Не дублирует durability/retention R06.

**Свидетельства.** crates/sw-engine/src/evidence.rs:16–21 bounded ring8 s/512 KiB per camera/32 MiB total; sampler200 ms. select (:145–227) выдаёт до 3 исторических overview,1 текущий overview и 1 текущий crop. Это не существующая последовательность ROI crops для temporal verifier. JPEG encode уменьшает overview до≤720 по грубому sampling (:253–270); при маленьком объекте overview может скрыть детали. crates/sw-engine/src/hazards.rs:478–481 объявляет SparseCoverage при maxgap>600 ms независимо от разрешённого evidence_fps1–5 (crates/sw-protocol/src/cameras.rs:240–262). При идеальном 1 fps ожидаемый gap≈1000 ms всё равно получает этот флаг. Context limitation не равен EvidenceState::Partial: crates/sw-storage/src/evidence.rs:268–273 задаёт state по сохранению файлов, не качеству temporal coverage.

**Изменение.** Bounded pre-event ROI feature/crop ring с исходным timestamp и transform, формируемый из privacy-masked input. Для локального temporal verifier хранить точно определённое число actual observations/временное окно/missingness; не заставлять 2D image-packet API притворяться video model input. Для semantic/operator packet выбирать кадры before/during/after плюс ROI course по задаче; оставить current overview для grounding. Coverage хранит отдельно configured target, achieved sampling, novelty, attachment coverage и абсолютную пригодность для конкретного task.1 fps может быть честно too sparse для motion, но не должен обозначаться как ошибка исполнения заявленного 1 fps.

**Минимальная проверка.** Tiny ROI survives crop sequence; pre-event retained under byte cap; missing sample не дублируется; same image not counted as independent movement;1/2/5 fps achieved-vs-required distinction; current overview remains available; privacy mask invariant.

**Gate.** Verifier получает тот temporal signal, на котором обучался, а оператор/semantic route видит достаточное актуальное основание. Memory/cost bounds сохраняются; packet refresh согласован с F07 и incident lifecycle.

## 7. Производительность, CUDA и допустимая нагрузка

В коде уже есть latest-frame/watch вместо неограниченной очереди, отдельные capture/inference процессы с deadline, глобальный ORT thread pool **в пределах каждого worker**, повторное использование входного tensor и worker RGB, CPU thread budget, ограниченный приоритет 150 мс, защита overdue Fire, отдельный detail stream и собственный RGB-бюджет. TF32 уже выключен. Эти пункты не следует выдавать за новые оптимизации. История подтверждает намерение: `28ef0b09d6955522e964d3f5602f9de3225135f4` разделяет один ORT pool между model sessions worker; `0558441e4b2a690bd39c30ce5aa481cbd563f78d` добавляет priority/late Fire/burst; `8dc370bf6f45349c84f7f9a5c73040c10800e238` добавляет codec и capture capacity. [Runtime](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/lib.rs#L120), [scheduler](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L592).

Цель остаётся **16 камер с заданными 5 Pose + 2 Fire fps на каждой**, на явно названном и проверенном hardware/runtime profile, плюс предусмотренная нагрузка от evidence и просмотра. CPU-профиль на четыре камеры — самостоятельный полезный результат, но не закрытие исходной цели 16. Переход к 3/1, снижение разрешения или более лёгкой pose-модели меняет профиль качества и требует отдельной оценки; это не бесплатный успех оптимизации.

При 16×(5+2) требуется 112 model executions/s: 80 PoseBalanced и 32 Fire. `Both` экономит часть preprocessing/IPC, но всё равно запускает две модели последовательно. Число RPC лежит между 80 и 112/s в зависимости от совпадений; номинальные сетки 200/500 мс совпадают раз в секунду и дали бы 96 RPC/s без иных округлений. Дополнительные Fire требуют резерва сверх базовой нагрузки. Appearance histogram для каждого подходящего человека, NMS, RGB transport, evidence, preview/detail сюда не включены. [Worker](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/inference.rs#L89).

Сохранённый Linux CUDA-отчёт даёт около 88.229 model executions/s суммарно, то есть приблизительно на 21.2% меньше требуемых 112; для достижения цели ему понадобилось бы примерно 26.9% прироста относительно измеренного уровня. Это **старый отчёт другого состояния**, не оценка быстродействия текущего HEAD. Сохранённый CPU — около 59.296/s. Последний Windows A/B на HEAD-предшественнике при 16 камерах имеет худшую камеру 1.588/1.588 и 288/288 ниже цели; сам статус сообщает о фоновых длительных запусках и не объявляет опыт изолированным доказательством скорости. [CPU report](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/bench-linux-cpu.json), [CUDA report](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/bench-linux-cuda.json), [Windows A/B](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/IMPLEMENTATION_STATUS.md#L375).

Ниже Pxx — отдельные reviewable изменения. «Обязательное» означает нужно для честного завершения, «условное» — делать лишь после измеренного bottleneck.

### P01 — Сделать измеряемой всю обработку и отличить source cadence от wall cadence

**Статус: подтверждённый пробел измерения; обязательное.** Worker `elapsed_ms` начинается после чтения полного RGB, заканчивается до JSON serialization/write. Core `captured_at` ставится после получения кадра из capture pipe. Cadence использует последние десять **секунд источника**, а freshness — локальный возраст после pipe. Поэтому медленное поступление advancing PTS способно выглядеть достаточно частым на source clock, хотя реальные wall fps ниже. Это доказуемое свойство формул, а не установленная причина нынешних провалов. [Worker:87–129](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/inference.rs#L87), [Core receive:840–917](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L840), [cadence:561–588](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L561).

**Изменение.** Ввести `ExecutionTiming {input_read_us, preprocess_us, pose_run_us, pose_decode_us, fire_run_us, fire_decode_us, serialize_us}` и измерение round-trip на стороне Core. Использовать микросекунды; текущий integer-ms недостаточен для коротких стадий. Для split decode/NMS/appearance — вложенные диагностические spans, включаемые на калибровке, если базовое измерение покажет заметную цену postprocess. Строго не суммировать clocks разных процессов как timestamps: передавать durations, а end-to-end RTT считать в Core одним `Instant`.

Дополнить `Analysis` двумя bounded очередями accepted `Instant` для Pose/Fire, чтобы публиковать `wall_cadence` отдельно от существующей `source_cadence`. Дополнить монотонными per-task counters: dispatched, accepted, timeout, rejected_stale, rejected_scope, rejected_generation, missed_source_slots, intentional_burst. Счётчики уровня процесса не обнулять на source epoch, а помечать эпоху; иначе частые reset могут скрывать дыры. Гистограммы по всем выполненным запросам (или детерминированной документированной выборке), не p95 из многократно опрошенного `last_worker_ms`. Высокую cardinality UUID не переносить в публичные бесконечные metrics labels: camera diagnostics — ограниченный реестр активных камер, aggregates — bounded labels.

`InferenceResult` сейчас `deny_unknown_fields`, поэтому просто добавленное поле ломает смешанную пару Core/worker. Минимально: capability/version handshake для новой пары и синхронное обновление worker protocol; старый SWI1 остаётся старым контрактом. Бинарники и protocol version входят в профиль. Не начинать с отдельного telemetry-сервиса. [Wire:179–187](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-protocol/src/worker.rs#L179).

**Acceptance.** Unit на два процесса simulated clock, на source 15 fps, поступающий с половинной скоростью: source cadence может быть нормальной, wall cadence обязан показать половину. Fake worker с delays до read/run/response демонстрирует, что сумма и Core RTT объясняют расходы без двойного счёта. Epoch reset не исчезает из denominator отчёта. Existing `analysis_core.py:126–179` продолжает ловить низкий source fps и recovery. Цена telemetry измеряется A/B и входит в выпуск; чтение/запись логов не на каждую RGB копию.

**Зависимости/эффект.** Первая работа; сама по себе не увеличивает throughput. Разрешает выбирать следующие оптимизации по реальной цене, а не по предположению.

### P02 — Убрать накопление округления из базового расписания

**Статус: подтверждённая семантика; обязательное небольшое изменение.** `due()` считает interval от PTS последнего принятого результата. На строго 15 fps требуемые 2 fps каждый раз округляются до восьми кадров, то есть 533.33 мс и 1.875 fps даже при бесконечно быстром worker. При задержках фаза дополнительно сдвигается. [due:650–652](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L650), [accept:463–478](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L463).

**Изменение.** В `Analysis` добавить `TaskClock` отдельно для Pose/Fire: origin/source phase, последний claimed базовый slot, последний accepted slot, последний обработанный `FrameKey`, optional in-flight claim. `FrameKey = (SourceScope, capture_generation, header.frame)`; dedupe именно `(FrameKey, task)`: Pose и Fire разрешено обработать на одном кадре независимо. Не хранить очередь устаревших заданий. Кандидат всегда latest usable frame; пропущенные slots считаются, а не исполняются догоняющей очередью.

```text
slot(pts, origin, fps) = floor((pts - origin) * fps / 1_000_000)
                            // checked i128, только native advancing PTS
if slot > last_claimed_slot and frame_key != last_dispatched_frame_for_task:
    claim(current slot, frame_key, request_id, worker_generation)
    count skipped slots, dispatch newest frame
on valid result: count accepted; clear matching claim
on failure/stale result: count reason; clear matching claim; keep phase
on new scope/generation: reset phase and claims under the explicit boundary rule
```

Решение, считать ли первый кадр slot 0 и как совместить cold start с burst, должно быть записано в одном helper, а не размазано по `next/accept/status`. Базовый clock не должен отсчитываться заново от фактически запоздавшего результата. Проверять соответствие claim при accept; orphan late response не закрывает новый slot.

**Acceptance.** Pure scheduler tests без реального времени: 15 fps→2 fps даёт чередование семи/восьми source кадров и 120 базовых fire slots за 60 source seconds (с явно оговорёнными границами окна); 29.97 fps, 3 fps, jitter, dropped inputs, source gap, same PTS, новая эпоха, worker timeout. Ни один кадр не исполняется дважды одной задачей, но может исполняться Pose и Fire. Burst не сдвигает базовую фазу; base misses видны. Старые тесты bounded priority/overdue Fire/cooldown (`vision.rs:673–725`) сохранить.

**Зависимости/эффект.** После P01. Убирает потери от quantization drift; не превращает 1.6 fps в 5 fps и не добавляет вычислительной мощности. Политики, чувствительные к реальному шагу source PTS, прогоняются на сохранённых observations и на коротких реальных replay.

### P03 — Отдельно учитывать дедлайн Fire при выборе Both и ограничить extra budget

**Статус: подтверждённая задержка общего ответа; обязательное точечное изменение, конкретный эффект измеряется.** При overdue Fire `next_job()` выбирает Both, если Pose тоже due. Worker сначала делает Pose, потом Fire, а Core получает только общий ответ после обеих моделей. Переставить Fire первым недостаточно: без partial response наблюдение всё равно удерживается до завершения Pose. [Scheduler:613–622](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L613), [worker:92–129](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/inference.rs#L92).

**Минимальный путь.** Сохранить однозапросный wire protocol. Если Fire уже опоздал и Pose тоже due, отправлять `Tasks::Fire`, чтобы Fire observation вернулась отдельно. Both оставлять для ситуаций, когда измеренная верхняя оценка total-cost укладывается в оба оставшихся deadlines. `CostEstimate` по worker/backend/shape/tasks, conservative EWMA + bounded tail reserve; cold/unknown cost означает conservative split, а не нулевую цену. При обычной нагрузке Both полезен, потому что переиспользует preparation/IPC.

Недостаток этого решения — лишний RPC/preprocess и риск starvation Pose при постоянной перегрузке. Поэтому decision требует per-model deadline debt: после Fire проверить старейший due Pose, вести отдельный miss counter, а фактический overload выставить явно. Нельзя гарантировать обе частоты scheduler-ом при demand выше service capacity. Не маскировать недополученные Pose под «пожарный приоритет».

Burst ограничить **доступной** частью бюджета worker после базовых требований всех его камер. Учесть действующие 2 s/10 s/3 fps и bounded priority. При замене interval на phase slots определить два независимых объекта: базовые слоты и лимит всех Fire executions во время burst. Burst не должен заранее закрывать будущий base slot; выполнявшийся после base boundary Fire может закрыть текущий slot. Чтобы не взорвать max-rate и не сдвинуть фазу, использовать явный fire-rate limiter и считать отложенный base-slot miss, а не скрыто пересчитывать phase от extra frame. В тестах зафиксировать границы burst и переход обратно. На нагруженном профиле extras могут быть нулевыми — это приемлемо, если базовая ранняя детекция проходит без них; burst не должен служить единственным обоснованием recall.

**Acceptance.** Fake worker с Pose=200 ms, Fire=20 ms: overdue Fire появляется в Core примерно через Fire request RTT, а не через сумму обеих моделей. Тест одновременно due Pose/Fire обязателен: старый `late_fire_precedes...` проверяет случай, где Pose ещё не due. На feasible synthetic workload ни один normal-priority camera/task не голодает, bounded high priority сохраняется. На infeasible workload оба deficit корректно видны; нет ложного PASS. Сравнить event latency/false events на одних кадрах с/без extra scheduling.

**Зависимости/эффект.** P01/P02. Улучшает deadline placement и раннюю доставку результата; не обещает рост sum fps. Partial response и перезапуск wire request посередине не нужны в первой реализации.

### P04 — Отслеживать upstream lag и ограничивать восстановление capture

**Статус: подтверждённая слепая зона, backlog — гипотеза до измерения.** `FrameClock` проверяет geometry, native/corrupt, PTS gap и hold/resume, но не сравнивает source progress с wall progress. Core-latest не гарантирует отсутствия запаздывания **до** Core. Decoder синхронно читает packets, делает decoded RGB и пишет полный кадр в pipe; медленная downstream стадия может задерживать следующую read. [FrameClock:1020–1054](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L1020), [decode:429–459](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-capture/src/native.rs#L429), [capture write:75–77](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/capture.rs#L75).

**Изменение.** `SourceProgress` рядом с FrameClock: `(anchor_pts, anchor_receive_instant, last_pts, lag_estimate, quality)`. Для live / paced replay считать рост относительного lag: `(receive-anchor_receive) - (pts-anchor_pts)`. Это не абсолютная задержка камеры: стартовая network/encoder latency неизвестна. Назвать поле `relative_source_lag_ms`, дать `unknown` до достаточного прогресса; unpaced replay не оценивать по этому правилу. Clock discontinuity открывает новую эпоху с явной причиной, а повторные reset не считаются здоровой стабильностью. Нельзя переякоривать на каждом позднем кадре — это обнуляет искомую проблему. Коррекцию drift ограничить и покрыть jitter/clock-skew тестами.

Постоянный backlog, уже накопившийся до первого anchor, этой формулой не обнаруживается. Для настоящего capture age нужен известный синхронизированный маркер сцены/потока либо достоверное отображение RTCP sender-report RTP→NTP с оценкой синхронизации и источника timestamp; controlled paced fixture даёт отдельно известную временную шкалу теста. Sender-report связывает часы RTP с NTP, но сам по себе не подтверждает момент экспозиции сенсора. Пока такой связи нет, `absolute_sensor_age = unknown`. Для межпроцессного timestamp протокола нельзя сериализовать Rust `Instant`: либо использовать durations из P01, либо explicit clock ID и handshake/correlation соответствующих Windows QPC/Linux MONOTONIC clocks с измеренной погрешностью. [RFC3550 §6.4.1](https://www.rfc-editor.org/rfc/rfc3550.html#section-6.4.1).

Если подтвердится persistent lag, ввести bounded catch-up **после декодирования**: split `next_decoded()` и `convert_rgb()` позволяет пропустить conversion/publication запоздавшего decoded frame, сохраняя codec reference state. Порог/длительность из профиля, с upper bound на skipped run. Если источник не догоняет в заданный бюджет — supervised reconnect с backoff и technical event; постоянный reconnect loop не выдавать за healthy. Для статичного кадра не превращать fps sampling в ложный frozen state.

Не начинать с случайного выбрасывания H.264/H.265 packets. Не выставлять UDP reorder buffer/max_delay в ноль вслепую: FFmpeg документирует RTP reorder queue и компромисс задержки/перестановок. `timeout` чтения не измеряет возраст контента. Проверять low-latency параметры на фактическом pinned FFmpeg build и TCP/UDP, сохраняя его confinement. [FFmpeg RTSP](https://ffmpeg.org/ffmpeg-protocols.html#rtsp).

**Wire ловушка.** `CaptureProcess` сейчас требует `header.frame == last_frame+1` (`capture_process.rs:286–289`). Если пропуск делается до IPC, `header.frame` обязан остаться номером **опубликованного** кадра; `decoded_frames`/`skipped_decoded_frames` — отдельные counters. Gap в source PTS уже переносится существующими timestamp fields. Иначе оптимизация нарушит протокол и запустит restart loop.

**Acceptance.** Throttled decoded source с advancing PTS: рост lag виден, wall fps ниже target, ни одна стадия не помечает такой источник нормальным только из-за fresh receive time. Controlled catch-up перестаёт публиковать старое, затем возвращается к realtime без неверного scope. TCP stall, UDP jitter/reorder, source clock drift, replay seek/repeat, portrait change и static live содержатся в короткой fault matrix. При gap политика сбрасывается по существующим rules, не строит неверный тренд через вырезанный участок.

**Зависимости/эффект.** Диагностика после P01 обязательна; catch-up только если есть измеренный backlog. Может улучшить freshness и CPU conversion, но не решает декодер, который физически медленнее realtime; такому сочетанию codec/resolution нужен иной профиль/аппаратный decoder.

### P05 — Уменьшить повторные RGB allocations без разрушения memory budget

**Статус: подтверждённые allocations, выигрыш не измерен; условное после P01.** Converter возвращает новый RGB Vec с row copy, Core на каждый кадр делает `vec![0; length]` и pipe read; это происходит до inference due check. Worker inference RGB и tensor уже reuse. Latest frame/Arc не устраняет preceding conversion/pipe. [Converter:612–680](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-capture/src/native.rs#L612), [Core allocation:291–309](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/capture_process.rs#L291).

**Изменение.** Сначала bounded recycling pool в Core keyed by frame byte length (или camera+geometry), возвращающий lease `FrameBuffer {Vec, budget_permit}`. `Frame` хранит lease, а последний Arc-drop возвращает его в pool. Permit остаётся у pooled Vec **включая idle**, иначе cache обойдёт raw budget. Idle eviction освобождает Vec+permit вместе. Не выдавать буфер заново пока его держит evidence/viewer/inference. Во время resize очистить несоответствующие idle buffers в пределах global cap; избежать reserve deadlock, когда все permits заняты cache не той geometry. Чтение partial pipe не публикует partially initialized Vec; только read_exact и validated header. Не вводить unsafe set_len ради экономии memset до отдельного доказательства.

В capture worker добавить API `read_rgb_into/recycle_rgb` для reuse его конвертируемого Vec после полного pipe write; отдельный процесс и маски перед export сохраняются. Это локально уменьшает allocator churn, не убирает RGB transport. Shared memory/ring descriptors — следующий, значительно более рискованный wire change; не нужен без измеренного доминирования IPC.

Масштаб потенциального трафика одного RGB-hop при 16×15 fps: 640×360 ≈158.20 MiB/s, 1080p ≈1423.83 MiB/s, 4K ≈5695.31 MiB/s. Это арифметический payload, не замер фактической RAM bandwidth. Потоки анализа и detail уже разделены; не уменьшать analysis image ещё сильнее до проверки малых/слабых дымовых regions.

**Acceptance.** Размер active+idle allocations никогда не превышает тот же budget; смена геометрии с занятыми Arc не зависает; cancellation/EOF возвращает lease; нельзя прочитать старые пиксели после короткого чтения. Memory high-water после освоения всех профилей стабилен. Existing capture admission/replay fault tests проходят. Измерить allocation count / elapsed under exactly same sources; если цена <значимой доли, не продолжать в shared memory.

**Зависимости/эффект.** P01, при необходимости P04. Убирает allocation/zero-init churn; не снимает decoding и копирование pipe автоматически.

### P06 — Включить просмотр и snapshots в тот же измеренный ресурсный профиль

**Статус: подтверждённая неполнота compute budget; обязательное для цели «16 камер + просмотр».** `media.rs` имеет max8 publishers, max4 concurrent snapshot encoders и max32 viewer sessions. Publisher разделяется по `(SourceScope, profile)`, что хорошо; detail может создать дополнительный RTSP capture. Каждый publisher отправляет native RGB в FFmpeg, масштабирует там и настраивает два encoder threads. Ограничение `workers*inference_threads <= logical cores` не ограничивает эту суммарную нагрузку. [Media limits](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/media.rs#L40), [publisher admission](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/media.rs#L458), [native RGB/threads](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/media.rs#L1051), [Core thread validation](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/main.rs#L757).

**Изменение.** Перенести hardcoded encoder/snapshot concurrency в validated execution-profile limits; отдельные weights для preview/detail source classes. Новый viewer существующего publisher не расходует повторно decoder/encoder permit; новый `(scope,profile)` расходует. При объявленной насыщенности reject **новый** просмотр с существующим capacity error, сохраняя анализ. CPU count не считать throughput proof: permits отражают измеренные допустимые комбинации. Отдельно учитываются detail frame bytes и decoder cost.

Профиль примера: 16×analysis 640×360/15 H.264 + `K_preview` 480px/5 + `K_detail` 1280px/12 + snapshot request limit. Конкретные K получают из короткой калибровки, не угадывают. На одном профиле минимум должен быть явно выбран командой продукта; нулевой K не закрывает функцию просмотра при анализе. Работа с 4K main даёт иной класс, даже если encoder output1280.

`snapshot()` сейчас каждый запрос вызывает encode (media.rs:766–795). Если P01 покажет цену: bounded short-lived JPEG cache keyed by `(SourceScope,capture_generation,frame_id,mask/scene_revision,encoding_profile)` с coalesced in-flight encode. Даже cached bytes повторно проверяются по текущему authorization, scope, usability/freshness после encode; старый JPEG не спасает expired scope. `Cache-Control: no-store` для HTTP остаётся. Не делать глобальный безразмерный image cache.

**Acceptance.** Расширить `whep_core.py:227–263`: он уже проверяет sharing detail decoder и рост ONNX counters при остановке detail, но не per-camera rates. Добавить measured wall/source cadence всех 16 во время разрешённых views, перехода preview↔detail, двух зрителей одного publisher, одновременных snapshots, закрытия последнего viewer и 100 bounded churn cycles. Excess view получает отказ до steady analysis deficit; освобождение permit позволяет следующий view; маски/revocation/freshness тестируются как сейчас.

**Зависимости/эффект.** P01 + P07 profile. Предотвращает неожиданную конкуренцию от интерфейса. Действующий код уже экономит на shared publishers; не заявлять этот эффект повторно.

### P07 — Честный hardware capacity profile и атомарный admission

**Статус: подтверждённый ограниченный смысл `capture_capacity`; обязательное.** Current capture capacity означает slots и три raw RGB frame на camera, проверяется в transaction при create/patch. Не означает inference capacity. `SourceProbeRequest` даже не содержит requested sampling. [Protocol:417–456](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-protocol/src/cameras.rs#L417), [required bytes](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/cameras.rs#L67), [transaction:231–268](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/cameras.rs#L231).

**Изменение данных.** Добавить отдельный trusted local `ExecutionProfile` (не путать с CPU-derived model manifest): ID/digest, hardware CPU/GPU/RAM/driver IDs, binary+ORT+CUDA-provider hashes, model-role hashes, image/tensor shape classes, decoder/codec/source rates, worker×thread setting, nominal sampling, per-camera minimum rates, declared simultaneous media/evidence workload, resource high-waters, qualification report digest. Новый model/fire-v2, ORT, hardware или sampling меняют digest; certificate от старого графа не переносится автоматически.

`AnalysisCapacity {state: verified|unknown|insufficient, profile_id, admissible_workload, limiting_resource, reason}` рядом с existing capture_capacity. Не переименовывать capture.fit в full-system fit. Source probe принимает optional desired sampling/profile, default backward compatible. Частоту входа нельзя достоверно узнавать из одного кадра: нужны advertised+short-observed data с quality flag; неизвестная source rate означает unknown class, а не бесплатный decode.

Самый простой admission — именованные проверенные workload classes с максимальным count и media allowances, а не ненадёжная онлайн ML-оценка произвольных камер. При наличии measured cost classes можно консервативно суммировать `Σ(pose_fps*c_pose + fire_fps*c_fire + capture_cost + evidence_cost)` по lane; combined savings учитывать только подтверждённые. Профиль явно содержит headroom, например инженерная стартовая цель utilization≤80%, но окончательное значение определяется худшим сценарием в квалификации. Средняя модельная ms не доказывает p99 latency.

В existing SQLite transaction пересчитывать compute reservation для **итогового** CameraConfig с заменой собственного резерва; create, enable, sampling patch, source/dimensions patch проходят один allocator. Если изменяется profile во время команды — compare profile generation; retry или conflict. Probe остаётся предварительной оценкой, два одновременных probe не резервируют одну ёмкость дважды. Reconcile после restart применяет тот же profile; mismatch даёт явный degraded/unknown, а не молчаливое снижение частоты.

**Acceptance.** Concurrent creates/patches не превышают declared capacity; disabling/replacing освобождает ровно старый резерв; idempotent retry не дублирует reservation; изменение hashes делает прежний certificate непригодным; GPU профиль не применим к CPU fallback. Profile advertised16 проходит 16, включая худшую камеру и media allowance. Capacity failure сообщает ресурс и допустимый проверенный вариант. CPU4 профилю не присваивается «16 completed».

**Зависимости/эффект.** P01, P06, P09, P13. Не увеличивает скорость; делает ограничения управляемыми и не позволяет UI/installer обещать неподтверждённый профиль.

### P08 — Не переписывать sharding до доказанной imbalance

**Статус: потенциальная проблема неоднородных источников, не доказанная причина нынешнего homogeneous 16-test; условное.** New cameras назначаются lane с минимальным **числом** камер, затем assignment статический. Это не `camera_index % workers` и в равномерном 16/4 распределяет примерно четыре на worker. Локальные policy maps принадлежат lane, поэтому слепой work stealing уничтожает/перемещает temporal state некорректно. [Registry:674–730](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/registry.rs#L674), [policy ownership:327–341](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L327).

**Минимальное изменение при доказанном перекосе.** На creation/reconcile назначать по сумме проверенных costs: geometry/source class + requested sampling; существующие камеры не перетасовывать каждую секунду. Add per-lane utilization and service-time distribution. Если noisy costs делают баланс хуже, сохранять static count path для однородного профиля. При необходимости explicit reassignment только на safe boundary с переносом camera-owned `Analysis` и `HazardPolicies`, сохранением scope, in-flight generation fencing и без двойной обработки.

Глобальная очередь статeless inference workers — следующий архитектурный вариант: per-camera actor владеет policies, phase clocks и claims; workers возвращают только observations; dispatcher может назначить любой ready worker, но outstanding task claims ограничены. Это требует вынести temporal policy из lane loop и явно сериализовать результаты по camera. Не делать это первой реализацией, пока P01 не покажет свободный worker при одновременном голодании другого.

**Acceptance.** Mixed 16 источников: 4:3/16:9, low/high pixel counts и crowds, несколько slow decoded sources. Сравнить static count vs static weighted при неизменных models/rates; camera reload/fault не сбрасывает чужую FSM. Тест late result после reassignment не изменяет новую state. Нет обещанного прироста homogeneous profile.

**Зависимости/эффект.** P01/P07. Может выровнять worst-camera cadence. Не увеличивает общую service capacity и не нужен CUDA профилю, который сейчас допускает один inference worker.

### P09 — Квалифицировать реально исполняемые CUDA роли без удаления провала из отчёта

**Статус: подтверждено; обязательное решение о scope выпуска.** Live worker загружает PoseBalanced+Fire. Parity driver проверяет `ModelId::ALL`, включая Pose. Committed CUDA report провален единственно `pose/gray` по mean abs; активные роли прошли по девять сохранённых cases каждая. Это не означает, что вся CUDA или все сцены уже qualified. `models.lock` schema2 требует CPU reference provider и все три graphs; менять `execution_provider` на cuda сломает валидацию и смешает происхождение reference с местом исполнения. [live roles](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/inference.rs#L45), [all-model verifier](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/parity.rs#L56), [CUDA report](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/models/evaluations/rust-linux-cuda-parity.json), [manifest constraints](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/pack.rs#L229).

**Конкретный путь.** Сохранить immutable artifact manifest и проверку безопасности/хэшей **всех** его graphs. Ввести отдельный execution qualification sidecar из P07: exact approved role mapping `person→pose_balanced`, `hazard→fire`, backend CUDA FP32, shapes, ORT/CUDA/cuDNN/driver/hardware fingerprint, tolerances version, corpus/report hash. Core и worker независимо проверяют, что выбранная role входит в разрешённый профиль; путь запуска Pose на этой qualification запрещён. Это не удаление Pose из ModelId::ALL и не фиктивное PASS всего model pack.

Именно `PoseBalanced+Fire` на GPU — главный путь к 16: он сохраняет действующий выбор моделей и нынешние входы tracker/fall/fire policies, а переносит вычисления в проверяемый ускоренный backend. Замена Balanced на nano Pose ради fps затронула бы и качество, и как раз проблемную CUDA роль. Старый GPU throughput ещё ниже цели; дальнейший запас необходимо получить измерением и адресными исправлениями либо выбрать более производительный GPU. Full-pack CUDA defect остаётся отдельным незакрытым пунктом до P10, а промежуточная пригодность активного профиля не превращается в «100% всего model pack».

CLI выдаёт одновременно `artifact_validation`, `full_pack_numeric`, `active_profile_numeric`, `active_profile_events`, `capacity`. Известный full_pack failure сохраняется; активный профиль может стать release candidate только после его собственных численных, observation/event и capacity gates. Для нового fire model sidecar и квалификация перевыпускаются. Нельзя назвать профиль qualified исключительно потому, что 18 старых fixtures зелёные.

**Acceptance.** Попытка активировать `pose` в narrowed CUDA profile отклоняется; digest mismatch/CPU fallback/другая shape/старый report не принимаются; CPU manifest validation сохраняет прежние проверки. Smoke test именно installed Core доказывает вызов двух разрешённых roles. Исходный all-model report остаётся failed, пока P10 не решит конкретное расхождение.

**Зависимости/эффект.** Параллельно P01/P02. Позволяет честно не блокировать реально используемый профиль неиспользуемой ролью, но не ослабляет численные bounds и не закрывает G5 целиком.

### P10 — Закрыть численный дефект и сделать observation comparisons настоящими gates

**Статус: подтверждено; обязательное для full-pack CUDA, расширение gates — для любого ускорения.** `pose/gray` mean=0.000177251851... при 0.00015, changed anchors=0. TF32 false, cuDNN heuristic/max workspace variants и смена graph optimizations уже исследовались; повторять эти переключатели как новую идею не нужно. В текущем `compare_observations()` считаются score/box/keypoint/histogram errors, но fatal только missing/extra/flag differences; matching выбирает лучший IoU одного класса без минимального IoU. Поэтому численно заметно сдвинутая единственная detection может остаться matched. [Parity:121–127](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/parity.rs#L121), [matching:183–237](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/parity.rs#L183), [bounds:247–281](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/parity.rs#L247), [prior experiments](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/IMPLEMENTATION_STATUS.md#L147).

**Диагностический commit.** На неизменном gray input сравнить четыре траектории: reference PyTorch CPU, PyTorch CUDA, ORT CPU, ORT CUDA. Reference фьюзинг/weights/ONNX export фиксировать. Детерминированно несколько повторов: отличить nondeterministic scatter от устойчивой backend difference. Посчитать ошибки по output channels, confidence-threshold proximity, coordinates/keypoints; получить ORT profiling/provider node placement. В debug copy ONNX временно вывести intermediate nodes и бинарным поиском найти первый оператор, где расходится сверх ожидаемого baseline. Debug graph не выпускать как новый reference. CPU-vs-CUDA error не считать автоматически допустимой только потому, что anchors совпали.

**Исправление.** После локализации — минимальная корректная замена export/operator/shape path; новая immutable graph revision с requalification. Если конкретную Pose роль не удаётся исправить в этом цикле, full-pack CUDA остаётся не закрыт, а narrowed P09 path честно ограничен. Не повышать mean limit до увиденного числа. Любая новая tolerance — отдельная заранее обоснованная численная спецификация с независимым корпусом и event sensitivity, не способ перекрасить один провал.

Если ошибка локализована в одном CUDA operator, есть запасной путь: CPU-исполнение **минимального проблемного subgraph**, а не всей PoseBalanced. Это условный вариант после сравнения transfer cost. Не обещать простой существующий флаг «данный node на CPU»: текущий builder не предоставляет такого контракта. Реализация потребует явного разбиения graph на проверяемые sessions/subgraphs либо поддерживаемого provider partition mechanism, описанного в новой immutable manifest revision: hashes, boundary tensors, dtype/shapes, execution order, H2D/D2H synchronization. Одни и те же inputs должны давать допустимый output и достаточный throughput. `error_on_failure()` при регистрации CUDA не является доказательством, что каждый node исполнился на GPU; placement фиксируется ORT profiling. Если fallback слишком дорог или разбиение сложно, исправление export/operator предпочтительнее. Для активного Balanced+Fire такой fallback не вводить без выявленного численного дефекта.

`ObservationTolerance` сделать входом immutable qualification spec: min matched IoU, score max, pixel/normalized box and joint displacement, joint conf, histogram L1, tensor pixel equality. Сопоставление deterministic и one-to-one; низкий IoU означает missing+extra. `pixel_error` preprocessing тоже gate. Для изменённых CPU-only preprocessing helpers требовать битового совпадения tensor там, где это математически возможно. Для GPU runtime никаких обещаний универсальной битовой идентичности: остаются старые tensor bounds + новые observation gates + event equivalence.

**Acceptance.** Unit: одна detection с тем же label, но перемещённым box обязана провалиться; равночисленные переставленные detections корректно match; confidence рядом с 0.20/0.15, NMS ties, tiny smoke, joints/hist deltas имеют заранее утверждённые bounds. Для temporal policies прогнать identical frame sequences CPU/GPU: одинаковые классы и допуски delay/review/urgent заранее, случаи вблизи thresholds явно видны. Полный набор всех model roles сохраняется; active pack — дополнительный, не замещающий результат.

**Зависимости/эффект.** P09, взаимодействие с ML-спецификацией quality. Убирает риск «ускорили, но незаметно изменили решение»; сам по себе throughput не улучшает.

### P11 — Прогрев shape inventory и осторожный CUDA device input reuse

**Статус: подтверждено, ускорение условное.** Startup CUDA прогревает лишь изображение 640×360, для rectangular preprocessing это 384×640. Смешанные aspect ratios создают другие tensor shapes и allocations после readiness; существует live deadline2 s. Host TensorRef проходит `session.run`, output извлекается на CPU. ORT документирует hidden H2D/D2H внутри Run при host inputs/outputs. [Warmup:55–68](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-worker/src/inference.rs#L55), [host input:240–253](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/lib.rs#L240), [ORT I/O Binding](https://onnxruntime.ai/docs/performance/tune-performance/iobinding.html).

**Обязательная малая часть.** В execution profile перечислить поддерживаемые tensor shapes и прогреть обе активные роли на каждой до READY, в существующем bounded startup budget. `warm_shapes` — валидированные sizes, dedupe, bounded list, estimated VRAM high-water. Произвольная новая shape не наследует certificate автоматически. Если она нужна, профиль расширяется и квалифицируется; холодное прогревание на работающем единственном GPU worker может задержать все 16 камер, поэтому его нельзя делать скрытно как часть добавления камеры. На first-run новой shape explicit starting/unqualified state; предпочтительнее небольшое заранее разрешённое множество актуальных aspect ratios. Не удлинять live2 s timeout до 60 s для скрытия cold cost.

**Условная оптимизация.** Если stage profile показывает заметные copies/allocations: `PreparedTensor` с Host и Cuda вариантом; worker один раз prepares и uploads вход, Both последовательно передаёт одну device allocation двум sessions, а outputs копирует на host для существующего decoder/postprocess. Buffers строго по bounded shape inventory, lifetime до завершения обеих models, explicit stream synchronization; ошибки не дают читать старый output. Cpu path/PerceptionBackend callback сохранить. Не обещать zero-copy: исходный RGB остаётся host, и output пока нужен CPU. Для 384×640 input составляет 2.8125 MiB; экономия H2D на совпавшем Both измеряется, а не предполагается bottleneck на PCIe.

CUDA Graphs не первый патч: ORT требует постоянных addresses/shapes для graph ID, I/O binding, все nodes на CUDA и serial run одного session; captures хранятся до уничтожения session. Динамическое создание graph ID под произвольные dimensions рискует VRAM growth. Реализовывать только после P10, bounded shapes и доказательства launch overhead. [ORT CUDA Graphs](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html#using-cuda-graphs-preview).

**Acceptance.** Cold start и restart всех разрешённых shapes ≤declared startup budget; первая live-frame каждой shape ≤2 s. Mixed-shape loop после прогрева не увеличивает VRAM неограниченно. Full observation/numeric/event gate до/после unchanged. Fault после upload не принимает stale result. Измеряется wall RTT и GPU timing, не только время до asynchronous enqueue.

**Зависимости/эффект.** Warmup — P07/P09; device reuse — P01/P10. Экономия переносов/allocator возможна, но не обещает требуемые 27% и не заменяет более подходящее железо, если общий capacity всё равно не хватает.

### P12 — CPU/preprocess tuning с коротким конечным search, без «крутить все knobs»

**Статус: текущие возможности подтверждены, выигрыш гипотетичен; условное.** ORT pool уже global per worker, inter-op1, spin false; sessions выполняются последовательно. Capture decoder настроен threads1. Core runtime, encoders, snapshots, evidence и network тоже потребляют CPU. `Preprocessor` cache держит только последнюю geometry и пересчитывает sample maps при каждом чередовании shape; исходные RGB640×360 с gain1 всё равно идут через bilinear integer math. Appearance сканирует torso ROI, а не весь frame для каждого человека. [Pool](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/lib.rs#L120), [decoder threads](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-capture/src/native.rs#L408), [preprocess:128–168](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/preprocess.rs#L128), [appearance](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-inference/src/appearance.rs#L37).

**Изменение.** На данном host ограниченный набор worker×thread кандидатов с сохранением CPU headroom, например 4–6 обоснованных комбинаций, а не полный Cartesian sweep. Для каждой одинаковые sources, phase, media load, warmup; выбрать по worst-camera p95/p99, не peak throughput одной камеры. Результат становится config execution profile. Не увеличивать число workers на CUDA вопреки существующему one-worker contract и VRAM replication без отдельной архитектурной работы.

Если prepare значим: exact no-resize RGB→NCHW branch, сохраняющий текущую `/255.0`, padding и канал-порядок; bounded cache resize maps по geometry (не cache кадров/tensors всех камер). Existing area2x special case оставить. Если appearance значим: после измерений оптимизировать polygon scanline, сравнив точные включённые pixels и histogram; не удалять histogram или joint filtering, от которых зависит tracking. Anchor candidate scratch buffers можно reuse, только если postprocess allocations реально заметны.

**Acceptance.** Tensor exact equality для random image dims/edge pixels/identity/area2x и existing rect fixtures; output/event gates P10. Mixed geometry cache cap, hits/misses и memory bounded. Crowd fixtures проверяют worst-case appearance. Candidate profile проверяется с media workload и с source resolutions, которые он рекламирует. Нет обещанного четырёхкратного ускорения от 16 workers.

**Зависимости/эффект.** P01/P07/P10. Изменения маленькие и обратимые, но реализуются по measured bottleneck. Batch inference не часть этого патча: current export.dynamic касается H/W/anchors, batch остаётся 1; runtime/manifest/output checks тоже batch1. Для batch2/4 нужны новая graph revision, result demux, max batch wait, shape grouping, fair queue и вся requalification. Это optional escalation, если более простой путь не достигает 16.

### P13 — Превратить benchmark в ограниченную по времени приёмку с настоящим verdict

**Статус: подтверждённые ограничения harness; обязательное.** `qualify.py` по умолчанию warmup6 s при cadence window10 source seconds. Все камеры получают копии одного подготовленного 15 fps clip. API polling sequential, общий point берётся до опроса всех камер. Percentiles `last_worker_ms` являются sampled status, не distribution реальных executions. После записи report нет общего acceptance verdict, а successful script означает лишь успешное измерение. Эти ограничения не обесценивают сохранённые числа, но не позволяют объявлять final qualification. [fixture](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L42), [sampling](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L145), [last-value percentiles](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L193), [default warmup](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L274).

**Изменение.** Один authoritative протокол **Fast RC v2, 180 минут на профиль**. `qualify.py` остаётся collector: собирает immutable run inputs, counters, samples и ошибки измерения, но не выдаёт release PASS. Новый `validate_report.py` — единственный validator: проверяет полноту manifest и заранее зафиксированные gates, пишет `PASS|FAIL|INCOMPLETE`, machine-readable причины с observed/required/evidence и nonzero exit для последних двух. Новый `fast_rc.py` — orchestrator: запускает стадии, задаёт общий deadline180 min, записывает fault ledger, вызывает collector и validator, выполняет cleanup. Orchestrator может только распространить verdict валидатора, а не самостоятельно превратить частичный запуск в PASS. Все три роли используют один `run_id`, candidate/profile digest и версию Fast RC v2; наличие report.json или exit0 collector не означает принятого релиза.

Readiness barrier включает все камеры с expected source/model scope, полный cadence warm window и все warm shapes; timeout не превращается в исключение знаменателя. Clock каждого sampled camera берётся после ответа или в серверном atomic snapshot; исполненные model timings берутся из P01 histograms. Source epoch changes, missing cadence, unplanned degradation и no observations считаются отдельно и не исчезают из coverage. Изолированный запуск обязателен для capacity; если соседний процесс нагружает CPU/GPU, report помечен interference и не сертифицирует capacity.

Отдельный fixture manifest задаёт источник каждой камеры, кодек, геометрию, rate, start offset, phase class, нормальные сцены/толпа/ранний дым/негативы. Камеры действительно отличаются; имеются aligned и staggered source phases. Изменённый transform/dataset hash входит в report. Записывать CPU model/physical+logical cores, RAM, GPU name/VRAM/driver, OS, binaries, ORT/provider libs, model+policy+profile digests, worker threads, source and tensor geometries, media viewers, actual warmup, test start/end, injected fault interval ledger, known external interference.

**Размещение performance-проверок в едином Fast RC v2.** Отдельного capacity/numeric budget и второго release verdict нет. Cold build, скачивание datasets, установка зависимостей и подготовка runners выполняются **до**180 min. Численные результаты, полученные заранее, допустимы только с точно теми же candidate/model/runtime/profile digests; их проверка и привязка к текущему run входят в preflight.

- **0–15 мин:** artifact/profile preflight, численная проверка активных ролей до этой стадии или внутри неё, проверка неизменности reference cases, startup всех allowed shapes, фиксирование cold costs. Это проверка готовых артефактов, не обещание успеть cold build.
- **15–40 мин:** evaluator regression, virtual clocks, seeded state traces и pre-aged boundaries из T02–T04; native ramp не конкурирует с этими измерениями.
- **40–80 мин:** ramp1→4→8→16, concurrent admission/patch, преднамеренный overload и recovery распределяются внутри общей functional/fault стадии. Здесь же короткие heterogeneous/burst/shape cases и viewer churn; они не съедают последующий clean 60 min интервал. Профиль CPU4 проверяет отказ сверх своей ёмкости и не объявляет 16 поддерживаемыми.
- **80–105 мин:** общая reliability стадия storage pressure/atomicity/crash cutpoints. P01 counters и detection canaries продолжают собираться; намеренно нарушенную нагрузку не включать в clean capacity verdict.
- **105–165 мин:** полные 60 min непрерывной quiet нагрузки на заявленном максимуме профиля. Для целевого GPU это 16 камер плюс объявленный preview/detail/snapshot/evidence workload. Readiness/warmup barrier завершён до 105 мин; вычесть его из обязательных 60 мин нельзя. Нет fault injection, параллельной сборки или другого нагрузочного прогона; заранее включённые negative/canary сцены являются частью фиксированной workload. Получить per-camera wall/source cadence, tail latency, actual useful detections, thermal/resource high-waters.
- **165–180 мин:** финальный end-to-end canary, bounded cleanup/restart/restore, ресурсный diff и единственный machine verdict от `validate_report.py`. Проверить отсутствие оставленных процессов. Cleanup не объявлять успешным, пока не проверены его результаты.

**После каждого injected fault — known detection canary**, а не лишь ready/counter growth. Если Fast RC v2 не укладывается в общий deadline или mandatory cases/clean 60 min не завершены, validator выдаёт INCOMPLETE с перечнем непокрытого, а не PASS по пройденному кусочку. Зафиксированное нарушение gate остаётся FAIL даже при последующем истечении deadline; неполное покрытие также отражается отдельно. Profile sets запускаются параллельно только на независимых hosts/devices. Windows и Linux на одном host требуют отдельных последовательных 180 min запусков; на общем CPU/GPU нельзя назвать одновременно запущенные тяжёлые тесты независимыми.

### 7.1. Предлагаемые численные критерии профиля

Действующие source-clock floor code values — Pose4 и Fire1.5 fps при nominal5/2 (`pose_fps×48` и `fire_fps×45` per minute при десятисекундном окне). Их сохранять как нижние 10 s source окна, не путать с полностью достигнутой целью 5/2 и не считать готовыми wall-clock SLO. Для полноценного target profile предлагаю дополнительно per-camera wall rate≥95% nominal в каждом полном 60 s окне: Pose≥4.75, Fire≥1.90, то есть минимум 285 и 114 принятых результатов соответственно. Окно определить как `(t−60s, t]`, брать accepted монотонные timestamps, оценивать каждую секунду после его заполнения. Source quantization тестируется отдельно целым числом slots; она не даёт второй, незаявленной скидки к этим wall limits. Это **предлагаемый новый acceptance contract**, не текущий green status. Значения фиксируются в одном profile manifest и читаются validator, а не дублируются разными числами в collector/orchestrator. Команда может выбрать более строгий контракт до запуска, но не менять его после просмотра результатов.

Дополнительные обязательные критерии:

- Полное покрытие всех камер, объявленных в profile manifest, весь steady interval; для целевого 16-camera профиля — всех 16. Нет unplanned degraded/missing cadence/epoch churn, прячущего плохие измерения; преднамеренные fault windows отдельно обозначены, report содержит и общий, и steady denominator.
- Core receive→accepted result p95≤500 ms и p99≤1 s как **предлагаемая** инженерная latency цель профиля, по каждому task каждой камеры в полном clean интервале; все завершения и причины rejection учитываются отдельно. Статистика только быстро принятых результатов не скрывает timeout/stale failures. Stale>3 s никогда не принимается по существующему freshness guard. Это не абсолютная задержка от объекта перед камерой. Fire-specific deadline statistic и worst camera важнее aggregate p95.
- Нет нарастающего relative_source_lag; предложенный actionable threshold>1 s sustained либо drift>500 ms/60 s после стабилизации должен включать clock-quality context, а не penalize normal jitter. Absolute latency измеряется отдельным синхронизированным тестом при необходимости.
- Установленные startup≤60 s CUDA/≤30 s CPU и live infer≤2 s сохраняются. Recovery budgets связываются с реально действующими deadlines/backoff, отдельно учитывают Windows RTSPS startup. Нельзя обещать 2 s full recovery только из-за 2 s timeout одного запроса. [Startup deadline](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/inference_process.rs#L38), [request deadline](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/inference_process.rs#L77).
- Все активные numerical/observation/event gates P09/P10 проходят. Положительные canaries после faults доказывают восстановление полезной работы; capacity pass не закрывает независимую smoke/steam discrimination оценку.
- Нет роста child/FD/socket count после 100 закрытых cycles. RSS/VRAM сравниваются **после** warmup всех разрешённых shapes и библиотек с заранее заявленным high-water; ORT arena, удерживающая peak allocations, сама по себе не утечка. Выход за profile cap — failure, бесконечный линейный рост — failure.
- Неожиданное нарушение gate даёт FAIL; недостающие mandatory данные, незавершённый clean 60 min или deadline без полного покрытия дают INCOMPLETE, если уже не установлен FAIL. Преднамеренные fault timeouts сопоставляются с ledger и проверяются по recovery/canary gates, а не автоматически валят ожидаемый fault test. Оба отрицательных verdict дают nonzero exit `validate_report.py` и `fast_rc.py`. Release tooling принимает только exact digest report этого validator, не строку «ready», exit0 collector или наличие report.json.

## 8. Клиенты, Firebase и завершение продукта

### C01 — Уведомление не должно исчезать при Home или потере связи

**Статус: подтверждённое поведение кода, P0 для текущего notification lifecycle.** В LocalAlerts._connectionChanged условие !foreground или link != connected одновременно останавливает polling и вызывает _removeAll. Последний метод отменяет показанные native notifications. Исчезновение транспорта не подтверждает ACK, resolve или окончание attention cycle. [alerts.dart:209–228](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/alerts.dart#L209-L228), [359–371](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/alerts.dart#L359-L371), [main.dart:41–49](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/main.dart#L41-L49).

**Изменить:** разделить управление соединением и жизненный цикл показанного уведомления. Home/disconnected останавливает foreground sync, но не отменяет действующий alert. Удаление разрешается по достоверному ACK/resolve/silence/expiry, явному отключению уведомлений либо потере авторизации. При offline не продлевать TTL; показывать последнюю подтверждённую актуальность. Хранить ограниченный persistent notification index с устойчивым идентификатором core/device/incident/cycle; после рестарта сверять с native delivered notifications и авторитетным состоянием Core. Отсутствие на первой странице feed не считать исчезновением инцидента — зависимость R07.

Native ACK сейчас требует точного совпадения revision из payload и при расхождении молча выходит. Направить действие через общий incident action dispatcher: повторно получить текущий incident, проверить cycle и права; при изменившемся evidence открыть обновлённую карточку и явно показать, что действие не применено. Не ACK-ать новый cycle старой кнопкой. [alerts.dart:331–356](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/alerts.dart#L331-L356).

**Проверки:** urgent показан → Home → сеть пропала → app process restart → уведомление сохраняется до допустимого исхода; offline expiry, revoke, changed revision и новый cycle; в каждом случае receipt, ACK и resolve различаются. C01 можно закрыть до реализации нового фонового транспорта.

### C02 — Реализовать отдельный фоновой маршрут мобильных уведомлений

**Статус: функциональный пробел, а не только отсутствие теста.** Просмотренный путь — трёхсекундный foreground polling. Android manifest не объявляет alarm service, iOS AppDelegate не регистрирует remote notification transport. Существующее успешное foreground уведомление не означает получение нового события при suspended/terminated приложении. [LocalAlerts](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/alerts.dart), [AndroidManifest](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/android/app/src/main/AndroidManifest.xml), [AppDelegate](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/ios/Runner/AppDelegate.swift).

**Предпочтительная реализация:** общий NotificationTransport с одним durable alert/cycle contract, foreground adapter и платформенными адаптерами. Для iOS — обычный видимый APNs alert через отдельный delivery worker/relay, регистрация и обновление device token, signing entitlement, expiry/collapse identity и action handler. Core за NAT передаёт в relay только разрешённый минимальный envelope через исходящее аутентифицированное соединение. Кадры, токены Core и диагностические подробности в push не включаются. Silent push не использовать как обязательный механизм пробуждения: Apple не гарантирует его доставку. [APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns), [background delivery](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app).

Для Android direct mode — явно включаемая пользователем с переднего плана native foreground service с постоянным статусным уведомлением, защищёнными credentials и поддерживаемым клиентским протоколом. Добавить AlarmService.kt, service declaration, необходимый manifest service type/permissions и reconnect/receipt ownership независимо от Activity. Тип службы выбирается по реальной функции и разрешённому deployment; specialUse требует описания случая и отдельной проверки правил распространения, это не обход ограничений. Такой direct mode требует доступности Core/VPN/relay; если нужен иной push-provider, реализовать его отдельным квалифицированным adapter. [Android service types](https://developer.android.com/develop/background-work/services/fgs/service-types).

**G3:** не включать FCM автоматически. Его официальная документация содержит запрет определённых life-critical/high-risk сценариев; название «вторичный канал» само по себе не разрешает использование. Зафиксировать реальный сценарий и допустимый транспорт; APNs и direct service тоже проверяются по собственным условиям и capabilities. [FCM](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-priority#life_critical_use_cases).

**Готово:** новая тревога, возникшая после ухода UI в фон, наблюдается на настоящих Android/iPhone в поддержанных состояниях; provider acceptance, OS presentation, client receipt и ACK не смешиваются. Force-stop, deny и Focus фиксируются как ограничения доступности, без ложного receipt. Зависимости R07/R08, C01/C03/C07.

### C03 — Короткая физическая матрица вместо бесконечной ручной проверки

Подготовить единый device-run sheet с build hash, OS, model, network, permission/channel settings, действиями и результатами. На каждом реальном телефоне проверить foreground, Home, lock, штатное завершение процесса, пользовательский force-stop, denied permission, разрешение после отказа, DND/Focus/silent, Wi-Fi→mobile/VPN, Core offline/reconnect, stale push и device revoke. В каждом состоянии отдельно отмечать ожидаемый результат и поддерживаемость; например force-stop не переименовывать в успешную доставку.

Использовать тестового получателя и controlled canary из полного видеоконвейера; для проверки исключительно native UI допустим отдельный delivery fixture, явно помеченный как fixture. Сценарий foreground→Home→new incident нельзя заменять тем, что уведомление было показано ещё до Home. Проверить реальный звук, не только успешный возврат API.

Для камер провести компактные физические UVC/ONVIF/dual-profile случаи: enumeration, disconnect/reconnect, credentials, geometry change, scene mask, несколько viewers. Внешний NAT проверить телефоном через мобильную сеть с TURN/TLS, без локального Wi-Fi; refresh, revoke и recovery пройти в той же топологии. Существующие VM/namespaces результаты сохраняются и уменьшают число повторных ручных шагов. [Открытые физические gates](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/docs/acceptance.md).

**Готово:** матрица имеет наблюдаемые результаты на подписанных артефактах и реальные пределы capabilities. Ни один обязательный физический случай не закрыт скриншотом симулятора. Это конечный набор действий, для него не нужно ждать сутки.

### C04 — Довести Firebase route до воспроизводимой эксплуатации

Текущий worker умеет cloud_node/hybrid_web, долгий browser poll, route guards, отдельные credentials и App Check через token_file/development_debug. Production issuance в репозитории описано как внешняя обязанность хоста; готового автоматического issuer в этих исходниках нет. [config.js](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/packages/firebase-worker/src/config.js), [app-check.js](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/packages/firebase-worker/src/app-check.js), [документация](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/docs/firebase-worker.md).

**Изменить:** оформленный host service/process profile для Node и явный dedicated Chrome deployment, health/capability status для local/cloud раздельно. Для production — документированный разрешённый App Check issuance/refresh process, атомарная замена expiring token, видимая ошибка до истечения запаса, исключение debug secret из release-конфигурации. Конкурсный debug profile отделён от production. Сохранить явный opt-in dedicated browser storage и отсутствие зависимости локального ONNX от браузера.

**Короткая матрица:** local-only с загруженной моделью → restart Chrome/Node → новый ответ именно on_device; prefer-local при недоступности модели → подтверждённый in_cloud только при consent; без consent ноль cloud dispatch; hidden/frozen/discarded tab; malformed/refusal/timeout/429; App Check expiry/rotation; отмена consent во время request. Готовность handshake не заменяет реальный вызов. Проверять current inference source из результата SDK, не из желаемого route.

provider-models.lock.json содержит live_evaluation_qualified=false. Формально успешный transport не даёт права считать качество live semantic оценок принятым; квалифицировать модель+prompt и разрешённые действия профиля совместно с F-карточками urgent. [Model lock](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/packages/firebase-worker/provider-models.lock.json).

**Готово:** R04 job recovery проходит, actual local/cloud маршруты воспроизводимы после отказов, production credentials обновляются штатно, optional provider failure не останавливает локальный анализ. Версии Chrome/SDK/model и cold costs входят в manifest.

### C05 — Согласовать изменения с четырьмя SDK и Companion

Из R07/R08, P01/P07 и новых evidence contracts следуют изменения schema/API, а не только внутреннего Rust-кода. Обновить sw-protocol, OpenAPI/JSON Schemas, generated TypeScript, Rust async/blocking, Python sync/async, Dart и TypeScript Node. Сохранять string u64, stable operation id, cursor generation, role/scope guards и pinning Core identity.

Общий fixture расширить: новый optional field, неизвестный enum с безопасным fallback, partial pagination, expired cursor, duplicate response, lost refresh response, rotation сохранена после процесса restart, scope revoke во время stream, changed evidence/cycle при ACK. Single-flight refresh сам по себе не решает lost-response; нужен протокол R08 и durable pending request. Credentials/result cache не попадают в logs/support bundle.

PendingIncidentAction в Companion сейчас хранится в памяти, с двухминутным сроком и строгой привязкой к просмотренному evidence. Не добавлять бесконечную повторную отправку после рестарта. Если заявляется сохранение offline command между запусками, сохранять ограниченную очередь в protected local store с исходными revision/generation/evidence/expiry/idempotency key и повторной проверкой; иначе явно завершать pending action с видимым объяснением. [profiles.dart](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/apps/companion/lib/profiles.dart#L20-L61).

**Готово:** один shared fixture проходит во всех SDK; существующие pinned TLS/reconnect tests и dependency-isolation gate сохраняются: SDK не зависят от engine/ML, Rust поддерживает async/optional blocking, Python — sync/async. N/N−1 проверяется против двух реально поддерживаемых контрактов; до появления второй поддерживаемой версии этот конкретный пункт — обоснованное not-applicable, не фиктивный PASS.

### C06 — Закрыть access/privacy regression после изменений

Граница исправления: новые alert registrations, pagination, compute admission, model activation и media changes обязаны использовать существующие авторизацию, owner/scopes, instance identity и scene revision. Новых сетевых поверхностей для тестового clock/fault control в release нет.

Расширить tests/e2e/lan_core.py, sdk_core.py, whep_core.py и соответствующие sw-api tests: чужая камера в alert cursor; revoke во время send/view; privacy mask изменена между proposal/crop/evidence; старый detail stream и старая epoch; два одновременных enable/patch на последнюю compute reservation; неподписанная/чужая model qualification; bounded upload/IPC dimensions; diagnostics с redacted credentials. Исходный frame за маской не должен попасть ни в verifier persistence, ни в Firebase, ни в push. Выполнить существующие dependency/secret scans на точном release tree и приложить результаты к manifest.

**Готово:** прежние security invariants сохраняются на новых маршрутах; новые endpoints не доверяют client-provided camera ids без server scope filtering. Это регрессия конкретных изменённых границ, а не предложение заново писать систему авторизации.

### C07 — Завершить пакеты, установки, обновление и rollback

Собрать один frozen release candidate из чистого commit после F/R/P/C-изменений. Зафиксировать native runtime versions/compile options, SQLite runtime version, model provenance, enabled features, executable hashes и SBOM. Test harness/fault hooks не входят в отгружаемые binaries. Model activation принимает только квалифицированный профиль и сохраняет предыдущий rollback target.

Исполнить короткие установочные сценарии: Windows SCM/LocalService и camera credential access; Linux systemd service account; DEB/RPM/OCI CPU и заявленный GPU pack; install→reboot→new live canary; upgrade database→restart→canary; rollback через совместимый backup; uninstall сохраняет данные. Новые schema migrations для jobs/refresh/alerts проверяются на предыдущей базе, полупройденной операции и повторном запуске. Restore меняет stream generation, сохраняет identity в соответствии с существующим контрактом и не отправляет историю как новые live alerts.

Подписать Windows/Android и Apple device artifacts подходящей release identity; iOS unsigned archive не равен устанавливаемому приложению. Проверить inventory Companion без model/engine assets на готовых подписанных файлах, а не на предыдущем build directory. [Текущая packaging/recovery evidence](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/docs/acceptance.md).

**Готово:** опубликованные пользователю артефакты совпадают с manifest и проверенной конфигурацией, есть воспроизводимые install/update/rollback инструкции. Доступ к signing identity — явная внешняя зависимость с владельцем, не заменяемая выдуманной подписью.

### C08 — Закрыть G1/G3/G4 и операционную документацию

**G1:** подготовить фактическую схему Core ONNX + отдельный Firebase Web worker + Companion; показать настоящий on-device ответ в Chrome и consented fallback. Для Web submission сделать worker page понятной демо-поверхностью: текущий masked packet, фактический route, результат и связь с incident. Не показывать секреты браузерного/Core подключения. Android/iOS Companion не выдавать за on-device Firebase интеграцию на этих платформах, если её там нет. По неоднозначной архитектуре сохранить решение организатора либо документированную аргументацию соответствия, не задерживая инженерные фиксы.

Официальные правила разрешают лицензированные сторонние компоненты и debug App Check tokens в submission, требуют реально вызываемый Firebase AI Logic, открытый репозиторий с OSS license и рабочую демонстрацию. Поэтому production issuance и конкурсный запуск — разные gates. Отдельно подготовить English README/testing instructions, описание изменений после начала конкурса и демонстрацию до трёх минут. [Правила](https://firebasehybridaihackathon.devpost.com/rules).

**G3:** зафиксировать назначение уведомлений, реальные ограничения OS/каналов и допустимость выбранных сервисов из C02. Нельзя закрыть этот пункт исключительно тестом доставки.

**G4:** составить component/license inventory для весов, export/train tooling, native FFmpeg build flags и изменений, ORT/MediaMTX, данных и отгружаемых assets. Для каждого элемента — источник, hash, условия использования/redistribution, требуемые notices/source, способ получения. Проверить совместимость предполагаемой лицензии собственного кода с фактической поставкой; если условия не подходят, выбрать совместимую лицензию/разрешение либо заменить компонент и повторить его qualification. Сам факт отдельного процесса/ONNX export или отдельной загрузки весов не доказывает отсутствие лицензионных обязанностей. [Ultralytics](https://www.ultralytics.com/license), [FFmpeg](https://ffmpeg.org/legal.html).

Обновить docs/operations.md: hardware profiles, supported routes, установка моделей, initial readiness, diagnostics, quotas, backup/restore, alert capabilities и действия при degraded coverage. Таблица «что работает» строится из exact candidate report. **Готово:** у каждого gate есть конкретное evidence и owner; unresolved external gate остаётся видимым.

## 9. Fast RC v2: замена обязательных 24/72 часов

### T01 — измерение и verdict должны быть отдельным контрактом

Сегодня `qualify.py` считает below_target, но лишь записывает JSON/CSV: ни одного failure exit из-за плохой cadence, короткой длительности, отсутствия coverage или роста памяти. [qualify.py:153–170](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L153-L170), [258–263](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L258-L263)

Оставить один pipeline: `qualify.py` собирает измерения, `validate_report.py` проверяет их по versioned `qualification-profile.json`, `fast_rc.py` запускает этапы и собирает единый итог. Не создавать параллельный набор правил успешности в каждом script. Выходы `PASS/FAIL/INCOMPLETE`, nonzero при двух последних, отдельные criteria с observed/required/evidence. У неполного запуска нет права стать зелёным за счёт исключения отсутствующих секунд.

В profile включить source SHA, binary/native/ORT/model/policy hashes, features, OS/driver/hardware/power mode, codec/dimensions/FPS, камеры/приоритеты/viewers, route, source trace/seed, RAM/VRAM/disk budgets, реальные sampling target/floor, fault schedule, time budget, необходимые case IDs. Старые отчёты reusable только для неизменившейся части с теми же входами и зависимостями; новый scheduler/profile требует собственного live-quality шага.

Основные условия:

- Per-camera actual cadence, max result age/gap и число окон без измерения. Конфигурация 5/2 fps и текущий floor4/1.5 fps — разные величины; нельзя переименовать floor в обещание 5/2. Epoch warmup/ожидаемый fault интервал показывается отдельно, но сохраняется в общем loss-of-coverage.
- Per-job timestamps/histograms; status snapshots `last_worker_ms` — распределение опросов, не независимых inference jobs. [qualify.py:193–203](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/qualify.py#L193-L203)
- В planned fault phase допускается именно ожидаемая техническая деградация с bounded recovery; в quiet load phase она является failure. Нулевая сумма degraded — бессмысленная цель, когда тест намеренно отключает камеру.
- Canary проверяет usable frame→local decision→durable incident+blob→outbox→test receiver. Semantic Urgent проверяется отдельно с fake deterministic provider и небольшой интеграционной проверкой реального выбранного provider; отсутствие urgent в offline replay не доказывает delivery.
- API target p95≤250 ms уже есть в плане; остальные новые recovery/resource limits должны быть explicit proposals, замороженными до запуска, а не подогнанными после результата.

### T02 — поправить evaluator, не зачисляя улучшения бухгалтерии в качество модели

`evaluate.py` правильно читает immutable opening `change_events`; проблемы «последний evidence переписал opening timestamp» в текущем коде нет. Реальные пробелы: знаменатель полной длительности отрицательных клипов независимо от coverage, достаточность всего одного frame для каждого model, отсутствие censoring и live burst в replay. [evaluate.py:73–104](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/evaluate.py#L73-L104), [134–172](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/tools/benchmark/evaluate.py#L134-L172), [vision.rs:334–339](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/vision.rs#L334-L339)

**Изменить:** отдельные nominal source hours, verified eligible coverage hours, unknown/lost seconds; bad coverage проваливает qualification, а не улучшает FAR путём исчезновения опасного интервала из отчёта. Onset interval, right censoring, видимый уже на старте сигнал, one-to-one episode/ROI matching, immutable first source decision и wall persistence/delivery latency. В сцене с несколькими объектами coincidence по category+window недостаточно.

**Regression fixtures:** clip39.33 s/onset34 s/no event должен быть censored для 8 s window; one result+99.99%unready не должен давать полный час проверенного отрицательного опыта; обновлённое evidence не меняет opening; wrong ROI/epoch не засчитывается; non-repeat Completed учитывается отдельно от сбоя; live-only burst проверяется paced RTSP.

Исправление censoring касается короткого KMU; восемь полных ранних окон MmodalFire остаются 0/8 и требуют исправления detector/data/policy, а не отчёта.

### T03 — четыре разных времени и минимальные швы для тестов

| Время | Для чего используется | Как проверить быстро | Что нельзя сделать |
|---|---|---|---|
| Source clock (`source_us`, epoch) | Семплирование, temporal policy, onset, discontinuity | Подать фиксированные frame/observation traces с теми же timestamps; явные gaps/backwards/new epochs | Ускорить source PTS и объявить, что реальный decoder успевает нужный FPS |
| Monotonic runtime clock | Freshness, backoff, deadline, heartbeat, lease duration в текущем процессе | Pure `step(now)` и fake monotonic; Tokio paused time для задач, использующих Tokio Instant | Считать, что pause меняет `std::time::Instant`/libav/ONNX/OS clocks |
| Persisted UTC | Expiry outbox/evidence/session, retention cutoff, daily budget | Передавать `now` в storage; pre-aged DB; boundary−1/exact/+1; forward/back step | Менять системные часы машины для обычного unit test или путать persisted UTC с duration |
| Physical wall time | Нативный throughput, CPU/GPU/IO, service manager, реальный timeout SQLite | Короткий настоящий production binary run с внешними измерениями | Выдать 3 виртуальных дня за 72 часа физического покрытия |

**Нужный небольшой refactor:**

1. Сохранить уже имеющийся `now: UnixMillis` в storage/domain; пользоваться им в тестах. `reconcile_evidence()` сейчас вызывает собственный `now_millis()` на 563/583 — передать recovery timestamp через entry point. `maintain_storage` извлечь в step.
2. Для supervisor/semantic/backoff/freshness вынести pure decision/state reducer с переданным `Now { monotonic, utc }`; asynchronous runner занимается I/O и sleeps. Где действительно нужна virtual Tokio time, использовать Tokio Instant последовательно и `#[tokio::test(start_paused = true)]` с `test-util`. Не нужен переписывание всего проекта всего проекта на универсальную абстрактную VM.
3. `health::WallClock::observe(monotonic, utc)` уже показывает удачный seam; тест двигает значения без реального ожидания. [health.rs:235–279](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/bins/stillwatch-core/src/health.rs#L235-L279)
4. `storage::DatabaseBudget` и Store writer используют std Instant, SQLite busy timeout3 s и обычный thread. Их real concurrency/locking проверять с настоящими ограниченными задержками; pure решение «пора retry checkpoint» можно отделить и передать now. Внешний SQLite connection, OS scheduling и native workers не попадут под Tokio pause.
5. Не добавлять runtime endpoint «поставить время» или production env-switch «сымитировать отказ». Test-only dependency injection/`cfg(test)` для библиотек; отдельный неотгружаемый test harness binary для внутренних crash cutpoints. Release artifact manifest/CI проверяет отсутствие fault feature. Внешние kill/network/quota сценарии запускать на неизменённом release binary, в одноразовой test data directory и с test alert receivers.

Tokio официально ограничивает pause current-thread runtime и только своим Instant; большой `advance` будит просроченные таймеры вместе и не ждёт, что все обработчики закончатся. Поэтому продвигать по контрольным состояниям с явными acknowledgements/yield/auto-advance, а не одним прыжком на 72 h. [Tokio pause](https://docs.rs/tokio/1.54.1/tokio/time/fn.pause.html), [advance](https://docs.rs/tokio/1.54.1/tokio/time/fn.advance.html)

### T04 — логические месяцы за минуты: набор свойств

Проверять реальные reducers/SQL, а не вторую игрушечную реализацию, которая повторяет те же ошибки. Предлагаемый базовый объём — фиксированные regression seeds плюс несколько сотен seeded traces, суммарно порядка 100000 transitions в выделенном bounded бюджете; сохранить seed и минимизированный failing trace. Это целевой объём тестов, не обещание 100%надёжности.

Последовательности: enable/disable/config revision/reconnect; source gap/backwards/freeze/new epoch; local concern→evidence→semantic→ACK/silence/resolve; duplicate commands; storage Busy/Full/Deadline; crashed claim; stale provider result; cleanup/shared blobs; route absent; clock jump; daily budget/lease/token expiry.

Инварианты:

- На камеру/актуальную ревизию не более одного capture owner; завершённый non-repeat replay не возрождается.
- Старый epoch/scene/evidence не меняет текущий incident; отсутствие кадров не считается «безопасно» и не очищает active hazard.
- Один logical operator command/attention cycle не создаёт два durable результата; notification receipt не является operator ACK.
- Процесс restart не возвращает просроченный alert к жизни и не продлевает evidence freshness.
- Durable incident/change/outbox согласованы; доступный blob существует, совпадает hash/length; активную общую ссылку retention не удаляет.
- Pending queues/frame permits/active jobs/child owners имеют конечный размер; после drain/recovery permit/owner count возвращается в допустимый диапазон.
- Lease fencing отклоняет поздний результат старого owner. Daily request budget и отмена consent проверяются на каждом допустимом переходе.

**Реальные возрастные границы в коде:** evidence semantic TTL15 s; outbox TTL1 h; command idempotency7 days; alert audit30 days; pairing5 min; access10 min; refresh14 days. Они доступны для прямых boundary tests без суток ожидания. [hazards.rs:21–23](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-engine/src/hazards.rs#L21-L23), [db.rs:28–29](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/db.rs#L28-L29), [alerts.rs:12–13](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/alerts.rs#L12-L13), [trust.rs:14–16](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/trust.rs#L14-L16)

В изученном Core нет отдельной реализации коммерческого licensing lease; не стоит придумывать новый компонент ради проверки. Если SDK/provider привносит такую expiry, она получает contract test на своей реальной границе, а не подменяет auth tokens или daily budget.

### T05 — частые реальные отказы и lifecycle churn

Предлагаемый 40-минутный этап на выбранном native профиле:200 completed RTSP reconnect cycles суммарно по 4 камерам (50 накамеру),20 inference child restarts,10 Core process restarts,100 viewer grant/offer/close cycles, затем capacity ramp и проверка admission control. Это разные counters, не «330 полных рестартов системы». Часть camera cycles можно параллелить, phase с глобальным Core restart — последовательная. В каждом случае дождаться нового usable scope/result либо корректного terminal outcome; простое отправленное 100 раз kill не считается 100 проверенными восстановлениями. Симуляционные тысячи transitions считаются отдельно. Завершённость всех циклов и capacity cases — обязательное условие: если насыщенность не укладывается в выделенное время данного стенда, итог INCOMPLETE при отсутствии уже установленного нарушения gate. Превышение заданного recovery/startup SLO — FAIL, даже если запуск впоследствии не завершён. Уменьшать counts или считать отправленные faults завершёнными нельзя.

Перед циклом и после контрольных batches писать child PID/start identity, file descriptors/Windows handles, threads, allocated frame/evidence permits, streams/sessions, RAM/VRAM; после drain сравнивать с моделью ожидаемых живых ресурсов. Повторить одинаковые группы 20/40/80 циклов, чтобы отличить конечное cache warming от роста пропорционально числу событий.

### T06 — матрица хранения и аварий

Сохранить existing quota/WAL tests и добавить работающие камеры и canary. Не заполнять системный диск; использовать маленький искусственный DB quota, test volume или ограниченный тестовый filesystem. Отличать `SQLITE_FULL` page limit, `ENOSPC` на blob, `SQLITE_BUSY` lock, pinned reader/WAL и hung writer: это разные состояния.

Crash cutpoints: до/после incident+outbox transaction; после JPEG temp write/fsync; после rename до DB commit; после commit до ответа caller; после marking retired до unlink; после unlink до cleanup-row commit; semantic claim/dispatched/response/applied; refresh rotation response; backup staging до publish. После каждого открыть настоящую DB заново: `integrity_check`, `foreign_key_check`, committed logical state, уникальность cycle, packet/hash/length, orphan/pending cleanup, generation/cursor consistency и новое событие после восстановления.

SQLite сама использует отдельные I/O/crash/failure harnesses; процесс kill проверяет crash recovery, но не имитирует все эффекты пропавшего питания и не доказывает hardware flush semantics. Для app-level protocol достаточно fault hooks на своих стадиях плюс реальный SQLite/native filesystem; специализированный VFS нужен лишь для оставшихся конкретных низкоуровневых рисков. [SQLite testing](https://www.sqlite.org/testing.html#ioerrtesting)

Pinned reader нельзя «ускорить виртуальными днями»: держать реальную read transaction, довести WAL до фактического порога, снять reader и проверить resumable writes. Текущий порог 16 MiB, retry2 s, busy timeout3 s явно заданы в коде. [budget.rs:11–15,76–124](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/crates/sw-storage/src/budget.rs#L11-L124). SQLite объясняет, почему долгий reader мешает checkpoint и почему одна крупная transaction способна временно раздуть WAL; это нужно учитывать в бюджете overshoot, а не требовать абсолютный 16 MiB в каждый момент. [SQLite WAL](https://www.sqlite.org/wal.html)

### T07 — ресурсный критерий без мифа «RSS обязан вернуться к нулю»

Профиль фиксирует RAM/VRAM/process/FD/disk budgets и резерв под худший допустимый burst. Warmup заканчивается по завершению загрузки моделей/кэшей, затем отдельно анализируются churn и тихая нагрузка. Native allocators и ONNX могут сохранять кэши; сравнивать с допустимым plateau и пределом, а не с 0RSS.

Три независимых условия:

1. **Bounded objects:** после drain нет лишних children, streams, grants, evidence/frame permits, незавершённых jobs; все ожидаемые retained записи объяснены политикой. Это сильнее общей RSS-картинки.
2. **Measured budget:** фактический peak всей process tree/VRAM/DB/WAL/blobs остаётся ниже profile cap минус резерв; отдельно учитывать burst/maintenance. Резерв вычислить из допустимых concurrent buffers/models и измеренного worst case;20% можно предложить начальным запасом, но не назвать универсальной гарантией.
3. **Growth evidence:** минутные агрегаты поздней части run и после одинаковых churn batches не показывают необъяснённого роста на цикл. Оценивать наклон и его неопределённость по временным блокам, а не по миллионам коррелированных instantaneous samples. Если верхняя разумная оценка slope × выбранный эксплуатационный горизонт съедает запас, профиль не проходит; если данных недостаточно различить plateau и опасный drift, INCOMPLETE/требуется targeted churn или allocator investigation. Такая экстраполяция — проверка запаса, а не доказательство фактических суток работы.

Для известного leak-per-reconnect ускорение числом циклов действительно полезнее ожидания. Для редкой driver race после многих часов, thermal throttling или внешней сетевой среды короткий run слабее; убрать обязательное ожидание допустимо, скрывать эту границу — нет. После поставки можно наблюдать те же counters, это не предварительное трёхдневное условие завершения.

### T08 — новый default: 180 минут на один нативный профиль

Это **плановый бюджет проверки уже собранного кандидата на подготовленном стенде**, который требуется откалибровать на реальной машине. Холодная сборка, downloads, подготовка данных и полный CI выполняются до начала 180 минут и в этот бюджет не входят. Первые 15 минут содержат проверку hashes/features, targeted baseline regressions, native preflight и warmup. Отсутствующий GPU/fixture или незавершённый этап без установленного нарушения дают INCOMPLETE; измеренное нарушение numeric/capacity/deadline gate — FAIL, даже если позже истёк общий бюджет. Ожидаемые fault timeouts оцениваются через ledger и recovery gates. Этот план не обещает уложить произвольную машину в 180 минут. Нельзя сократить нагрузку, число завершённых циклов или необходимую экспозицию, чтобы получить PASS к дедлайну.

| Минуты | Работа | Основной результат |
|---|---|---|
| 0–15 | Hashes/features уже собранного кандидата; release artifact без fault hooks; targeted baseline regressions, native preflight и warmup | Все входы зафиксированы, native startup работает |
| 15–40 | T02/T03/T04: evaluator regression, virtual timers, seeded state traces, pre-aged SQLite boundary scenarios | Логические сроки и transitions проверены без ожидания суток |
| 40–80 | T05:200RTSP reconnects +20worker +10Core +100viewer completed cycles; capacity ramp/admission; изоляция и очистка ресурсов | Подтверждённый progress после каждого сбоя, восстановление, отсутствие накопления, отказ при превышении ёмкости |
| 80–105 | T06: DB quota/WAL reader/blob IO/controlled crash cutpoints, активные камеры и canaries | Atomicity, pressure visibility, автоматический выход из отказа |
| 105–165 | 60 min тихой реальной непрерывной нагрузки на заявленном maximum supported profile; continuous live source и предусмотренный viewer workload | Фактические FPS/freshness/resources; без fault injection, конкурирующей сборки и иных тяжёлых тестов |
| 165–180 | Финальный end-to-end canary, bounded cleanup/restart/restore check, ресурсный diff и machine verdict | PASS/FAIL/INCOMPLETE с полными counters и manifest |

Нагрузка на 4/8/16 камер не бесплатна:180 min относится к одному выбранному поддерживаемому profile. Его fallback можно коротко проверить дополнительно внутри functional phase; рекламировать иной maximum без собственной проверки нельзя. Слабый компьютер может честно иметь предел 4 камеры, сильный —16, если каждый соответствует quality+cadence gates. Детальные изменения capacity, scheduler и ORT описаны в P01–P13.

Windows и Linux на двух независимых готовых native runners: приблизительно 180 min wall одновременно. Один dual-boot host последовательно: приблизительно 360 min плюс переключение ОС. Это остаётся без 24/72 h, но нельзя обещать 3 h для двух последовательных 3h работ. Для разработки допустим отдельный 120 min smoke preset, однако он **не выдаёт полный release PASS** и не заменяет 180 min квалификацию. Основной инженерный default —180 min.

Даже при достаточных runners нельзя запускать независимый CPU/GPU benchmark на той же машине в clean performance interval. Параллелизм по разным native hosts допустим; «мы всё параллелили» на одном слабом ноутбуке делает цифры непригодными для оценки мощности.

## 10. Как закончить модельную работу без бесконечного исследования

### 10.1. От разметки до независимого результата

1. **Сделать таблицу происхождения данных.** Единица split — исходная сцена/камера/съёмочная сессия, а для человека также субъект, если он повторяется. Похожие кадры из одного ролика не распределять между train и holdout. Сохранять hash видео, origin, license, scene/subject family, все производные crops. Perceptual dedup дополняет точные hashes и имена; он не доказывает независимость разных записей одной сцены.
2. **Разметить именно задачу.** Для fire/smoke — последний кадр без признака, первый устойчиво видимый признак, spatial ROI, класс и nuisance, степень видимости, controlled context, episode identity. Для fall/ADL — person identity, onset/course/recovery, intentional lowering и ambiguous sequence. Пограничные onsets просмотреть вторым проходом; фиксировать интервал неопределённости, а не притворяться точностью до миллисекунды по редким stills.
3. **Перевести уже изученные failures в regression/development.** MmodalFire, KMU и разобранные ADL нельзя после многократной настройки назвать нетронутым финальным holdout. Они сохраняют ценность для воспроизведения прошлых ошибок. Новые комнаты/источники/люди резервируются до окончательной фиксации модели и policy.
4. **Кэшировать независимые слои.** Один frozen selected-frame ledger и raw inference pass обслуживают сравнение policy; новый detector/preprocess требует нового inference pass. Для каждого варианта сохранять одинаковые onset labels и episode matching. Нужны и pre-threshold диагностические outputs, и фактические production observations.
5. **Провести ограниченные ablations.** Сначала deterministic fixes на неизменных весах, затем масштаб/ROI, затем данные и detector, затем temporal verifier. Для каждого шага сравнить своевременность, nuisance events, unknown, duplicates и цену на камеру. Улучшение по кадрам без улучшения эпизодов не является финальным результатом.
6. **Зафиксировать кандидат до holdout.** Model/profile/policy/thresholds/sampling и evaluation manifest становятся неизменяемыми. После открытия holdout сохраняется результат целиком. Если результаты этого набора, включая агрегированные метрики, использовались для настройки или выбора варианта, он становится development; для следующего независимого заключения нужен новый reserve, а прежний провал не удаляется.
7. **Сверить offline и live.** Ускоренный offline pass использует исходный source time и объявленную cadence; paced RTSP проверяет live burst, native transport и полный путь до тестового получателя. Качество при максимальной нагрузке подтверждается canaries и согласованным frame-selection ledger, чтобы busy scheduler не выбрасывал единственные ранние признаки.

Практическая схема поиска: существующая модель → проверяемый масштабный вариант → один дообученный detector с правильной ontology → один temporal verifier. Простое решение остаётся предпочтительным, если усложнение не улучшает одновременно нужную своевременность, nuisance burden и вычислительный бюджет.

### 10.2. Что именно считать пройденным quality gate

В исходном техническом плане численные границы общего recall/FAR сознательно не зафиксированы до анализа baseline. Это **открытая продуктовая часть**, а не право объявить любой новый результат успешным. [Исходный принцип приёмки](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/StillWatch_Next_Rust_Technical_Implementation_Plan_RU.md#L1180-L1190).

Шаг 1 реализации обязан создать `quality-contract.json` с численными критериями до нового обучения и до открытия holdout. Обязательные поля и рекомендуемый способ заполнения:

| Поле | Правило |
|---|---|
| Early fire/smoke timeliness | Сохранить известное окно до 8 секунд после visual onset; допуск до onset и интервальная разметка указываются отдельно. Поздний hit не превращается в timely |
| Early Review recall | Рекомендуемая исходная цель — наблюдаемый Recall@8 s ≥95% на валидных независимых эпизодах, с отдельными результатами small smoke, low light и остальных групп. Это предлагаемая продуктовая цель, не отраслевой норматив и не доказанная точность |
| Критические regression cases | Все обязательные deterministic сценарии проходят; исправляемые Mmodal misses и mist cases входят в явный список. Нет скрытого усреднения проваленного критического случая с тысячей спокойных кадров |
| False Review budget | Задать допустимое число ложных Review на весь объект в сутки и вывести camera-hour threshold. Например, бюджет 8 Review/сутки для 16 камер соответствует 8/(16×24)=0.02083 на camera-hour. Это пример выбора масштаба нагрузки на оператора, не готовое требование пользователя |
| False Urgent budget | Отдельное численное ограничение, отдельные positives/negatives и интервалы неопределённости для достижимой local/semantic Urgent policy. Нельзя наследовать Review threshold или принять ноль от выключенной ветки |
| Fall/ADL | Сохранить чувствительность на прежних положительных regression cases; независимый event recall, ADL false Review/час и duplicates проверяются раздельно. ADL improvement не покупается пропуском валидированных падений |
| Coverage | Номинальное время, пригодное время и unknown показываются одновременно. Пустые/неполные окна не считаются правильными отрицательными ответами; недостаточное покрытие даёт отдельный failure |
| Identity / duplicates | Один физический эпизод сопоставляется одному incident согласно frozen merge/split policy; count duplicates и wrong-person/ROI matches обязательны |
| Статистические claims | Помимо point estimate всегда сохранять числитель, знаменатель, число независимых scenes/sessions и interval. Заявление о population FAR/recall допускается только в пределах соответствующих данных и допущений |

В числовых полях финального профиля не допускаются `null`, «по ощущениям» или «стало лучше». Если независимый набор не позволяет подтвердить требуемую частоту редкого события, статус этого claim — INCOMPLETE. Можно закончить более узкий явно определённый профиль, но исходный полный объём с обязательным Urgent/16-camera capability от этого автоматически не закрывается.

Для ориентира: даже 60 успехов из 60 независимых одинаково распределённых эпизодов дают одностороннюю 95% нижнюю биномиальную границу около 95.1%, а не 100%. Это вычисляется как `0.05^(1/60)`. Корреляция одной комнаты и постановочный характер клипов дополнительно ограничивают перенос результата. Число 60 здесь объясняет неопределённость, а не задаёт универсально достаточный размер датасета.

### 10.3. Когда остановить неудачную ветку

Если raw diagnostics показывают слабый правильный ROI, а native-scale input улучшает timely recall в пределах capacity, принимать F02. Если даже качественное изображение не даёт полезного разделения, следующий шаг — ontology/данные/веса; дополнительный таймер подтверждения отсутствующий сигнал не создаст. Если nuisance и smoke не разделяются доступным RGB-сигналом, сохранять явную неопределённость и ограничение профиля; не формировать confident safe из отсутствия отличительного признака.

Temporal verifier сначала сравнивается с простым baseline на том же crop sequence. Только улучшивший held-out development trade-off вариант получает право на усложнение runtime. Замена модели, export contract, обучение, threshold tuning и compute qualification — одна связанная работа: ускоренная лёгкая модель не принимается исключительно по FPS.

### 10.4. Конечный бюджет первой серии экспериментов

Это стартовый engineering budget, не обещание достаточного объёма научной квалификации и не повод бесконечно ждать обучения.

- Сначала только детерминированные исправления F03/F07/F12/F15 и новый диагностический ledger; веса фиксированы. Один frozen frame/observation pass даёт baseline и stage attribution, несколько policy variants используют тот же материал без повторного decode/inference.
- Для small-fire screening: текущий 640 и максимум два дополнительных входных варианта — один high-resolution и один bounded ROI. На этом этапе не запускать сетку десятков shapes/thresholds. Оставить вариант, который улучшает episode-level результат при том же nuisance/capacity budget; если ни один не улучшает, закрыть scale-ветку и перейти к данным/весам.
- Для обучения первого цикла: максимум два detector candidates (например, fine-tune существующего и одна обоснованная альтернатива); каждый с заранее записанным ceiling30 epochs и early stop после 5 epochs без улучшения development objective. Это конфигурация начального эксперимента, не финальный рецепт обучения. Победителю разрешён один повтор с другим seed, только если он улучшил нужный trade-off. Отдельный temporal verifier — один baseline, а не параллельная коллекция архитектур.
- После двух последовательных ablations без улучшения Pareto frontier timely recall / nuisance events / compute cost остановить эту гипотезу и разобрать errors. Не ослаблять hidden acceptance threshold и не продлевать runs автоматически. Простой baseline сохраняется, если усложнение не окупилось.
- Final holdout открывается после фиксации model/profile/policy/thresholds. Все специально добавленные критические regression traces должны проходить; для нового positive video соблюдается заранее выбранное окно 8 s там, где onset и follow-up позволяют его оценить. Число/разнообразие независимых scenes, интервалы неопределённости и rare-event claims публикуются отдельно: повторное ускоренное проигрывание не создаёт новую статистическую информацию.
- Routing acceptance разделить: local Review без semantic service; local Urgent только при включённой и квалифицированной UrgencyPolicy; semantic-assisted Urgent отдельно; delivery path отдельно. Успешный synthetic Urgent delivery не засчитывается как ML success, а zero Urgent без достижимой ветки не засчитывается как specificity.

### 10.5. Какие часы и какое качество действительно измерены

Три вида данных нельзя складывать:

- **State transitions/virtual age:** доказывают обработку сроков/переходов; не дают camera-hours среды или реального uptime.
- **Исходные независимые записи:** могут дать много camera-hours наблюдаемой среды, даже если обработаны быстрее реального времени, когда temporal sampling/source timestamps сохранены и покрытие полное. Это доказательство качества detector/policy на этих данных, а не живого decoder/scheduler throughput. Один и тот же час видео, повторённый 100 раз, не создаёт 100 независимых часов разнообразия.
- **Реальный native load:** даёт actual run exposure и timing/ресурсы конкретного профиля; один synthetic RTSP источник на 16 камер не является 16 независимыми помещениями для FAR.

Для zero false events односторонняя 95%Poisson верхняя граница около`3/T` при соответствующих допущениях. Например, чтобы содержательно обсуждать предложенную цель 0.01 события/камера-час, требуется около 300 валидных representative source hours без событий, а не 3 часа elapsed любой нагрузки. Это расчёт планирования, не текущая гарантия. Коррелированные комнаты/клипы требуют группировки и осторожной интерпретации; positives считать по независимым эпизодам/сценам, приводить numerators и uncertainty.

Один раз подготовленный корпус с разметкой позволяет быстро выполнять регрессию каждого RC без ожидания новых суток. Если его результаты использовались для настройки или выбора варианта, дальнейшие прогоны этого корпуса не считаются новым независимым holdout; для такого заключения применяется резерв согласно §10.1. Если исходных роликов/разметки нет, нельзя дописать к инженерному PASS «доказан низкий FAR». Для изменённой модели или порогов пересчитать её quality result; для unchanged model+preprocess допустимо reuse подписанного detector baseline, но изменения policy/sampling требуют соответствующего replay/live-load regression. Цель завершения — конкретные работающие поддерживаемые профили и воспроизводимые результаты;100% безошибочность по конечному тесту недоказуема.

## 11. Последовательность реализации до финального кандидата

Карточки выше — спецификация работ; таблица ниже задаёт порядок. Не требуется сначала завершать весь ML research: детерминированные дефекты и измерительный контур можно исправлять сразу. Идентификаторы P01–P13 в этом документе относятся к новому плану производительности; они не переобозначают исторические этапы P00–P17 из IMPLEMENTATION_STATUS.

| Шаг | Работа и карточки | Конкретный результат | Условие перехода дальше |
|---:|---|---|---|
| 1 | Заморозить контракт, источники и способ оценки — T01/T02, F01/F17, C08 | qualification-profile schema, версия Fast RC v2, manifest данных с lineage/split/onset, список 17 acceptance rows и G1–G5 | Приёмочные SLO, статистическая методика и обязательные case IDs зафиксированы до новых сравнений; старые ошибки воспроизводимы. Пороги решений модели подбираются только на development/calibration и замораживаются до открытия final holdout |
| 2 | Добавить наблюдаемость без изменения решений — P01, диагностическая часть P04, F01 | Stage timing, wall/source cadence, accepted/rejected/missed counters; raw diagnostic trace до confidence filter | Половинная скорость источника и задержанный worker видны; baseline trace воспроизводим; logging bounded |
| 3 | Закрыть freshness и durable jobs — R03/R04, F07 | Неизменный возраст evidence, leased semantic jobs, bounded evidence updates | Delayed persist не делает старый кадр свежим; crash/retry не теряет packet и не применяет старый ответ |
| 4 | Исправить supervision и storage recovery — R01/R02/R05/R06 | Camera task outcomes и restart policy, supervision критических задач, bounded maintenance, независимые coverage snapshots | Camera/vision/storage faults восстанавливают полезную обработку; completed replay не запускается бесконечно |
| 5 | Исправить incident lifecycle — F06/F14/F15 | Разделение track/episode/incident, durable recovery acknowledgement, явный benign-course reducer | Один физический эпизод не размножается от gap; неуспешный recovery можно безопасно повторить; Urgent/ACK не очищаются ошибочно |
| 6 | Исправить deterministic perception/policy — F03/F05/F09/F12/F13/F18 | Local visibility, непрерывность слабых candidates, cadence-aware motion, novelty/freshness separation, temporal evidence | Целевые causal traces проходят на 3/4/5 fps и при gaps; blind periods не считаются recovery; masks сохраняются |
| 7 | Исправить расписание и ограничение нагрузки — P02/P03/P04/P06/P07 | Phase-preserving task clocks, независимый overdue Fire result, capture lag policy, atomic compute/view admission | На 15 fps нет систематического округления Fire к 1.875; overload управляем и видим; последний compute slot не выдаётся двум камерам |
| 8 | Ввести model-profile contract и измеренно оптимизировать runtime — F10, P05/P08/P09/P10/P11/P12 | Profile roles/shapes/provenance, численные gates, warmup; только подтверждённые A/B оптимизации | Все активные роли проходят tensor/observation/event checks; каждая оптимизация сохраняет quality; hardware capacity измерена |
| 9 | Завершить модельную часть — F02/F04/F08/F11/F16, F18 | Выбранный detector/temporal verifier, controlled-context policy, calibrated local urgency | Development Pareto trade-off улучшен; independent holdout проходит замороженный quality contract; модельный профиль снова проходит шаг 8 |
| 10 | Закрыть delivery/auth/SDK — R07/R08, C01/C05/C06 | Полный alert feed, recoverable refresh rotation, notification lifecycle, обновлённые общие contracts | 129+ alerts доступны; потерянные ответы не теряют устройство/действие; Home/network loss не стирают активное уведомление |
| 11 | Закрыть реальные платформенные маршруты — C02/C03/C04 | Android background adapter, APNs adapter/relay, подписанные device builds, фактический Firebase route и App Check | Новое событие после Home видно на реальном устройстве в поддержанном состоянии; on-device после restart действительно on-device |
| 12 | Автоматизировать короткую квалификацию — T03–T08, P13 | Один fast_rc orchestrator, validator, pre-aged fixtures, seeded state traces, fault ledger | Неполный/проваленный отчёт не становится PASS; выполненный fault подтверждается полезным progress и canary |
| 13 | Подготовить frozen release artifacts — C07/C08 | Подписанные пакеты, hashes/SBOM/notices, миграции и rollback; инструкции на английском | Установленная поставка соответствует manifest; migrations/jobs/refresh совместимы; отсутствуют test fault hooks и скрытые Python runtime зависимости |
| 14 | Провести Fast RC v2 на заявленных профилях и короткую физическую матрицу — T08/C03/P13 | Отдельный 180-minute report каждого native profile и device-run sheets | Quiet load, capacity, failures/recovery, delivery и integrity проходят на точных артефактах |
| 15 | Сверить весь выпуск с §23.1 — все карточки, G1–G5 | Заполненная acceptance matrix и release decision | Нет FAIL/INCOMPLETE/необъяснённых обязательных open rows; исправления после freeze перепроверены по зависимостям |

### Где работать параллельно

После шага 1 можно независимо вести три направления: storage/supervision/contracts; diagnostics/scheduler/runtime; data annotation/model development. Клиентский foreground lifecycle C01 также исправляется отдельно. Общие структуры evidence, source scope, model profile и alert identity сначала согласуются в шагах 1–3: иначе параллельные изменения будут несовместимыми.

Для одного разработчика разумный старт — шаги 1–7 и C01: они дают проверяемые улучшения без ожидания обучения или доступа к телефону. При этом внешний доступ к signing, аппаратуре, независимым данным и условиям публикации лучше готовить с первого шага, чтобы он не остановил готовый код в конце.

Большие оптимизации выполняются только при доказанном узком месте. Shared memory, новая GPU batching схема, CUDA graphs, переделка tracker и полная замена storage не являются обязательными работами ради самого факта модернизации. Обязателен измеренный контракт; конкретный механизм выбирается по P01.

### Как закрывать отдельный фикс

Каждое изменение включает исходный failing scenario, минимальный regression, implementation, результат повторной проверки и перечень затронутых профилей. Один commit/PR отвечает за один связный механизм. Например, не смешивать исправление источника timestamp с переобучением detector: иначе исчезает возможность понять, что изменило delay.

После изменения scheduler, моделей, resize/NMS, context policy, evidence или native library недостаточно прежнего отчёта по тем же видео. Повторяются зависимые numeric/event/capacity gates. Для изменения текста документации такой пересчёт не нужен. Правила повторов хранятся в dependency manifest; запускать заново весь многосуточный soak после каждого commit больше не требуется.

## 12. Матрица полного завершения по 17 требованиям §23.1

Это дополнение к существующей [acceptance.md](https://github.com/mikhailbovt/StillWatch-Next/blob/79da236df1b17a5e21306b8750a7bbacb47fdad0/docs/acceptance.md), а не удаление прежних доказательств. Проверенные результаты сохраняются с их исходными hashes и ограничениями. Если новый фикс затрагивает их зависимость, новая строка получает новый результат.

| Требование | Чем закрывается в этом плане | Финальное доказательство |
|---|---|---|
| 1. Автономность | R01–R06, F16, T03–T08, C07 | Установленный Core без UI/Python/Cargo/Firebase восстанавливает live analysis после reboot и fault; достижимый локальный уровень решений указан |
| 2. Rust migration | F10/F17, P09/P10, C07 | Происхождение baseline и моделей, A/B tensor/observation, intentional policy differences, event holdout каждого профиля |
| 3. Разделение Core/Companion | C07 | Inventory именно подписанных Companion; модели/ORT/engine отсутствуют, Core работает при закрытом UI |
| 4. Камеры | R01, F09, P04/P05/P07/P08, C03 | CRUD/reconnect/concurrency/epochs, physical UVC/ONVIF, geometry changes, atomic admission; число configured и active камер различается |
| 5. Vision | F01–F18, T02 | Early fire/smoke, mist/steam, controlled flame, falls/ADL, identity, visibility и recovery проходят объявленную оценку |
| 6. Firebase | R04, C04/C08 | Настоящие local/cloud SDK responses после restart/fault, consent, App Check, pinned model/SDK; статусы G1/G2 с evidence |
| 7. Safety semantics | R03/R04, F06/F07/F14–F16, C01/C05 | Unknown не становится safe; ACK не resolve; old evidence не эскалирует актуальное событие; urgency/re-alert имеют независимые quality gates |
| 8. Durable state | R03–R08, T04/T06 | SQL/blob/outbox/semantic job crash cutpoints и реальные файлы; consistency, idempotency, TTL и bounded maintenance |
| 9. Доступ | R08, C03/C06 | Pairing/TLS/roles/revoke/scopes/SSRF/upload сохраняются; проверены новые endpoints, refresh lost response и внешний DNS/LAN deployment |
| 10. Live | F09, P04/P06/P07, C03/C06 | Кодеки, masks, detail sharing, multi-viewer, revoke, stale indication; физический external NAT/TURN и inference cadence одновременно |
| 11. SDK | R07/R08, P01/P07, C05 | Один contract suite для Rust async/optional blocking, Python sync/async, Dart и TS; reconnect/idempotency; dependency-isolation без engine/ML; N/N−1 для реально поддерживаемой пары версий |
| 12. Приложения | C01–C03/C05/C07 | Устанавливаемые Windows/Linux/Android/iOS builds; рабочие камера/scene/history/evidence/actions/access flows на заявленных устройствах |
| 13. Алерты | R07/R08, F16, C01–C03/C08 | Новое событие → durable intent → transport → наблюдаемое native presentation; receipt/ACK разделены, фон и ограничения ОС записаны, G3 закрыт |
| 14. Оптимизация | P01–P13, F02/F10/F17, T01/T07/T08 | Цель 16 камер выполнена на конкретном профиле; steady cadence/freshness/API/resources; сохранение качества и отдельный CPU fallback |
| 15. Recovery | R01–R08, F15, T05/T06, C07 | Backup/restore/migration/upgrade/rollback, instance/generation, отсутствие повторной отправки истории; bounded recovery после fault |
| 16. Сборки | P09–P11, C07/C08 | Native Windows/DEB/RPM/OCI и signed mobile; точный состав native deps/weights/SBOM/licenses, installability iOS, G4/G5 |
| 17. Операции | R02, P01/P07, C03/C08 | English install/ops/testing docs; doctor/support bundle без секретов; hardware/capabilities/quotas/health и диагностика выбранных remote paths |

### Внешние gates остаются явными

| Gate | Конкретное закрытие | Что не является закрытием |
|---|---|---|
| G1 — конкурсная архитектура | Фактическая Web Hybrid демонстрация, соответствующая опубликованным правилам; требуемая трактовка спорной схемы зафиксирована; готовые public submission assets | Само наличие Firebase SDK в dependencies или обещание, что организатор согласится |
| G2 — Firebase runtime | On-device результат после restart; cloud fallback с consent; потеря браузера/маршрута не ломает Core; credentials обновляются | Handshake, результат in_cloud вместо заявленного on_device или development credential как production deployment |
| G3 — сервисы доставки | Выбран фактический допустимый transport/deployment с проверенными OS capabilities и условиями | Смена названия high-risk сценария или один успешный push |
| G4 — права и поставка | Inventory и совместимая лицензия/права/redistribution для каждого поставляемого элемента и данных | Утверждение «всё стало Rust/ONNX, значит старые условия исчезли» |
| G5 — native и качество | Numeric/observation gates всех активных ролей, independent event qualification, capacity и native packaging точного профиля | CPU успех вместо CUDA результата, успешный import модели или parity без приемлемого detector quality |

Доступ к физическим телефонам/камерам, signing identities и допустимым данным — входы плана с явным владельцем. Их отсутствие не исправляется виртуальным временем. Эти действия конечны и не требуют 24/72 часов ожидания. Пока обязательный вход не получен, соответствующая строка BLOCKED/INCOMPLETE; весь исходный объём не объявляется завершённым.

## 13. Артефакты и машинный критерий выпуска

### 13.1. Предлагаемая структура

Новые имена ниже — часть плана реализации; этих команд и каталогов нельзя считать уже созданным функционалом репозитория.

| Артефакт | Назначение |
|---|---|
| `qualification/profiles/<profile>.json` | Замороженные workload, roles/shapes, SLO, budgets, required cases и allowed faults |
| `qualification/schemas/report-v2.schema.json` | Общая схема результатов и правил статуса; без самостоятельных разных verdict у каждого collector |
| `qualification/runs/<run_id>/manifest.json` | Commit/binary/native/model/policy/data/harness hashes, OS/hardware/driver/power, профиль и версии |
| `qualification/runs/<run_id>/faults.jsonl` | Плановый отказ, observed onset/end, recovery evidence и canary outcome |
| `qualification/runs/<run_id>/metrics.*` | Ограниченные сырые job/camera measurements, cadence windows, latency histograms, resources |
| `qualification/runs/<run_id>/report.json` | Required/observed/result/evidence для каждого критерия и итог валидатора |
| `models/evaluations/<candidate>/` | Dataset/split provenance, detector/policy ablations, независимый holdout, uncertainty и error gallery |
| `docs/acceptance.md` | Сопоставление 17 требований и G1–G5 с точными доказательствами и остаточными ограничениями |

Большие видео, credentials и приватные кадры не обязаны попадать в Git: сохранять контролируемые ссылки, hashes, права доступа и способ воспроизведения. Публичный отчёт содержит агрегаты и разрешённые примеры. Это сохраняет воспроизводимость без случайной публикации домашней записи.

### 13.2. Пример формы результата

Это пример схемы, **не фактический успешный результат**. Поля `null` обозначают отсутствующее измерение.

```json
{
  "protocol": "fast-rc-v2",
  "run_id": "example-not-executed",
  "candidate_digest": "sha256:TO_BE_COMPUTED",
  "profile_digest": "sha256:TO_BE_COMPUTED",
  "native_wall_seconds": null,
  "source_video_seconds": null,
  "virtual_clock_span_seconds": null,
  "verdict": "INCOMPLETE",
  "criteria": [
    {
      "id": "cadence.pose.per-camera.60s",
      "required": {"accepted_min": 285, "window_seconds": 60},
      "observed": null,
      "result": "NOT_RUN",
      "evidence": []
    },
    {
      "id": "source.reconnect.completed",
      "required": {"count": 200, "usable_progress": true},
      "observed": null,
      "result": "NOT_RUN",
      "evidence": []
    }
  ],
  "coverage": {"required_cases": null, "completed_cases": 0},
  "limitations": []
}
```

Collector exit0 означает, что измерения записаны. Только валидатор решает, выполнен ли профиль. Настоящее нарушение имеет приоритет FAIL над последующей неполнотой; недостающие данные дополнительно перечисляются. Если нарушения ещё не установлены, но нет обязательного случая/часа чистой нагрузки/камеры, результат INCOMPLETE. PASS возможен только при полном покрытии и выполненных порогах.

### 13.3. Предлагаемый интерфейс запуска

После реализации harness интерфейс можно сделать таким:

```bash
python tools/qualification/fast_rc.py --profile qualification/profiles/windows-cpu4.json --candidate release/manifest.json --output qualification/runs/win-cpu4-001
python tools/qualification/validate_report.py --profile qualification/profiles/windows-cpu4.json --run qualification/runs/win-cpu4-001
```

`fast_rc.py` внутри вызывает существующий расширенный `tools/benchmark/qualify.py` и затем тот же `validate_report.py`. Для 16 камер выбирается отдельный квалифицируемый CPU/GPU profile manifest; название файла само по себе не обещает производительности. Test tooling может использовать Python на стенде; проверяемая устанавливаемая поставка Core по-прежнему должна работать без него. Сценарий offline install проверяет это непосредственно.

## 14. Финальный checklist владельца выпуска

1. **Контракт зафиксирован:** 17 строк §23.1, G1–G5, поддерживаемые ОС/устройства/каналы, model roles/shapes, 16-camera target, thresholds и budgets имеют однозначное значение.
2. **Все обнаруженные детерминированные дефекты закрыты:** для каждого R/F/P/C fix есть regression, результат и commit; conditional оптимизация может быть закрыта как не требующаяся только по измерению её причины.
3. **Early detection исправлено по существу:** своевременность новых независимых эпизодов проверена; прежние Mmodal misses не исчезли за счёт нового знаменателя; mist/ADL burden и duplicate incidents учитываются.
4. **Локальная автономная срочность квалифицирована**, если она входит в профиль; успешная synthetic доставка и ноль Urgent у недостижимой ветки не подменяют её качество.
5. **Source freshness и useful progress наблюдаемы:** состояние API ready не выдаётся за работающий анализ; недоступность не скрывает сохранённый incident.
6. **Каждый runtime profile имеет полный Fast RC v2:** exact hashes, 60 минут quiet нагрузки, required fault counts, bounded resources, usable recovery и native canaries. 120-minute smoke не принят за release qualification.
7. **Mobile/desktop доставка проверена на устанавливаемых артефактах:** background entry, permission/revoke/offline lifecycle, реальные звук/показ и различие receipt/ACK.
8. **Поставка восстанавливается:** install/reboot/upgrade/restore/rollback, новые migrations и model activation проверены; backup соответствует схеме и не создаёт live alerts из истории.
9. **Внешние gates и документация закрыты:** signing/installability, данные/лицензии, действительные Firebase routes, условия транспорта, English instructions и redacted diagnostics.
10. **Отчёт соответствует отгружаемой версии:** после финального исправления зависимые gates пересчитаны; нет зелёного отчёта от старого бинарника и нерешённых обязательных open rows.

После выполнения этого списка разработку исходного объёма можно завершать. Дальнейшая эксплуатационная телеметрия, новые камеры и редкие полевые случаи дают материал для следующих версий. Обязательный многосуточный прогон не возвращается автоматически: обнаруженный риск получает конкретный reproducer и проверку соответствующего механизма.

## 15. Первичные источники и практический смысл исследования

Источники ниже объясняют выбор подходов, но не доказывают качество StillWatch на его данных. Все пороги, расписание Fast RC и последовательность изменений в этом документе — предлагаемые инженерные решения.

| Источник | Что из него используется |
|---|---|
| [Tokio pause](https://docs.rs/tokio/latest/tokio/time/fn.pause.html), [advance](https://docs.rs/tokio/latest/tokio/time/fn.advance.html) | Граница virtual time; реальные native/OS clocks остаются отдельной проверкой |
| [SQLite testing](https://www.sqlite.org/testing.html), [WAL](https://www.sqlite.org/wal.html) | Адресная проверка crash/IO/locking/checkpoint; process kill и аппаратная потеря питания различаются |
| [ORT threading](https://onnxruntime.ai/docs/performance/tune-performance/threading.html), [CUDA EP](https://onnxruntime.ai/docs/execution-providers/CUDA-ExecutionProvider.html), [I/O binding](https://onnxruntime.ai/docs/performance/tune-performance/iobinding.html) | Tuning по профилю, цена copy/synchronization, условия GPU paths; ни одна настройка не обещает заранее ускорение |
| [SAHI, оригинальная работа](https://arxiv.org/abs/2202.06934) | Sliced inference — проверяемая гипотеза для маленьких объектов с дополнительной вычислительной ценой |
| [STV-FSANet](https://www.mdpi.com/1424-8220/26/15/4970) | Temporal/spatial verification как направление сравнения; upstream candidates и пересечение датасетов требуют отдельного аудита |
| [FASDD, авторский репозиторий](https://github.com/openrsgis/FASDD) | Возможный источник дополнительных изображений после проверки ontology/лицензии/lineage; не замена независимого early-onset video holdout |
| [Firebase Hybrid Web](https://firebase.google.com/docs/ai-logic/hybrid/web/get-started) | Фактический browser route и его runtime prerequisites; наличие Node не доказывает on-device inference |
| [Android foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types) | Правильный background adapter зависит от реальной функции, permissions и условий распространения |
| [APNs requests](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns), [background updates](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app) | Видимая доставка и silent background execution — разные механизмы; delivery acknowledgement не равен наблюдаемому показу |
| [FCM life-critical cases](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-priority#life_critical_use_cases) | Выбор допустимого transport является отдельным gate G3 |
| [Официальные правила конкурса](https://firebasehybridaihackathon.devpost.com/rules) | Рабочий фактический Hybrid route и submission evidence; не автоматическое одобрение конкретной архитектуры |
| [Ultralytics licensing](https://www.ultralytics.com/license), [FFmpeg legal](https://ffmpeg.org/legal.html) | Проверка конкретного состава поставки и прав; процессная изоляция не решает вопрос автоматически |

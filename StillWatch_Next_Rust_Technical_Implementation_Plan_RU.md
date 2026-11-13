# StillWatch Next — Rust Core: техническая архитектура и план реализации

**Рекомендуемое имя новой репы:** `stillwatch-next`. Продукт остаётся **StillWatch**; Firebase — конкретный вычислительный провайдер, а не часть имени продукта.

**Редакция:** 2.0 — Rust-first, заменяет Python-Core архитектуру редакции 1.0.  
**Дата Rust-ревизии и проверки новых источников:** 13 ноября 2026.  
**Исходная репа:** `mikhailbovt/stillwatch`.  
**Зафиксированный baseline:** `b0d20a20d38a5bbc22178d150999e57cc9e7bfb3`, версия проекта `0.5.0`.  
**Область документа:** код, протоколы, модели, инфраструктура, безопасность, тестирование, сборка устанавливаемых артефактов. Публикация в магазинах, создание конкурсной заявки, продвижение и отправка видео не входят.

> Это план полной технической реализации, а не утверждение, что реализация уже выполнена или конкурсная допустимость гарантирована. Здесь отдельно отмечены подтверждённые свойства старого кода, проектные решения, результаты локальных экспериментов и внешние условия, требующие проверки.

## Что изменено в Rust-редакции

**Собственный движок переписываем на Rust, а не заворачиваем Python в Rust-executable.** На Rust реализуются Core, lifecycle/supervisor, camera registry, inference orchestration, tracking и fire/fall policies, evidence, storage/outbox, API, CLI, desktop AlarmAgent и основной optional gateway. Рабочая установка Core не требует Python, pip, PyTorch или Ultralytics runtime.

Локальные **веса и задачи существующих YOLO сохраняем**, но экспортируем их в проверенные ONNX-артефакты и выполняем через `ort`/ONNX Runtime. Python остаётся только в изолированных инструментах экспорта, legacy differential tests и независимом HTTP SDK для Python-пользователей. Это не production dependency Core.

**Граница переписи:** лёгкий интерфейс компаньонов остаётся Flutter/Dart, системные интеграции — native bridges; модели в компаньоны по-прежнему не попадают. Firebase AI Logic остаётся за небольшим официальным JS SDK-мостом, поскольку Rust отсутствует в перечне официальных клиентских SDK. Это намеренные границы, а не обещание, что абсолютно каждый файл, UI и сторонняя библиотека будут Rust. [R30, R37]

Пересмотрены не только названия технологий: Cargo workspace, concurrency/ownership, native-library packaging, перенос моделей и golden fixtures, Rust SDK, plugin ABI, CI и **все 18 этапов P00–P17**. Старые findings и результаты аудита сохранены как исторический Python baseline, **не как измерения нового Rust Core**. Переход на Rust не закрывает конкурсные и сервисные gates автоматически.

## Содержание

1. [Обязательные решения и блокирующие условия](#1-обязательные-решения-и-блокирующие-условия)
2. [Что изучено в старой репе](#2-что-изучено-в-старой-репе)
3. [Матрица конкурсных требований](#3-матрица-конкурсных-требований)
4. [Архитектура и выбор стека](#4-архитектура-и-выбор-стека)
5. [Структура новой репы](#5-структура-новой-репы)
6. [Core: процессы и жизненный цикл](#6-core-процессы-и-жизненный-цикл)
7. [Источники и многокамерность](#7-источники-и-многокамерность)
8. [Конвейер видео и временная модель](#8-конвейер-видео-и-временная-модель)
9. [Огонь: контекст, риск и ложные тревоги](#9-огонь-контекст-риск-и-ложные-тревоги)
10. [Падения и другие видимые опасности](#10-падения-и-другие-видимые-опасности)
11. [Firebase-провайдер и настоящая гибридность](#11-firebase-провайдер-и-настоящая-гибридность)
12. [Подключение, pairing и удалённый доступ](#12-подключение-pairing-и-удалённый-доступ)
13. [Трансляции](#13-трансляции)
14. [API и SDK](#14-api-и-sdk)
15. [Компаньоны](#15-компаньоны)
16. [Алерты, сирены и доставка](#16-алерты-сирены-и-доставка)
17. [Хранение, приватность и восстановление](#17-хранение-приватность-и-восстановление)
18. [Оптимизация и фактически выполненные эксперименты](#18-оптимизация-и-фактически-выполненные-эксперименты)
19. [Проверка качества моделей и системные тесты](#19-проверка-качества-моделей-и-системные-тесты)
20. [Поэтапная реализация](#20-поэтапная-реализация)
21. [Сборки и установочные артефакты](#21-сборки-и-установочные-артефакты)
22. [Миграция старого проекта на Rust](#22-миграция-старого-проекта-на-rust)
23. [Условия технической готовности](#23-условия-технической-готовности)
24. [Источники](#24-источники)

---

## 1. Обязательные решения и блокирующие условия

### 1.1. Неподвижные требования продукта

- **Core автономен.** Модели, камеры, расписание анализа, состояние инцидентов, хранение и политика оповещения работают на хосте, а не в интерфейсе.
- **В компаньонах нет inference.** Ни YOLO, ни Gemini Nano, ни локальной языковой модели. Только управление, отображение, аппаратное декодирование трансляции и системные уведомления.
- **Windows/Linux — desktop; iOS/Android — mobile.** macOS-компаньон не входит в обязательный объём. macOS-машина или runner для сборки iOS — отдельный вопрос.
- **Существующие YOLO сохраняются по весам/задачам, а не по Python-runtime.** Их verified ONNX-export исполняется Rust inference worker через ONNX Runtime; temporal policy портируется на Rust с parity tests. Полезные ограничения не заменяются большим новым prompt.
- **Production Core — Rust без встроенного Python.** Экспорт/legacy oracle изолированы от runtime и не поставляются как обязательная часть пользовательского Core.
- **Несколько камер работают одновременно.** Нет лицензионного лимита на количество настроенных камер, но есть явные ограничения ресурсов и admission control для активного анализа.
- **Облако дополнительно, не обязательно для локального наблюдения.** Сбой облака — состояние деградации, не доказательство отсутствия опасности.
- **Никаких обещаний гарантированного спасения или безусловной доставки.** Продукт не выдаёт отсутствие ответа за «всё безопасно».

### 1.2. Главная конкурсная развилка

Сочетание «локальные YOLO + облачная Gemini» само по себе **не доказывает** выполнение требования Firebase Hybrid Inference. Правила называют конкретные локальные Firebase-интеграции; наличие SDK в зависимостях также недостаточно. [R01, R02]

**Предлагаемое решение:** сохранить основной продукт без изменений этих требований и добавить **самостоятельный Firebase semantic worker** — вычислительный компонент, не приложение-компаньон.

Два исполнения одного провайдера:

| Исполнение | Назначение | Где выполняется | Статус решения |
|---|---|---|---|
| `firebase-cloud-node` | Headless-анализ изображений облачной моделью через Firebase AI Logic | Рядом с Core или на выделенном сервере | Реальный Node entry point существует в официальном SDK; конкретный pinned релиз, App Check и вызов проверяются интеграционным spike |
| `firebase-hybrid-web` | Реальная локальная семантическая проверка через Firebase, затем cloud fallback / дополнительная облачная проверка | Поддерживаемый Chrome на вычислительном узле | Требует проверки устройства, bootstrap и жизненного цикла; не обещать универсальный headless Docker |

Официальный исходник `@firebase/ai` содержит Node exports и Node integration tests. Это основание для реализации sidecar, **не доказательство**, что любой опубликованный SDK, любая модель и App Check уже работают в выбранной конфигурации. `main` не использовать как production dependency. [R06]

**Граница честности:** локальная Firebase-модель в Chrome не становится обычной серверной библиотекой от того, что процесс назван worker. Chrome предъявляет требования к оборудованию и загрузке модели; инициализация может требовать пользовательского действия. [R03, R04]

Если Core размещён на NAS, hybrid worker может быть отдельным доверенным вычислительным узлом в той же инфраструктуре. Закрытие всех компаньонов не должно останавливать ни Core, ни выделенный worker. Если worker требует открытого браузера, это явно показывается в его capability/status, а не маскируется под гарантированный сервис.

### 1.3. Пять условий, которые нельзя «закрыть» красивой архитектурой

| Gate | Что необходимо установить | Что разрешено делать до закрытия |
|---|---|---|
| **G1 — конкурс** | Допустимы ли вспомогательные YOLO вне Firebase AI Logic и предложенный отдельный hybrid worker в данной категории | Разрабатывать независимый Core; не объявлять сборку полностью соответствующей конкурсу |
| **G2 — runtime** | Реальные Firebase local/cloud-вызовы, App Check, перезапуск, модельное скачивание, background/locked-session поведение | Использовать честный mock для тестов транспорта; отдельно cloud-only профиль |
| **G3 — условия сервисов** | Допустимость конкретного сценария безопасности и транспорта уведомлений | Демонстрационные записи и согласованные non-life-critical сценарии; локальные технические проверки |
| **G4 — права** | Лицензии runtime, весов, данных и нативных библиотек в фактической сборке | Не распространять зависимости под неверной общей лицензией; вести SBOM |
| **G5 — Rust parity / native deployment** | Экспорт именно locked моделей; preprocessing/postprocessing и event semantics; ORT/FFmpeg на Windows/Linux без Python | Разрабатывать API и domain на fixtures; не считать ONNX-export эквивалентным исходной модели до проверки |

Особенно важно **G3**: документация FCM прямо запрещает emergency/high-risk применения, где отказ API может привести к гибели или травме. Это не просто предупреждение о best effort. Нельзя считать проблему решённой переименованием «fire alarm» в «review notification» или одной строкой disclaimer. Необходимо оценить реальное назначение продукта и условия всей используемой цепочки сервисов. [R07]

Поэтому `NotificationTransport` проектируется независимо от FCM. FCM-адаптер для реального life-critical режима не включается по умолчанию. Переход на прямой APNs или иной сервис также не считается автоматически достаточным юридическим решением. Полностью выполненный технический план не заменяет проверки G1/G3/G4.

---

## 2. Что изучено в старой репе

### 2.1. Объём исходного аудита (сохранён из редакции 1.0)

В предыдущем аудите получено дерево зафиксированного commit; прочитаны ключевые реализации `app`, `runtime`, `capture`, `buffer`, `perception`, `triggers`, `decisions`, `storage`, `sources`, `config`, Python SDK, manifest моделей, `pyproject.toml`, third-party notices; изучены существенные части `incidents` и `scheduler` и структура существующих тестов. [B00–B16]

Это **не** формальный аудит каждой строки всего репозитория, всех зависимостей или безопасности поставки. Полный исходный `pytest` в текущей среде не запускался. Изолированные проверки и микробенчмарки ниже действительно выполнены; их границы указаны отдельно.

### 2.2. Что сохраняем

В репе уже есть полезная инженерия: bounded compressed frame buffer; latest-frame захват без бесконечной очереди; собственные source timestamps и epochs; защита от просроченного cloud-ответа; история эпизодов и guard восстановления одного и того же человека; явный `UNKNOWN`; согласие на облачные расходы; строгая загрузка проверенных safetensors; hash-lock структуры модели; маски приватности до обработки; ограничения файлов и upload; sync/async HTTP SDK. [B01–B13]

Особенно важно не потерять medical-регрессии: частично видимый человек, смена track ID, окклюзия, повторное падение, восстановление активности, «другой человек стоит рядом» и изменения source clock. Новая оболочка не должна откатить эти свойства.

### 2.3. Findings: дефекты, ограничения и риски миграции

Обозначения: **D** — воспроизведённое поведение/дефект; **A** — подтверждённое архитектурное ограничение; **R** — риск, который требует отдельного теста. Priority относится к новой архитектуре, а не к оценке качества старого прототипа.

| ID | Тип / priority | Где и что обнаружено | Исправление и критерий проверки |
|---|---|---|---|
| AUD-01 | A / P0 | `runtime.Runtime` содержит один `capture`, `Analysis`, `camera_id`; `PUT /source` переключает источник, а не добавляет параллельный | `CameraRegistry` + независимые `CameraRuntime`; два одинаковых `track_id` с разных камер не объединяются |
| AUD-02 | A / P0 | `FireRules` использует score и совпадения рамок; локального представления controlled flame нет | Отделить visual detection от hazard policy; добавить зоны, временную динамику и структурированные контекстные признаки |
| AUD-03 | D / P1 | В `FireRules` две подходящие детекции подтверждают candidate даже после очень большого промежутка без вызовов | Проверять elapsed source time, непрерывность evidence и epoch; сбрасывать накопление при gap; не считать это доказанным реальным ложным alarm — пока это candidate |
| AUD-04 | D / P1 | Приоритет `3` привязан к литералу `0.65`, хотя `fire_immediate_confidence` настраивается | Единая policy-конфигурация и отдельный danger score; тест с threshold `0.9` |
| AUD-05 | D / P0 | `Storage.save_packet()` возвращает успех, если JPEG записан, даже когда запись metadata через `_write` завершилась ошибкой | Типизированный результат storage; успех только после metadata commit; при сбое отметить orphan и reconcile |
| AUD-06 | A / P1 | `capture.make_sample()` делает quality analysis и JPEG на каждом decoded frame до ограничения `FrameBuffer.fps` | Решать необходимость encode до JPEG; raw latest slot и evidence sampling отдельно |
| AUD-07 | A / P1 | `snapshot()` вызывает `storage.stats()` → рекурсивный обход evidence; SSE повторяет полный snapshot каждому клиенту каждую секунду | Учёт размеров в DB; редкая сверка файлов; один cached telemetry snapshot + delta event stream |
| AUD-08 | A / P0 | При исключении inference route получает `runtime_error`; `Analysis.process` затем пропускает всё, что не `ready` | Worker supervisor, bounded restart/backoff, health event; моделировать crash и восстановление без перезапуска всего Core |
| AUD-09 | A / P0 | Shared bearer token и статический `operator` header не обеспечивают отдельные устройства/роли | Per-device credentials, RBAC, revoke, pairing, обязательный TLS для сетевого доступа |
| AUD-10 | A / P1 | `/stream` — MJPEG, `/updates` — snapshots без устойчивого cursor/replay | WebRTC для live; SSE change log с event IDs и восстановлением; MJPEG оставить fallback/debug |
| AUD-11 | A / P0 | OpenAI связан с конфигом, model name, payload, parser, credentials и budget telemetry | Новая provider-neutral схема; удалить OpenAI из исполняемого пути, сохранить соответствующие policy-регрессии |
| AUD-12 | A / P1 | SDK не переподключает SSE, не запоминает cursor; offset pagination нестабильна при новых событиях | Reconnect + jitter + last event ID; stable keyset paging; typed transport errors |
| AUD-13 | R / P0 | Проверка RTSP URL синтаксическая; при удалённом управлении появляется SSRF/network probing поверхность | Разрешённые camera CIDR/ports, запрет metadata/служебных endpoints, контроль DNS и redirects |
| AUD-14 | A / P1 | `_write` коммитит операции по одной; нет атомарной связки incident + durable notification intent | Единственный DB writer; транзакционный outbox и change log |
| AUD-15 | A / P1 | Несколько `Runtime` умножат веса, thread pools, бюджеты и дисковые обходы | Shared inference workers и global resource/budget scheduler, а не копирование Runtime на каждую камеру |
| AUD-16 | R / P0 | Wrapper Ultralytics собирается через внутренние классы/атрибуты | Убрать Python-wrapper из production. В изолированном exporter восстановить **тот же** locked graph и overrides, экспортировать ONNX; принять Rust backend только после трёх уровней parity |
| AUD-17 | A / P1 | Серверная настройка источника не является полноценным durable CRUD нескольких камер | Versioned camera config, secret refs, atomic reconfigure/rollback, audit log |
| AUD-18 | R / P0 | Новая ретрансляция raw encoded stream может случайно обойти существующие privacy masks | Явные raw/redacted media variants и ACL; redacted path маскируется до выхода за trust boundary |
| AUD-19 | A / P1 | `queue_capacity`, budget и source clock спроектированы вокруг единственного источника | Per-camera fairness + общий лимит; source clocks нельзя сравнивать между камерами как один таймер |
| AUD-20 | R / P1 | Поток OpenCV нельзя надёжно принудительно завершить при зависшем нативном decoder | Изоляция capture в процесс; kill/restart по watchdog; reconnect с jitter |

### 2.4. Дополнительные риски именно Rust-переноса

Это проектные риски **R**, а не новые воспроизведённые дефекты старой репы.

| ID | Риск | Обязательная защита / тест |
|---|---|---|
| RUST-01 | ONNX экспортирует стандартную архитектуру, а не locked graph со старыми overrides | Manifest source hashes + reconstructed graph; экспорт из проверенного объекта; Python→ONNX tensor parity |
| RUST-02 | Одинаковый «YOLO decoder» неверно разбирает pose YOLO26 и fire YOLOv8 | Отдельные layout/postprocessor specs; NMS-mode фиксируется при export, а не угадывается по имени модели |
| RUST-03 | ORT/decoder блокирует Tokio executor; timeout не останавливает native работу | Выделенные workers, bounded admission, process watchdog и реальный kill/wait |
| RUST-04 | `Arc`/channels маскируют неограниченное удержание больших кадров | Byte budgets, leases, bounded in-flight и отдельный pinned-evidence budget; memory soak |
| RUST-05 | «Нативный exe» запускается только на машине разработчика из-за DLL/ABI | Clean-VM install без Python/Cargo; locked native deps, безопасный loader path, CPU target baseline |
| RUST-06 | Rust `.so` plugin ломается после смены compiler/layout | Внешние плагины через versioned IPC; не обещать стабильный ABI Rust trait objects |
| RUST-07 | Быстрая перепись меняет медицинские переходы и усиливает пропуски | Observation-trace differential tests; каждое intentional изменение отдельно версионировано |

**Не считать найденным нарушением:** root Apache-2.0 не означает, что автор уже неправомерно перелицензировал всё. `THIRD_PARTY_NOTICES.md` прямо сохраняет сторонние условия. Проблема новой сборки — проверить её конкретную композицию и не превратить эту аккуратную оговорку в ложное «всё Apache». [B14]

---

## 3. Матрица конкурсных требований

Краткое сопоставление правил; конкретные технические решения далее — наш проект реализации. [R01, R02]

| Требование | Техническое свидетельство |
|---|---|
| Firebase AI Logic реально вызывается | Integration trace, версия SDK, route/model, request metadata |
| Поддерживаемая локальная Firebase-платформа | Проверенный hybrid worker; не выдавать YOLO за Nano |
| App Check | Enforcement и проверка отказа без валидного токена |
| Существенные изменения существующего проекта | Baseline SHA, migration map, список новых модулей |
| Работающая тестовая сборка | Детерминированные fixtures, smoke test, воспроизводимые инструкции |
| Доступность проверки | Изолированное demo-окружение без production credentials |
| Открытые исходники с лицензией | SPDX/SBOM/NOTICE и доступные необходимые зависимости |
| Английский интерфейс и материалы | English locale как обязательная; технические инструкции EN |
| Права на SDK, веса и данные | Provenance manifests и license gate |
| Нет секретов и чужих персональных данных | Secret scan; синтетические/разрешённые тестовые материалы |
| Соответствие показанного работающему коду | Зафиксированная сборка, честные capabilities, без mock под видом AI |
| Допустимый контент | Нейтральные UI/error/demo strings, без брани и чужого брендинга |

App Store-публикация не является обязательным техническим требованием. Регистрация участника, eligibility, конкурсная форма, видео и финальная отправка — вне объёма. Возраст, резидентство, санкционные ограничения и отсутствие конфликтов интересов нельзя подтвердить анализом исходников; это отдельная проверка владельца проекта.

Для build provenance и срока жизни тестовой инфраструктуры зафиксировать окно: начало новых конкурсных изменений — **12 ноября 2026, 04:30 ICT**; окончание submission — **1 декабря 2026, 05:00 ICT**. Бесплатный доступ судьям к рабочей проверочной среде должен сохраняться до окончания judging — **17 декабря 2026, 05:00 ICT**. Это переводы указанных в правилах Pacific timestamps. Не полагаться на короткоживущие trial credentials: проверить expiry, бюджет и доступность тестового профиля. Процесс отправки материалов здесь не описывается. [R01]

Не добавлять Firestore/Auth/Remote Config только ради количества логотипов. Они нужны лишь для конкретных функций, а не как замена устойчивому локальному состоянию.

---

## 4. Архитектура и выбор стека

### 4.1. Принятый базовый стек

Rust — обязательное решение для собственного backend/engine. Выбор библиотек ниже — архитектурное решение, не обещание автоматического ускорения каждой стадии.

| Слой | Решение | Граница и причина |
|---|---|---|
| Core/control plane | Rust stable, edition 2024, Tokio | Служба без GUI; typed ownership, bounded concurrency; async только для подходящих I/O-задач |
| HTTP/API | axum + tower/tower-http; serde DTO | REST/SSE, auth/limits/tracing; domain не зависит от web framework |
| State/domain | Rust reducers/state machines, `Result` и явные enum-состояния | Воспроизводимые fire/fall transitions; никакого inference внутри HTTP handler |
| Metadata | SQLite WAL + `rusqlite`, dedicated writer thread | Прозрачные транзакции; минимум инфраструктуры; connection принадлежит одному исполнителю |
| Capture | Rust capture-worker + проверенные libav/FFmpeg bindings | PTS/timebase и frame metadata сохраняются; нативный decoder изолирован процессом |
| Media plane | MediaMTX + отдельные проверенные FFmpeg transcoders | Не переписываем RTSP/WebRTC/кодеки ради языковой чистоты |
| Local inference | Rust worker + `ort` + pinned ONNX Runtime, FP32 CPU baseline | Нет Python/Torch в production; GPU-профили после numerical/event parity |
| Evidence | Rust bounded buffers, immutable handles, lazy encoding | Учитываются байты и сроки жизни, а не только число элементов |
| Firebase adapter | Rust bridge + отдельный TypeScript/Node или Web SDK worker | Официальный Firebase AI Logic, без самодельного Gemini REST вместо конкурсной интеграции |
| Companion | Flutter/Dart + тонкие OS bridges | Windows/Linux/iOS/Android, без моделей и engine внутри клиента |
| Client SDK | Rust async-first; Python sync/async; Dart; TypeScript | Четыре независимых HTTP/SSE-клиента одного v2 контракта |
| Optional gateway | Rust для relay/auth/quotas/heartbeat | Firebase Admin/App Check signing — узкий отдельный официальный SDK broker, когда нужен |
| AlarmAgent / CLI | Rust, platform adapters | User-session звук отдельно от service; CLI не запускает скрытую GUI-сессию |

axum интегрируется с Tokio/Tower; SQLite connection в rusqlite не `Sync`, поэтому выбран отдельный владелец, а не общий connection в handlers. [R28, R29] `ort` — поддерживаемый его авторами Rust-интерфейс к ONNX runtime, не собственный Rust-порт всей математики Microsoft; выбранная связка crate/native ABI требует проверки. [R30]

**Toolchain:** в P00/P01 выбрать реально проверенную stable-версию и закрепить точный номер в `rust-toolchain.toml`; в crates указать проверенный MSRV через `rust-version`, а не обещать минимальную версию, на которой сборка не запускалась. Edition `2024` не является номером compiler. Для virtual workspace явно `resolver = "3"`. `Cargo.lock` коммитится; CI использует `--locked`. Версии ORT/FFmpeg/SDK и их features фиксируются отдельно. [R32]

Flutter остаётся UI-решением, а не местом исполнения Rust Core. Tauri, Rust FFI в телефонах или новый собственный GUI framework не добавляются без отдельной причины: они не нужны, чтобы выполнить перепись автономного движка. Разрешённые native bridges и аппаратное декодирование видео не являются ML inference. [R13, R14]

**Не делаем:** embedded CPython/PyO3 как скрытый permanent runtime; новую реализацию нейросетей с нуля на Candle/Burn вместо проверяемого ONNX-переноса; бесконечные `tokio::spawn`; Kafka/Redis/Kubernetes для домашнего Core; общий глобальный `Arc<Mutex<Everything>>`; свои кодеки, криптографию и Firebase SDK ради лозунга «100% Rust».

Rust не отменяет C/C++/GPU dependencies ORT и FFmpeg, лицензии весов и условия cloud APIs. Изолированный и измеряемый native backend предпочтительнее обещания «один exe без зависимостей», которое не соответствует поставке.

### 4.2. Логическая схема

```text
RTSP / USB / file
        │
        ▼
Capture + Media router ───────────────► authenticated live stream
        │                                         │
        ▼                                         ▼
Per-camera latest frames                  Desktop / iOS / Android
        │                                  Companion (NO inference)
        ▼                                         │
Shared local inference workers                    │ REST + SSE
        │                                         │
        ▼                                         ▼
Temporal tracking + fire/fall evidence ◄────── Core control API
        │
        ├──► local policy / preliminary attention event
        │
        ▼
Immutable EvidencePacket
        │
        ▼
Firebase provider adapter
        ├── cloud Node worker ── Firebase AI Logic ── cloud model
        └── hybrid Web worker ── Firebase AI Logic ── local / cloud
        │
        ▼
Schema validation + freshness + policy guards
        │
        ▼
Incident state + change log + transactional outbox
        ├──► local alarm agent / supported hardware adapter
        └──► approved remote notification transport
```

Rust Core — единственный authoritative writer инцидентов и настроек. Firebase worker возвращает наблюдения, не посылает сирену напрямую. Компаньон отправляет команды, а не редактирует локальную «копию истины».

---

## 5. Структура новой репы

```text
stillwatch-next/
├── Cargo.toml                       # workspace, shared dependencies/lints/profiles
├── Cargo.lock
├── rust-toolchain.toml              # exact tested stable compiler
├── .cargo/config.toml               # xtask alias, target build configuration
├── crates/
│   ├── sw-domain/                   # pure fire/fall/incident state machines
│   ├── sw-protocol/                 # validated wire DTOs, IDs, schemas, error codes
│   ├── sw-engine/                   # registry, scheduling, supervision, evidence
│   ├── sw-storage/                  # rusqlite writer, migrations, outbox, backup
│   ├── sw-inference/                # ORT boundary, preprocess, per-model decode
│   ├── sw-capture/                  # native capture boundary, PTS, IPC frames
│   ├── sw-api/                      # axum routes, auth, pairing, REST/SSE
│   ├── sw-adapters/                 # Firebase bridge, media, secrets, alerts
│   └── stillwatch-sdk/              # public Rust client, no engine/native ML deps
├── bins/
│   ├── stillwatch-core/             # service executable, composition root
│   ├── stillwatch-worker/           # subcommands capture/inference, no public API
│   ├── stillwatch-cli/              # install/doctor/pair/backup/migrate/replay
│   ├── stillwatch-alarm-agent/      # opt-in desktop user-session agent
│   └── stillwatch-gateway/          # optional Rust relay/control service
├── apps/companion/                  # Flutter UI, does not link Core
│   ├── lib/{features,platform,app}/
│   ├── android/
│   ├── ios/
│   ├── windows/
│   ├── linux/
│   └── integration_test/
├── workers/
│   ├── firebase-cloud/             # official JS SDK; isolated Node candidate
│   └── firebase-hybrid-web/        # separate supported Chrome compute worker
├── services/firebase-token-broker/ # optional official Admin SDK bridge
├── packages/
│   ├── contracts/{openapi.yaml,schemas,examples,compatibility}/
│   ├── sdk-python/                 # pure HTTP sync/async client, not Core binding
│   ├── sdk-dart/
│   ├── sdk-typescript/
│   └── firebase-provider-common/
├── models/
│   ├── models.lock.json            # ONNX + source provenance + preprocessing
│   ├── native-deps.lock.json       # ORT/FFmpeg/EP binaries and hashes
│   ├── licenses/
│   └── evaluations/
├── tools/
│   ├── model-export/               # isolated Python 3.12 + its own uv.lock
│   ├── legacy-oracle/              # pinned Python baseline; tests only
│   └── contract-generation/
├── xtask/                          # Rust CI/dev orchestration, non-production
├── tests/
│   ├── fixtures/{observations,tensors,expected-events}/
│   ├── differential/
│   ├── contract/
│   ├── integration/
│   ├── failure-injection/
│   ├── e2e/
│   └── performance/
├── benches/                        # Criterion scenarios where appropriate
├── fuzz/                           # parsers, DTO validation, state traces
├── deploy/{docker,systemd,windows-service,media,gateway}/
├── packaging/{windows,deb,rpm,android,ios}/
├── docs/{adr,protocols,operations,technical-testing}/
├── .github/workflows/
├── package-lock.json               # chosen JS workspace lock; not Cargo replacement
├── LICENSE
├── NOTICE
├── THIRD_PARTY_NOTICES.md
└── baseline.json
```

Это логические границы. В первой реализации не дробить каждую папку на отдельный crate: внутри `sw-engine` держать modules для registry/scheduler/evidence, пока отдельный crate не даёт реальную изоляцию зависимостей или reuse.

**Dependency direction:** `domain` не зависит от Tokio, axum, ORT, Firebase и storage; `protocol` — wire-формат, не набор ORM entities; `engine` вызывает порты/команды; adapters реализуют порты; `core` связывает компоненты. `stillwatch-sdk` зависит от protocol/client transport, но не от `sw-engine`, `sw-inference` или `sw-capture`.

`domain` не читает часы/файлы сам: время и observations передаются явно. Domain mutation возвращает transition/effects; persistent state и outbox фиксируются storage-командой. Не сериализовать внутренний memory layout domain напрямую как формат API или DB.

CI проверяет дерево зависимостей отдельно для Core, SDK и каждого companion. В desktop/mobile bundle запрещены Python/Ultralytics/ORT/model assets; исключение — отдельно выбранный пользователем **Core installer**, а не спрятанный runtime внутри Companion.

**Features:** CPU inference и platform adapters собираются по явной матрице; CUDA/TensorRT и test-only oracle не включаются через общий `--all-features`. Native optional deps должны отсутствовать из SDK dependency tree. `cfg(target_os)` ограничивает platform-specific код, не подменяет runtime capability checks.

---

## 6. Core: процессы и жизненный цикл

### 6.1. Процессы, executors и владение состоянием

**`stillwatch-core`** — Rust service с Tokio runtime для API, таймеров, IPC/network и supervision. На Tokio workers не выполняются длительные ORT calls, codec loops, синхронные SQL/disk scans или JPEG encoding.

**Camera actor** владеет scene revision, tracker history, fire/fall episode state и расписанием одной камеры. Это lightweight task/state owner, не копия всего Core и не обязательный OS thread. На API handlers выдаются immutable snapshots/command handles, а не mutable ссылки на policy state.

**`stillwatch-worker capture`** — отдельный Rust process на источник в начальном профиле. Он владеет decoder/context, извлекает PTS и timebase, применяет допустимое преобразование геометрии, формирует typed frames. `sw-capture` использует pinned `ffmpeg-next`/libav boundary после P00 smoke. Если конкретный USB backend не работает в service mode, capability честно отключается; camera driver не заменяется выдуманным универсальным индексом. [R35]

**`stillwatch-worker inference`** — отдельный Rust process на device/backend, разделяемый камерами. Внутри него model sessions живут у выделенного executor/ограниченного пула; отдельное состояние сессии не раздаётся конкурентным вызовам без проверки API. Не помещать ORT mutex вокруг всего Core. CPU baseline: serial task admission + рассчитанный ORT thread budget; GPU: общий model pool и bounded batch scheduler.

**DB writer** — выделенный `std::thread`, владеющий `rusqlite::Connection`. Он получает bounded commands, отвечает через oneshot. Небольшой отдельный read executor обслуживает чтение своими connections. Нет синхронного SQL внутри async handlers; короткие transactions объединяют state/outbox/change events. [R29]

**Firebase worker** остаётся отдельным JS runtime: Rust даёт ему versioned request и принимает validated response. **MediaMTX** — самостоятельный media process. Внешний Node/Chrome/MediaMTX не превращается в Rust-код, но все решения об инцидентах остаются в Rust Core.

### 6.1.1. Каналы и backpressure

| Поток | Механизм / политика | Что нельзя делать |
|---|---|---|
| API command → domain/storage | Bounded `mpsc`, ответ `oneshot`, deadline/idempotency | Подтверждать success до commit или бесконечно копить команды |
| Latest analysis frame per camera | Один owned latest slot / `watch<Arc<Frame>>`; old pending frame заменяется | Использовать lossy slot для истории инцидентов |
| Inference work/results | Bounded queue + permits, FrameKey и generation | Создавать task на каждый кадр быстрее, чем идёт обработка |
| Telemetry/config snapshot | `watch`, coalescing допустим | Называть coalesced updates полным audit log |
| Incident changes | DB change log — источник истины; `broadcast` только wake-up | Потерять событие из-за `Lagged` и продолжить как ни в чём не бывало |
| Notification delivery | Transactional DB outbox | Заменить outbox in-memory channel |

`watch` хранит последнее значение, `broadcast` может сообщить о пропущенных значениях медленному читателю. Поэтому оба механизма используются лишь в соответствующих им ролях; SSE читает durable sequence и умеет resync. [R26]

После получения latest frame кратковременно клонировать `Arc`, отпустить borrow/lock и только затем делать `.await`. Не удерживать mutex/`watch::Ref` во время inference или сетевого ожидания. Число held frame handles ограничено permits и global byte budget: `Arc` не является защитой от утечки удержанием ссылок.

### 6.1.2. CPU work, deadlines и cancellation

`spawn_blocking` подходит для ограниченных блокирующих операций, но **запущенную blocking closure нельзя остановить обычным abort**. Таймаут ожидания результата не означает, что нативный inference прекратил занимать CPU/GPU. Поэтому admission ограничивается до запуска работы; после timeout слот не освобождается, пока работа реально не закончилась либо worker не остановлен. [R25]

При необходимости жёсткого timeout supervisor завершает изолированный process, дожидается exit/reap, инвалидирует worker generation и только потом создаёт замену. Внешний вызов может уже быть оплачен: cancellation не обещает rollback cloud billing.

`CancellationToken`/task tracking обеспечивают cooperative shutdown для async-задач. Child process хранится у supervisor; `Drop`/отмена future не считаются полноценным process-tree cleanup. Windows Job Object и Linux service/cgroup profile используются там, где нужны дочерние process-tree limits/cleanup; после kill явно `wait`. Логи stdout/stderr читаются параллельно с bounded buffering. [R27]

### 6.1.3. IPC и буферы

Native worker protocol v1: version/capability handshake, request ID, camera/epoch/frame ID, schema version, payload length и deadline. Header и binary frame читаются с жёсткими пределами до allocation; shape/stride/bytes согласуются checked arithmetic. Не использовать ABI Rust struct или trait object как wire format.

Первая реализация допускает **ограниченное число копирований через local IPC**, с измерением bytes/sec. JSON описывает metadata, а raw/JPEG bytes идут отдельным bounded payload; не base64 на каждом внутреннем hop. Физический LAN/remote worker получает TLS и scoped identity; это не публичный неавторизованный inference socket.

Shared memory — следующая оптимизация только после профиля: fixed slot pool, leases, generation и явный ACK release; slot нельзя переиспользовать, пока native inference читает его. `Arc` внутри процесса не управляет сроком жизни mmap-буфера в другом процессе. Crash worker освобождает leases только после подтверждённого завершения; stale results от прежней generation отвергаются.

---

### 6.2. Порядок запуска

1. Проверить config schema, права на data directory, version locks и свободное место.
2. Получить instance lock: два Core не могут владеть одним writable data directory.
3. Открыть БД; выполнить проверенную миграцию; при необходимости использовать предварительный backup.
4. Загрузить identity и credentials через секретное хранилище; не печатать их.
5. Запустить API в состоянии `initializing`; разделить liveness и readiness.
6. Поднять media router, проверить version/hash бинарников.
7. Загрузить модели один раз, проверить hash и class map, выполнить warmup.
8. Восстановить camera registry и запустить capture staggered, без reconnect storm.
9. Восстановить незавершённые incidents как `assessment_stale`; не присваивать `safe`.
10. Поднять outbox sender, scheduler и watchdog; зарегистрировать mDNS при разрешении владельца.
11. Перейти в `ready`/`degraded` с детализацией по камерам и маршрутам.

Пример CLI-контракта (служба и административный клиент — разные binaries):

```text
stillwatch-cli init
stillwatch-core run --config <path>
stillwatch-cli doctor --json
stillwatch-cli pair --expires-in 300
stillwatch-cli service install|start|stop|status
stillwatch-cli backup --destination <path>
stillwatch-cli restore --from <path> --dry-run
stillwatch-cli migrate --legacy-data <path> --destination <path> --dry-run
```

Названия команд — целевой интерфейс, не утверждение, что они уже реализованы.

### 6.3. Установка и отказоустойчивость

Windows: SCM service с отдельной низкопривилегированной identity; Linux: systemd unit; NAS: Docker/Compose с restart policy и persistent volumes. Нет обязательного tray или открытой консоли. Не отключать sleep всей системы без согласия: показывать, что при sleep Core не наблюдает.

Перезапуск worker: bounded exponential backoff с jitter; circuit breaker после повторяющегося crash; статус `analysis_unavailable`, отдельное уведомление о здоровье системы. Не бесконечно перезапускать повреждённые веса. Для GPU OOM сначала уменьшить batch, затем перегруппировать нагрузку, затем явная деградация — не молчаливое падение на очень медленный CPU.

Graceful stop: перестать принимать новые задачи; сохранить state/outbox; ограниченно дождаться запросов; отменить оставшиеся; закрыть shared-memory handles; остановить decoder/media; завершить writer. Deadline shutdown настраиваемый и проверяется тестом.

**Windows-особенность:** служба работает не в пользовательской desktop-сессии. Для UI/звука нужен per-user AlarmAgent либо подходящее внешнее устройство; нельзя обещать, что системный сервис сам покажет окно пользователю. [R11] На NAS без динамика «локальная сирена» также требует отдельного выходного устройства.

---

### 6.4. Ошибки, ownership и native boundary

В `sw-domain`, `sw-engine`, `sw-api` и SDK включить `#![forbid(unsafe_code)]` для собственного кода. FFI — в узких native/platform adapters с документированными invariants и тестами; сторонние зависимости могут содержать unsafe, поэтому общий запрет не означает доказанную memory safety всей цепочки. Нельзя вручную писать `unsafe impl Send/Sync`, чтобы заставить compiler принять неподходящий decoder/session. [R36]

Ожидаемые отказы выражаются `Result<T, Error>`: network disconnect, corrupt frame, no model, disk full, unavailable provider. `unwrap/expect` на внешнем вводе или нормальном runtime path запрещены. Ошибка камеры не завершает процесс Core; panic рассматривается как дефект, а не штатный механизм восстановления. Повторяющийся дефект вызывает явную деградацию и ограничение restart, не бесконечную петлю.

Завершение service переводится в общий shutdown protocol из SCM/systemd/CLI. Для Windows status/checkpoints во время model load передаются корректно, а не блокируют SCM в ложном `Running`. Конкретный Windows adapter — pinned `windows-service` с проверенными правами и lifecycle. [R34]

---

## 7. Источники и многокамерность

### 7.1. Поддерживаемые входы первой полноценной версии

- RTSP/RTSPS с явно указанными credentials, TCP/UDP transport profile и reconnect policy.
- USB/UVC-камера, доступная **хосту Core**; stable device identity предпочтительнее индекса `0`.
- Загруженный видеофайл для replay/testing; отдельный `live=false`, чтобы учебный файл не запускал реальные сирены по ошибке.
- ONVIF discovery как отдельный адаптер обнаружения/получения RTSP URL, не как обязательная зависимость каждой камеры.

«Любое число входов» означает отсутствие жёсткого продуктового лимита на записи registry. Это не означает поддержку произвольного видеопротокола или бесконечную одновременную вычислительную мощность.

### 7.2. Camera registry

Для каждой камеры хранить:

```text
camera_id: UUID
name, enabled, tags
source_type, endpoint_without_credentials, credentials_ref
main_stream, analysis_substream
requested_pose_fps, requested_fire_fps, evidence_fps
priority, resource_profile
privacy_masks, controlled_fire_regions, scene_revision
retention_policy, cloud_consent
config_revision, created_at, updated_at
```

Runtime-поля отдельно: `source_epoch`, `capture_state`, `last_frame_age`, `last_analysis_age`, actual FPS, decoder error, queue age, dimensions, codec, capacity status.

Добавление через Companion: validate syntax → безопасный probe с timeout → показать codec/resolution/доступность → capacity estimate → подтвердить config → durable commit → запустить. Не активировать неизвестный remote URL просто потому, что текстовое поле приняло строку.

Update применяет optimistic concurrency. Если новая конфигурация не стартовала, пользователь видит rollback/failed update; остальным камерам ничего не происходит. Удаление источника не удаляет историю без отдельной retention-команды.

### 7.3. Capacity admission

Registry может содержать много выключенных камер. Перед включением оценить decode, inference, memory и outgoing video budget. Когда минимальный проверенный FPS недостижим, показать `capacity_exceeded` либо предложить конкретный меньший профиль; не выдавать пониженный режим за исходный.

Камера на частном LAN недоступна облачному Core без сетевого маршрута. Это решается VPN/туннелем/edge-узлом, а не обещанием Companion «подключаться откуда угодно». IP из телефона не делает этот IP достижимым из VPS.

---

## 8. Конвейер видео и временная модель

### 8.1. Разделение потоков данных

1. **Encoded live stream** — по возможности remux/pass-through, без JPEG и ML.
2. **Raw analysis frames** — bounded latest slots; разрешение и cadence соответствуют task profile.
3. **Evidence ring** — compressed snapshots с выделенным byte/time budget.
4. **Pinned incident evidence** — отдельный bounded pool для начала и развития инцидента.
5. **Optional event clip** — короткий video segment, не бесконечная запись по умолчанию.

Не превращать все камеры в JPEG-поток, не держать все raw frames за 30 секунд и не отправлять весь live stream в облачную модель.

### 8.2. Идентификаторы и часы

```text
FrameKey = (core_instance_id, camera_id, source_epoch, frame_id)
TrackKey = (camera_id, source_epoch, track_id)
PacketKey = (incident_id, evidence_version, packet_id)
```

Использовать три времени: `source_time` для последовательности движения; `monotonic_time` для deadlines/возраста на текущем процессе; `UTC wall time` для UI и аудита. Сохранять исходный PTS и факт его достоверности. После reboot monotonic timestamps прошлого процесса недействительны.

В Rust хранить source PTS как проверенное целое + рациональную timebase и нормализованное source-time значение; conversion должен сохранять порядок, корректно учитывать overflow и неизвестный PTS. `Instant` не сериализуется между процессами/перезапусками. Строковые wire-представления `u64` sequence/epoch/revision предотвращают потерю точности у JS/Dart-клиентов; это фиксируется JSON Schema. До переноса геометрии не менять одновременно её float precision: сначала воспроизвести legacy расчёты на `f64`, отклоняя NaN/Inf на входе, затем оптимизировать измеренно.

Reconnect, смена камеры, изменение геометрии, значительный source discontinuity увеличивают epoch. Scene/zone mapping при перемещении камеры инвалидируется. Old cloud results не могут повлиять на новую сцену.

### 8.3. Планировщик

Global scheduler выбирает наиболее свежий кадр для каждого camera/task и распределяет GPU/CPU по weighted round-robin с aging. Priority incident может временно повысить cadence, но не лишить остальные камеры базового fire heartbeat.

**Backpressure применяется к необработанным кадрам, не к durable events.** Старые кадры заменяются свежими; инциденты и outbox нельзя бесследно терять. Evidence ring отдельно сохраняет pre-event контекст. Низкий FPS не должен ломать motion model: всё считается по timestamps, а coverage фиксируется явно.

Предлагаемые стартовые параметры, не доказанные оптимумы: pose 5 FPS, fire 2 FPS, evidence 5 FPS; при подозрении — временный burst. На CPU batch=1, GPU batch=1/2/4/8 после измерений. Microbatch wait не более 20–50 ms при обычной нагрузке; urgent candidate может обходить ожидание. Эти значения затем меняются по measured recall/latency.

### 8.4. Evidence packet

```json
{
  "schema_version": "1.0",
  "packet_id": "uuid",
  "incident_id": "uuid",
  "camera_id": "uuid",
  "source_epoch": "4",
  "evidence_version": "7",
  "scene_revision": "3",
  "category": "fire",
  "observed_interval": {"start_source_s": 12.4, "end_source_s": 16.9},
  "coverage": {"expected_samples": 10, "received_samples": 8, "max_gap_ms": 800},
  "images": [
    {"image_id": "f42-wide", "source_s": 16.9, "kind": "overview", "sha256": "...", "mime": "image/jpeg"}
  ],
  "local_facts": {"flame_detected": true, "controlled_region_overlap": 0.82},
  "limitations": ["partial_occlusion"],
  "expires_at": "UTC timestamp",
  "policy_version": "fire-policy-1",
  "prompt_version": "fire-evidence-1"
}
```

Изображения передавать бинарно через локальный IPC/HTTP multipart или ограниченные content-addressed refs; не копировать base64 между каждым внутренним модулем. Внешний provider может потребовать base64 — преобразовывать на границе. Reference не должен становиться произвольным URL, который worker скачает без проверки.

Пакет неизменяем. Ответ обязан соответствовать packet ID, evidence version, camera epoch и scene revision. Смысловая оценка не должна назначать идентификаторы или версии самостоятельно: это делает adapter из исходного request context.

---

## 9. Огонь: контекст, риск и ложные тревоги

### 9.1. Главный принцип

**Уверенность в наличии пламени ≠ вероятность опасного пожара.** Свеча может иметь очень высокий detector score; реальный дым на плохом изображении — низкий. Повышение одного confidence threshold одновременно сокращает ложные срабатывания и может увеличивать пропуски. Поэтому требуется отдельная оценка события, а не только настройка YOLO.

`FireRules` формирует candidates, а не окончательную сирену. Нельзя оценивать ложные тревоги по количеству candidates без измерения всей цепочки. Старый cloud prompt уже упоминает controlled flame/steam; новая версия должна превратить это из общей фразы в проверяемые данные и policy guards. [B04, B05]

### 9.2. Представление сцены

Владелец может разметить `controlled_fire_region` — горелку, камин, мангал, свечу. Это **контекст**, не permanent ignore zone. Разметка содержит polygon, тип источника, разрешённый режим, version и геометрию камеры.

Нельзя автоматически объявлять объект безопасным потому, что рядом есть человек или модель назвала его stove. Визуальное присутствие человека не доказывает контроль. Автоматически предложенные зоны требуют подтверждения владельца.

Изменение ракурса, zoom, resolution/crop geometry или сильный scene shift переводят зону в `needs_review`. Зона не переносится слепо на новую картинку.

### 9.3. Признаки fire episode

Рассчитывать на временной последовательности, с учётом camera motion и качества:

| Группа | Признаки | Ограничения |
|---|---|---|
| Достоверность детекции | persistence, class/box consistency, model version | Detector score не считать калиброванной вероятностью опасности |
| Геометрия | flame area/frame area, расширение region, выход за controlled polygon | Bounding box колеблется; использовать robust trend, а не один скачок |
| Дым | наличие, направление/разрастание plume, совместное появление с пламенем | Пар/туман/экспозиция могут выглядеть похоже |
| Контекст | допустимая область, окружающие предметы, путь распространения | Не делать вывод о горючести невидимых материалов |
| Динамика | рост площади на интервале, новые соседние regions, персистентность | При gap нельзя считать наблюдение непрерывным |
| Качество | blur, glare, dark, occlusion, frozen feed, moved camera | Низкое качество даёт uncertainty, а не safe |

Первоначально используем прозрачные правила + context model. Позже можно обучать event classifier на размеченных последовательностях. Переход к обучаемому score требует нового evaluation gate, а не автоматического «self-learning» из всех нажатий пользователей.

### 9.4. Структурированный ответ semantic provider

```text
classification:
  no_visible_fire | likely_controlled_flame |
  possible_uncontrolled_fire | likely_uncontrolled_fire | insufficient_evidence
observations:
  flame_visible: yes | no | unknown
  smoke_visible: yes | no | unknown
  apparent_spread: yes | no | unknown
  contained_in_designated_region: yes | no | unknown
  controlled_source_supported: yes | no | unknown
  visibility: good | limited | poor
supporting_image_ids: [known packet image IDs]
contradictions: [enum]
limitations: [enum]
brief_description: bounded string
```

Не требовать произвольный `confidence: 0.97` от LLM и не смешивать его арифметически с YOLO score. Если provider выдаёт confidence, хранить как некалиброванный служебный атрибут, не как «97% вероятность пожара».

Полный кадр обязателен хотя бы для одного актуального момента; crop — дополнительный. Отрицательный ответ на crop не очищает остальную сцену. Модель не должна читать текст на изображении как команду.

### 9.5. Политика переходов

| Ситуация | Действие |
|---|---|
| Единственная слабая детекция, нет других признаков | `observing`; собрать дополнительные кадры |
| Стабильное пламя внутри подтверждённой зоны, нет видимого распространения, достаточное качество | Продолжать наблюдение; без новой сирены, но с периодической переоценкой |
| Controlled zone + выход огня за границы / нарастающий дым / видимое распространение | Зона не подавляет эскалацию; немедленная проверка и соответствующий attention level |
| Явные быстро развивающиеся признаки вне разрешённой области | Предварительный urgent attention event по validated local policy, не ждать длинный batch/cloud |
| Cloud unavailable или invalid response | `unknown/degraded`; не переводить в safe; local policy продолжает работать |
| Человек нажал false alarm | Записать feedback и ограничить повтор той же неизменной evidence; новая существенная evidence снова оценивается |
| Камера пропала | Camera health incident; предыдущая опасность не считается разрешённой |
| Уже есть urgent event, затем один отрицательный ответ | Не снимать latch автоматически; нужен установленный resolution policy и/или оператор |

До валидации local urgent policy не использовать один YOLO score как самостоятельное подтверждение пожара. В UI честно различать «подозрение, требуется немедленная проверка» и подтверждённые человеком обстоятельства.

### 9.6. Что специально тестировать

Плита с нормальным пламенем; свечи; камин; мангал; горящий экран/видео с огнём; отражения; закат; красное освещение; пар чайника; дым готовки; сварка; сигаретный дым; огонь вне expected region; возгорание соседнего предмета; смещение камеры; загрязнение линзы; ночь/IR; маленький объект вдали; постепенное задымление без видимого пламени.

Негативы и позитивы должны быть разными **эпизодами/местами/камерами**, а не соседними кадрами одного ролика, случайно разнесёнными по train/test.

---

## 10. Падения и другие видимые опасности

Переписываем текущие `MotionClassifier`, tracker, episode/identity guards **на Rust**, сохраняя поведение по recorded observation traces. Python reference выполняется только в offline differential harness. Сначала parity, затем отдельно версионированные исправления — не механическое переименование классов и не permanent Python subprocess. Обычная геометрия «человек горизонтальный» не является надёжной тревогой.

Событие описывает наблюдаемое: `suspected_fall`, `persistent_low_posture_after_transition`, `repeated_failed_recovery`, `visibility_lost_after_suspicion`. Не утверждать «без сознания», «не дышит», «травма» на основании видео.

Дополнительные обязательные условия новой реализации:

- Camera/epoch-qualified person IDs; один человек на камере A не имеет отношения к тому же числу track ID на камере B.
- Сохранение onset и pre-event frames, а не только финального человека на полу.
- Recovery относится к тому же человеку и актуальному episode; соседний стоящий человек не закрывает событие.
- Сам факт подъёма головы, движения рукой или краткого вставания не равен устойчивому восстановлению; при этом не требуется ходьба от человека, у которого наблюдался seated baseline.
- Пропадание keypoints/окклюзия не является неподвижностью или выздоровлением.
- Выбор observation window — отдельная задача по категориям. Старые 15 секунд medical window не переносить на fire.
- Реакция пользователя не обучает модель немедленно. Feedback идёт в versioned review dataset после контроля качества и согласия на использование.

Новые категории опасностей подключаются через `HazardPolicy` и имеют отдельные datasets, lifecycle и критерии. Не обещать «любую опасность» одной общей Gemini-командой.

## 11. Firebase-провайдер и настоящая гибридность

### 11.1. Provider-neutral contract на Rust

Контракт разделяет результат модели и обязательное доменное решение. Ни JS worker, ни SDK не имеют права самостоятельно закрывать incident или запускать сирену.

```rust
// Проектный интерфейс: доменные типы определяются в sw-domain/sw-protocol.
// Фрагмент описывает контракт, не является готовым автономным crate.
use std::{future::Future, pin::Pin, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

pub trait SemanticProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    fn evaluate(
        &self,
        packet: Arc<EvidencePacket>,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> ProviderFuture<'_, SemanticAssessment>;
    fn health(&self) -> ProviderHealth;
    fn shutdown(&self) -> ProviderFuture<'_, ()>;
}
```

Boxed futures выбраны для низкочастотной provider boundary и object-safe dispatch; в tensor hot path этот dynamic interface не нужен. Capability snapshot обновляется отдельным handshake; `health()` не совершает внезапный синхронный network request. Shutdown явный: `Drop` не заменяет async cleanup.

`ProviderCapabilities` сообщает modalities, max image count/bytes, structured-output dialect, available routes, cancellation semantics и фактические SDK/model versions. Они проверяются handshake/spike, а не выводятся из названия «Firebase».

Rust bridge валидирует DTO, response size, timestamps, known image IDs, packet/version/camera/scene binding. Provider выдаёт только observation fields; binding metadata присоединяется доверенным bridge из исходного request, а не доверяется LLM. Unknown enum/safety block/invalid JSON → typed uncertainty/error, не `NO_ALERT`.

Для same-host JS bridge базовый транспорт — loopback HTTP с random scoped credential, body/deadline limits и Origin/Host checks; UDS/named pipe возможен после проверки поддержки конкретных runtime. Remote compute worker — TLS/mTLS и отдельная registered identity. Прямой вызов Gemini REST из Rust **не подменяет** требуемый Firebase AI Logic.

Официальный список Firebase AI Logic SDK не содержит Rust. Community crate с похожим названием не считается официальной интеграцией или доказательством конкурсного соответствия. [R23]

### 11.2. Cloud Node worker

Сначала проверить конкретный published Firebase JS SDK: package lock, Node engine, import `firebase/ai`, text+image request, schema, timeout, cancellation, App Check. В просмотренном upstream `main` объявлен Node `>=24.12.0`; это **не** основание считать эту ветку опубликованной стабильной версией. Установить фактическую совместимую версию через package metadata и интеграционный тест. [R06]

Worker получает запросы от Rust Core по выбранному в 11.1 loopback transport; UDS/named pipe — только после совместимого spike. Если размещён отдельно — TLS/mTLS с device identity. Публично открывать его как бесплатный inference proxy нельзя.

Cloud model ID задаётся явно через allowlisted `provider-models.lock.json`. Не полагаться на меняющийся default, не использовать `latest` и не утверждать, что самый новый/дорогой модельный вариант автоматически точнее на этих событиях. Сравнить разрешённые на дату реализации варианты по holdout accuracy, latency и стоимости. Модель можно переключить только на предварительно проверенную версию; Remote Config не должен обходить локальный safety allowlist. [R03, R05]

### 11.3. Hybrid Web worker

Это отдельное приложение вычислительного узла с маленькой страницей bootstrap/health. В нём нет экранов камер/управления вместо Companion. Оно получает минимальные evidence packets и выполняет **реальную полезную задачу**: классификацию видимых признаков controlled flame/неясного эпизода. Результат участвует в Core policy.

Проверяем три режима: local-only, cloud-only, prefer-local с fallback. Availability fallback SDK не равен автоматическому обнаружению «локальная модель ошиблась». Дополнительная cloud verification по uncertainty/значимости evidence — наша политика поверх SDK. [R03, R05]

На первом этапе local analysis использует подтверждённо поддерживаемое число JPEG/PNG; если локальная ветка принимает только один кадр для данной конфигурации, Core агрегирует временные факты сам. Не склеивать кадры в плохо читаемый collage ради искусственного обхода лимитов. Cloud branch может получать небольшой хронологический пакет после проверки своих лимитов.

Признаки успешного spike:

- Зависимости действительно вызывают Firebase AI Logic, а не прямой Chrome Prompt API вместо него.
- SDK/telemetry подтверждают фактическое local выполнение; отсутствие сетевого запроса проверено дополнительной network trace, а не является единственным доказательством.
- Cloud branch действительно выполняется при local unavailable и при явно запрошенной cloud verification.
- Local model уже загружена до активного наблюдения; прогресс/нехватка ресурсов видны.
- Есть тест при закрытом Companion, блокировке пользовательской сессии, reboot и потере интернета.
- Если браузер остановился, Core сообщает `semantic_local_unavailable` и не перестаёт локально наблюдать.

**Не обещать:** Nano внутри Electron/WebView/голого Chromium/Windows Service без проверки. Chromium runtime не автоматически равен поддерживаемому Google Chrome с нужной моделью. Browser-specific requirements фиксируются в `doctor` и manifest, а не скрываются в README. [R04]

### 11.4. App Check и секреты

Для локальной разработки — отдельные debug tokens в private environment. Для управляемого headless worker — custom App Check provider, если подтверждён для выбранного SDK. Сервер выдачи токенов проверяет зарегистрированную worker identity, владельца, challenge и ограничения; это не общедоступный endpoint «дай токен». Firebase Admin credentials находятся только на доверенном backend/BYO-хосте, не в APK/IPA/desktop bundle. Основной gateway написан на Rust; при необходимости официальный Admin SDK запускается в узком Node token-broker, а не переписывается вручную. Broker принимает только authenticated issuance-команды с owner/worker scope и квотой. App Check не заменяет user/device authorization. [R08]

Разделить:

- Firebase client config/project identifiers — публичная конфигурация, не универсальный секрет доступа;
- Gemini API credentials, service account private keys, App Check debug secrets, signing keys — секреты;
- local Core device tokens — другая система доверия, не Firebase API key.

Самостоятельный хост может использовать свой Firebase project. Managed billing требует отдельной авторизации и квот на owner/core/device; нельзя отправить всем пользователям один общий секрет.

### 11.5. Перенос OpenAI Decisions

Порядок изменения:

1. Вынести `COMMON_INSTRUCTIONS`, версионирование evidence и medical guards из OpenAI transport.
2. Зафиксировать provider-neutral JSON schema для fire и medical assessments.
3. Написать decoder нового ответа: unknown enums, лишние поля, missing answers, malformed JSON, refusal/safety block, пустое изображение, mismatch evidence.
4. Ввести `FirebaseDecisionProvider` bridge и честный mock с `semantic_evaluation=false`.
5. Удалить исполняемые references к `/decisions`, `gpt-6-luna`, `OPENAI_API_KEY`, OpenAI-specific answer types, token accounting и ветке config `provider=openai`.
6. Переписать provider tests на общие golden cases. Старые медицинские policy tests должны остаться независимо от новой модели.
7. Usage считать по реальным данным SDK; когда данных нет — `unknown/estimated`, а не выдуманные measured tokens.
8. Один слой retries. Deadline, `Retry-After`, jitter, circuit breaker и бюджет запроса контролируются централизованно. Ошибочный/устаревший ответ никогда не превращается в `NO_ALERT`.

Исторические миграционные документы могут упоминать OpenAI; CI должен запрещать его в production dependency graph и runtime code path, а не тупо запрещать слово во всём репозитории.

### 11.6. Пакетирование и расходы

Различать три вещи: tensor microbatch локальной модели; несколько изображений одного episode в одном semantic request; bulk asynchronous Batch API. Последнее не является подходящим режимом срочного наблюдения.

Не объединять несвязанные камеры/людей в один семантический вопрос без строгой схемы ассоциаций. Для первой версии облачный пакет — **один incident**, хронология и source labels, ограниченный размер, отдельный overview/crop. Локальные тензоры разных камер можно batch-ить, сохраняя FrameKey для каждого результата.

Кеширование — только точное совпадение content/evidence/context/model/prompt hash. Старый ответ о контролируемом огне нельзя переносить на новый кадр только потому, что perceptual hash похож. Предусмотреть provider-wide budget и fair per-camera quota; временная экономия не должна замаскировать отсутствие проверки.

---

## 12. Подключение, pairing и удалённый доступ

### 12.1. LAN pairing

mDNS объявляет `_stillwatch._tcp.local` с instance ID и API version, **без токенов и паролей**. Discovery — подсказка адреса, не аутентификация. При запрете multicast остаётся manual URL/QR.

Первичная привязка:

1. Владелец запускает `stillwatch-cli pair` локально либо использует уже авторизованный admin-клиент.
2. Core генерирует одноразовый random pairing secret достаточной длины с TTL, например 5 минут; в БД хранится его hash.
3. QR содержит endpoint, `core_instance_id`, fingerprint identity/TLS public key, nonce и одноразовый secret. QR нельзя логировать или сохранять в telemetry.
4. Companion проверяет закреплённую identity, выполняет challenge, регистрирует device public key/identity и получает права, заданные владельцем.
5. Pairing secret уничтожается; доступ устройства индивидуально отзывается.

В native LAN-клиенте допустим self-signed TLS только с явным pin/fingerprint bootstrap. Никакого общего `verify=False`. Для браузерного worker лучше trusted localhost/HTTPS origin и отдельная scoped registration. Host/Origin validation обязательны, в том числе для localhost: незащищённый localhost API может быть атакован чужой страницей.

### 12.2. Авторизация

Роли: `owner` — устройства, сеть, секреты, глобальный config; `operator` — разрешённые камеры и incident actions; `viewer` — только разрешённое чтение. Дополнительные scope: `cameras:write`, `events:ack`, `evidence:read`, `streams:read`, `provider:evaluate`.

Short-lived access token и rotated refresh credential; long-lived secret хранится в системном secure storage компаньона. На Core refresh secrets хранить hash-ами. Refresh reuse detection, rate limiting pairing/login, device revocation и audit trail — обязательны.

Не строить собственную криптографию. Использовать поддерживаемые библиотеки и стандартные ключи/подписи. Истечение сертификата и смена identity требуют понятной процедуры; неожиданный другой Core по тому же IP не доверяется автоматически.

### 12.3. Remote: последовательность поддержки

**Первый надёжный профиль — private VPN.** Companion обращается к тому же API через доступный приватный адрес. Сам VPN конфигурируется владельцем инфраструктуры; приложение умеет проверить connectivity и объяснить ошибку. Автоматическое открытие RTSP/UPnP наружу не делаем.

**Второй профиль — HTTPS reverse proxy.** TLS terminates на доверенном узле, proxy forwarding принимается только от allowlisted proxy, request/body limits включены. Identity в gateway должна оставаться привязана к исходному Core.

**Третий профиль — optional relay.** Core поддерживает исходящее authenticated соединение, relay маршрутизирует control messages; WebRTC для видео отдельно использует ICE/TURN. Relay не обязан хранить кадры. Если TLS завершается на relay, нельзя рекламировать его как не видящий никакие данные без дополнительного реального шифрования payload.

Control connectivity и media connectivity диагностируются раздельно: работа HTTPS-туннеля ещё не означает, что WebRTC UDP прошёл NAT. Нет доступного remote route — live/команды показывают offline, а локальное наблюдение продолжает работать.

### 12.4. Сеть и SSRF

Для камер частные IP — нормальная необходимость, поэтому правило «запретить все private ranges» непригодно. Вместо этого владелец задаёт разрешённые camera CIDR/ports; отдельно запрещаются metadata endpoints, неожиданный localhost, служебные интерфейсы и cross-tenant ranges. Проверять IPv4/IPv6, DNS resolution и redirects; decoder не должен обходить проверку разрешением DNS на другой адрес. Реализовать сетевое ограничение процесса/контейнера там, где одного URL-validator недостаточно. [R18]

FFmpeg запускается аргументами без shell. Секреты не включаются в exception text, public source descriptors и диагностический export. Upload ограничен размером, временем, codec probe и ресурсами decoder; расширение файла не является полной проверкой содержимого.

---

## 13. Трансляции

Выбран MediaMTX как отдельный media router. WHEP/WebRTC используется для live-view; не пишем собственный транспорт с JPEG поверх WebSocket. MediaMTX поддерживает WHEP и различные кодеки, но клиентская поддержка должна быть проверена на каждой ОС. [R12]

Поток:

```text
Companion → POST /stream-sessions (camera/profile)
Core → authorization + media capability selection
Core → short-lived session grant
Companion → authenticated WHEP offer
MediaMTX ↔ ICE/STUN/TURN ↔ native WebRTC renderer
```

Grant ограничен камерой, stream profile, субъектом, временем и операцией read. Он не даёт доступ к MediaMTX admin API. При отзыве прав Core активно завершает соответствующие media sessions: истечение grant само по себе не обязано разорвать уже установленный WebRTC transport. Пароль камеры никогда не отправляется компаньону.

**Кодеки:** сначала выбрать совместимый camera substream. Если камера выдаёт неподдерживаемый вариант H.265/H.264, сделать один общий transcode на profile, не отдельный на viewer. Codec negotiation и required transcoding видны в `doctor`. Audio по умолчанию выключено; включение означает отдельные права и privacy handling.

**Fallback:** ограниченный snapshot/MJPEG для диагностики и сетей без working media route; на совместимых клиентах можно добавить HLS с явно большей задержкой. Failed stream не должен выглядеть как живая неподвижная камера: overlay `last frame ... ago` обязателен.

**TURN:** короткоживущие credentials, ограничение allocations и bandwidth, запрет open relay. Протестировать symmetric NAT, UDP blocked, TCP/TLS relay и смену Wi-Fi↔cellular. Не выдавать pass-through TLS-туннель за полный NAT traversal.

**Privacy:** при активных masks выбрать redacted encoded pipeline или запретить raw view данному actor. Нельзя маскировать только ML-кадр, одновременно показывая незамаскированный stream всем клиентам.

В Companion grid получает thumbnails/низкий профиль; только видимые tiles подписаны на live. Fullscreen использует повышенный профиль. В фоне live-потоки закрываются: push, а не невидимый WebRTC, отвечает за разрешённые уведомления.

---

## 14. API и SDK

### 14.1. Contract-first

Канонический контракт — `packages/contracts/openapi.yaml` и JSON Schemas. Для первой реализации выбрать OpenAPI 3.1.x с проверенной поддержкой генератора, а не объявлять старую версию «самой новой». Errors — RFC 9457 `application/problem+json`. [R16, R17]

Rust/serde DTO валидируются на границе; structural deserialization не заменяет semantic validation диапазонов, размеров, NaN/Inf и разрешённых переходов. Query/path/body limits задаются в axum/Tower отдельно для JSON, upload и SSE. Debug formatter не должен раскрывать secret fields. [R28]

Domain types не генерируются бездумно из UI. HTTP DTO адаптируются к domain commands; формат persistence отдельно версионируется. Generated clients и server validation должны проверяться одними fixtures.

### 14.2. Целевые endpoints

| Группа | Основные операции |
|---|---|
| Bootstrap | `GET /health/live`, ограниченный `GET /bootstrap/info`, `POST /api/v2/pairing/exchange` |
| Сессии | `POST /api/v2/sessions/refresh`, `DELETE /api/v2/sessions/current` |
| Capabilities | `GET /api/v2/capabilities`, `/health/ready`, `/status` |
| Камеры | `GET/POST /api/v2/cameras`, `GET/PATCH/DELETE /cameras/{id}` |
| Источники | `POST /source-probes`, `GET /capture-devices`, upload sessions/files |
| Сцена | `PUT /cameras/{id}/privacy-masks`, `/controlled-fire-regions` |
| Инциденты | `GET /incidents`, `GET /incidents/{id}`, `GET /incidents/{id}/evidence` |
| Действия | `POST /incidents/{id}/acknowledgements`, `/feedback`, `/resolution` |
| Звук | `POST /alerts/{id}/silence` с явной областью и duration |
| Live | `POST/DELETE /stream-sessions`, bounded snapshot endpoint |
| Конфиг | `GET/PATCH /settings`, `GET /providers/status` |
| Устройства | `GET /devices`, `PATCH /devices/{id}`, `DELETE /devices/{id}` |
| События | `GET /changes?cursor=...` как SSE, `GET /snapshot` |
| Операции | authenticated diagnostics, backup metadata, metrics |

Не делать произвольный `POST /execute`, shell command endpoint или remote arbitrary-code/plugin execution независимо от языка.

### 14.3. Semantics команд

Изменяющие команды несут `Idempotency-Key` и, где необходимо, `If-Match`/record revision. Ключ scope-ится по actor+operation; повтор с тем же body возвращает прежний результат, с другим body — conflict. TTL idempotency records должен покрывать предусмотренное offline/retry окно; клиент не повторяет древние опасные команды автоматически.

Атомарная транзакция: проверить auth/revision → изменить domain state → записать audit/change event → записать outbox intent → commit → вернуть authoritative revision. Endpoint не отвечает «успешно», если состояние осталось только в памяти.

Config PATCH сначала валидирует полную новую конфигурацию, затем применяет её с rollback-планом. Частичный success явно моделируется, а не возвращается как общий `200 OK`.

### 14.4. Events и восстановление подключения

Выбираем **REST для команд + SSE для foreground state changes**. WebSocket не обязателен для этих функций; использовать его для специальных двунаправленных задач только при необходимости.

```json
{
  "schema_version": "1.0",
  "stream_generation": "uuid",
  "sequence": "8123",
  "type": "incident.updated",
  "entity_id": "uuid",
  "entity_revision": "9",
  "occurred_at": "UTC timestamp",
  "payload": {"attention_level": "review"}
}
```

SSE `id` содержит generation+sequence. На reconnect отправляется Last-Event-ID. Delivery допускает повторы: клиент dedupe-ит event ID и применяет только более свежую entity revision. Пропуск старше retained change log даёт явный `cursor_expired` и полный snapshot. После DB restore generation меняется; старые cursors не переиспользуются.

Heartbeat и idle timeout конечны; backoff с jitter. Медленный клиент не удерживает бесконечную очередь: disconnect с инструкцией resync. Авторизация применяется к каждой subscription и evidence/media; revoke инвалидирует активную сессию. Изменение permissions приводит к resync scoped snapshot.

### 14.5. Публичные Client SDK, включая Rust

| SDK | Целевой контракт | Production dependencies |
|---|---|---|
| **Rust `stillwatch-sdk`** | Async client/builder, typed DTO/errors, resumable event stream | HTTP/TLS, serde/protocol; никаких ORT/FFmpeg/engine |
| **Python** | Sync и async clients, context managers | HTTP и модели данных; не FFI-binding Core |
| **Dart** | Основной SDK Flutter, Streams/cancellation/credential provider | Клиентская логика, без нативной ML-библиотеки |
| **TypeScript** | Worker/integration client, Fetch/AbortSignal | Browser/Node capabilities задаются явно |

Канонический OpenAPI/JSON Schema общий. Генерировать DTO/низкоуровневые endpoints там, где generator поддерживает необходимые формы; handwritten facade отвечает за pairing, reconnect, retry policy, cursor и credentials. Generator/tool versions закрепляются. Для Rust допустим небольшой ручной transport поверх generated types, но contract fixtures и drift-check обязательны: `serde` derive сам по себе не доказывает соответствие OpenAPI.

Пример желаемого Rust API — **эскиз, не реализованный/проверенный crate**:

```rust
let client = StillWatchClient::builder(core_url)
    .credentials(device_credentials)
    .build()?;
let capabilities = client.capabilities().await?;
let camera = client.cameras().create(request, operation_id).await?;
let mut changes = client.changes().resume(cursor_store).await?;
while let Some(change) = changes.next().await {
    handle_change(change?).await?;
}
```

Rust SDK использует async reqwest client и connection pooling. Optional `blocking` module доступен явно, не создаёт скрытый runtime при import и не вызывает nested `block_on` из async context. Blocking reqwest нельзя просто вызывать внутри Tokio runtime; async callers должны использовать async API. [R33]

**Сохранить требования всех SDK:** конечные connect/read/total deadlines; streaming idle timeout; idempotent retries с `Retry-After`; typed auth/revision/capacity/rate/provider/cursor/transport errors; unknown enum/fields handling; no cross-origin bearer redirect; нормализация base URL; bounded downloads; stable cursors; redacted logging. Одна retry policy на запрос, не умножение попыток SDK×gateway×worker.

Credential provider — интерфейс, поддерживающий refresh single-flight; конкретный Keychain/Keystore/desktop secret store находится в приложении. SDK не пишет секреты в случайный локальный JSON. В TLS policy нет `insecure_skip_verify` как стандартного решения pairing.

Для SSE Rust facade должен правильно обрабатывать chunk boundaries, CRLF, multi-line `data`, UTF-8 границы, heartbeat, max event size, `Last-Event-ID`, reconnect jitter и explicit cancellation. Cursor сохраняется после принятия события клиентом согласно документированной delivery semantics; reconnect допускает duplicates. Snapshot bootstrap согласован с revision/cursor, иначе возможно окно потери события между GET snapshot и подпиской.

Public IDs/revision/sequence имеют одинаковое wire-представление во всех SDK; `u64` не превращается в JS `number` без проверки диапазона. Optional поля, enum fallback и error codes проверяются общими fixtures.

В первой версии Rust SDK async-first; Python sync/async остаются полезными другим разработчикам и **не делают Rust Core Python-приложением**. CLI/AlarmAgent используют Rust SDK вместо прямого доступа к private engine state. Версии API/SDK независимы; тестировать N↔N−1 в пределах объявленной матрицы.

### 14.6. Plugin SDK — отдельный слой

Для расширения Core определить стабильные интерфейсы `SourceAdapter`, `PerceptionBackend`, `SemanticProvider`, `NotificationTransport`, `HazardPolicy`. У них разные права и типы ошибок; единый «плагин может всё» запрещён.

В v1 bundled/trusted adapters реализуются Rust traits и компилируются вместе с выбранной версией Core. Для внешнего процесса — versioned JSON Schema/IPC protocol, deadlines, byte/memory limits, capability negotiation. Установка произвольного wheel, crate или динамической библиотеки через Companion запрещена.

**Rust trait API не является стабильным plugin binary ABI.** Не загружать чужой `.dll/.so` с `dyn Trait`, `Vec` или Rust layout через `dlopen` как будто они совместимы между compiler versions. Основной external SDK — процессный IPC. Если действительно потребуется C ABI, он оформляется отдельным versioned `extern "C"` интерфейсом с opaque handles, длинами, ownership/free правилами и запретом unwinding через границу. FFI требует отдельного review; это не задача первой версии. [R36]

Perception backend остаётся синхронной ограниченной операцией **в inference worker**; async provider bridge не заставляет native tensor call стать отменяемым. Rust plugin SDK не отдаёт raw mutable доступ к DB, secret store и глобальному IncidentManager.

Контракт provider-neutral не означает, что конкурсная сборка использует любые семантические API: runtime allowlist у неё Firebase-only, плюс mock для честно отмеченных тестов и отдельно разрешённый статус YOLO через G1.

---

## 15. Компаньоны

### 15.1. Единый функциональный объём

Все четыре платформы имеют: список Core; LAN discovery/manual URL/QR; connection health; camera list/grid/fullscreen; создание/редактирование/удаление источников; scene/privacy editor; settings; история и evidence инцидентов; live updates; acknowledge/false alarm/resolve/silence; device/access management согласно роли.

На первом запуске нет предположения, что Core находится на том же устройстве. Desktop может управлять NAS без локальной установки моделей. Компонент Core устанавливается отдельно или выбирается в installer; Companion не стартует скрытый второй Core автоматически.

Multi-Core поддерживается в UI отдельными profiles с независимыми credentials и состояниями. События всегда маркируются `core_id + camera_id`; одинаковые имена камер не создают коллизий.

### 15.2. Модули Flutter

```text
features/onboarding      discovery, QR, trust verification
features/core_profiles   saved endpoints, health, reconnect
features/cameras         CRUD, capacity, per-camera settings
features/live            media sessions, decoder, visibility lifecycle
features/incidents       timeline, evidence, operator actions
features/alerts          foreground sound, push actions, permissions
features/settings        roles, privacy, providers, retention
platform/                notification, keychain, discovery, lifecycle bridges
```

Клиентское network/view state живёт в shared Dart SDK/repositories, не в widget callbacks; authoritative domain state — исключительно в Rust Core. В Flutter не переносим fire/fall reducers и не встраиваем Rust inference crate через FFI. UI отображает серверные revision/status и не может локально «погасить пожар» изменением boolean без команды Core.

### 15.3. Права и платформенные особенности

iOS: описание доступа к LAN/Bonjour, разрешение уведомлений, Keychain, native notification categories/actions, проверка Critical Alerts capability. Android: runtime notification permission по target OS, channels, secure storage, настройки power restrictions и full-screen intent capability без предположения, что permission выдано. [R09, R10]

Windows/Linux: native notification integration + opt-in resident AlarmAgent. Закрытие главного окна не обязательно означает выход агента; UI должен ясно различать «закрыть окно», «остановить агент» и «отключить оповещения». Не оставлять непонятный процесс без настройки.

Нативное отображение видео поддерживает `flutter-webrtc`, но конкретная версия и зависимости проверяются на реальных целевых ОС. Не подключать Firebase AI plugin, TFLite, ONNX, Torch или model assets к Companion. Build audit проверяет dependency graph и содержимое APK/IPA/desktop bundle. [R14]

### 15.4. Offline UX

В offline остаются только ограниченные cached metadata, timestamps и настройки профиля. Камеры помечены unavailable; cached кадр не выглядит live. User action хранится как `pending`, а не как подтверждённое глобальное действие. После reconnect команда с истёкшим сроком/изменившейся revision не применяется без проверки.

Кеш evidence по умолчанию выключен либо короткоживущий; при logout/device revoke очищается. Preview на lock screen имеет настраиваемый privacy level; push не содержит фотографию детской по умолчанию.

---

## 16. Алерты, сирены и доставка

### 16.1. Раздельные состояния

```text
Assessment: unknown / no_visible_concern / concern / immediate_visible_concern
Incident lifecycle: open / resolved
Attention: info / review / urgent
Operator acknowledgement: unacknowledged / acknowledged
Delivery: queued / provider_accepted / client_seen / failed / expired
Audible state: ringing / silenced_until / stopped   (device-local)
```

Операторский ACK означает «увидел», а не «опасность исчезла». `false_alarm` — feedback с причиной и evidence version. `resolved` — отдельная команда с авторизацией. `silence` имеет scope, deadline и правила повторной эскалации. Новый существенный эпизод не должен навсегда наследовать mute старого.

### 16.2. Durable outbox

`incident transition + change event + notification intent` создаются одной транзакцией. Dispatcher делает at-least-once delivery; идентичный `alert_id/version/channel/device` dedupe-ится. Нельзя обещать exactly-once push.

ACK от push-провайдера означает, что провайдер принял запрос, не что телефон запищал. `client_seen` — отдельное сообщение приложения; `user_acknowledged` — отдельная команда. В истории отображаются эти различия.

Retry имеет deadline/TTL, backoff, лимит попыток и rate budget. После восстановления связи не отправлять пачку давно resolved alarms как новые. При этом важный неразрешённый инцидент не теряется: клиент получает актуальный state, а не устаревшие команды звука.

### 16.3. Каналы

| Канал | Реализация | Ограничение |
|---|---|---|
| Foreground Companion | SSE/state + локальный звук | Требует работающего клиента и разрешённого аудио |
| Desktop background | Отдельный user-session AlarmAgent | Без вошедшего пользователя звук не обещается |
| Host hardware output | Отдельный поддерживаемый адаптер | Проверяется реальное устройство; нет скрытого обещания динамика у NAS |
| iOS background | Разрешённый APNs/посредник, categories/actions | OS policy, entitlement, сеть, пользовательские настройки |
| Android background | Разрешённый push-transport + notification channel | FCM contractual gate, OS restrictions, user settings |
| Remote escalation | Адаптер после отдельной оценки условий/надёжности | Не включать автоматические звонки экстренным службам в базовую версию |

Firebase Cloud Messaging не является обязательным следствием использования Firebase AI Logic. В конкурсном/демо-профиле можно демонстрировать локальную реакцию и разрешённые тестовые уведомления, не выдавая их за допущенный канал реального life-critical применения. [R07]

### 16.4. iOS и Android: что не обещаем

Apple Critical Alerts требует отдельного entitlement и пользовательского разрешения. Подписка разработчика сама по себе этого не даёт. Без entitlement — доступные обычные/Time Sensitive возможности согласно фактическим разрешениям. Нельзя гарантировать бесконечную сирену при закрытом приложении, обход Focus или удалённую мгновенную остановку уже проигранного/поставленного в очередь звука. [R09]

Android full-screen intent ограничен системной политикой и разрешениями; отдельно проверяется право его использовать, а не автоматически объявляется приложение alarm clock. При недоступности — корректное high-importance notification без притворного обхода ОС. [R10]

Минимальный payload: `core_id, incident_id, alert_id, revision, expires_at, severity`, без credentials/видеокадров. Клиент проверяет freshness и получает authoritative state перед выполнением потенциально устаревшего действия.

Действие из push при недоступном Core может быть queued в gateway только с identity, сроком и idempotency key. UI сообщает «ожидает Core». Gateway не присваивает `resolved` самостоятельно.

### 16.5. Health monitoring

Отдельные технические события: camera offline/frozen; inference stopped; semantic worker unavailable; budget exhausted; disk almost full; notification permissions disabled; stale Core heartbeat; invalid clock; model integrity failure.

Полностью умерший Core не может сам отправить сообщение о своей смерти. Для remote host-offline detection требуется независимый heartbeat monitor; для полностью локальной установки — внешний watchdog/устройство либо честное ограничение. Не выдавать health dashboard подключённого клиента за независимый watchdog.

---

## 17. Хранение, приватность и восстановление

### 17.1. Схема данных

```text
core_identity, schema_migrations
users_or_principals, devices, sessions, pairing_challenges
cameras, camera_config_versions, secret_references, scene_regions
incidents, incident_observations, assessments
artifacts, artifact_references, evidence_packets
change_events, operator_actions, idempotency_records
notification_outbox, notification_attempts, device_receipts
provider_calls, budget_reservations
health_events, model_manifests
```

Часто фильтруемые поля (`camera_id`, status, time, severity) — нормальные индексируемые колонки, не только большой JSON. JSON подходит для versioned details. History pagination идёт по устойчивому `(created_at,id)` cursor.

SQLite WAL хранится на локальном filesystem хоста Core, не на произвольном SMB/NFS-mount. NAS-контейнер с локальным volume на самом NAS — другая ситуация, чем БД, которую несколько хостов одновременно открывают по сети. [R15]

### 17.2. Evidence consistency

Запись blob: temporary file → flush при выбранной durability policy → atomic rename → metadata transaction. Crash между этапами создаёт orphan, который startup reconciliation обнаруживает. Metadata не должна ссылаться на незавершённый файл как на готовый. Для AUD-05 `save_packet` возвращает типизированное `persisted / partial / failed`, а не boolean, игнорирующий DB error.

Content-addressed blob допустим для dedup, но reference counting и retention должны учитывать все incidents/versions. Deleted/expired evidence остаётся отражённым в истории как unavailable; не подменять его другим кадром.

Глобальные budgets: evidence bytes, pinned bytes, DB/WAL size, uploads, logs, temporary transcode files, backup space. Старый quota только на JPEG не покрывает всё дисковое использование.

### 17.3. Privacy-by-default

Raw continuous recording выключена. Сохраняются bounded ring и event evidence согласно настройкам. Cloud отправка включается отдельно по владельцу/камере; до неё применяются необходимые masks. Нельзя одновременно обещать скрыть все лица и сохранить максимальную точность pose — влияние масок показывается как coverage limitation.

Пароли камер хранятся за secret reference. На Windows — подходящее machine/user secret protection; Linux/NAS — закрытые file permissions + явный encrypted-secret механизм или mounted secret. Шифрование ключом рядом с данными не защищает от администратора этого хоста; модель угроз должна это честно признавать.

Нет лицевой идентификации, персональных имён людей на камере или скрытой telemetry с изображениями. Diagnostic export redacts URLs/tokens/device identifiers; кадры включаются только явным действием владельца.

### 17.4. Backup / restore / upgrade

Backup создаётся согласованным SQLite backup-механизмом плюс manifest immutable blobs; не копировать только `.sqlite3`, забыв активный WAL. Encryption keys и instance identity требуют отдельного защищённого backup.

Restore проходит в новой директории, валидирует schema/artifact hashes и права. После переключения запускается reconciliation; streams generation меняется; политика refresh-token revocation после restore определена явно.

Перед миграцией — preflight, место под backup и rollback plan. Downgrade бинарника без совместимой БД запрещён. При восстановлении из backup настройки cloud consent и notification destinations не сбрасываются в более открытый режим.

## 18. Оптимизация и фактически выполненные эксперименты

### 18.1. Что выполнено в предыдущем аудите; не Rust-результаты

В Rust-архиве предыдущие материалы находятся в `audit/legacy-python/`: `audit_harness.py`, `results.json`, полный вывод и оригинальный `buffer.py`. Их численные результаты ниже перенесены из предыдущего аудита, не измерены повторно для Rust. Git blob SHA-1 скопированного `buffer.py` проверен и равен upstream `7ab610ef8a085edb2d71ec9215320e0e81d6e918`.

**24 проверки успешно выполнены:** 8 проверок исходного buffer и его происхождения; 4 воспроизведения ограничений/дефекта старой логики; 8 проверок предлагаемых policy guards на синтетических фактах; 4 проверки модели очереди. Прохождение counterexample-теста означает, что проблема воспроизведена, а не что исходный проект уже исправлен.

Среда: Linux x86_64, Python 3.13.5, 5 доступных logical CPUs, OpenCV 4.13.0, NumPy 2.3.5. Это **не** целевая среда исходного проекта Python 3.12. Использовались isolated components и структурные fixtures; полный пакет StillWatch не устанавливался и его полный pytest не запускался.

### 18.2. Воспроизведённые наблюдения

1. Два fire observations score `0.3` с одинаковой рамкой и разницей **3600 секунд** в одном epoch всё равно создают candidate при втором вызове. Нет проверки age между вызовами. Caller reset на смене epoch в старом runtime существует; этот тест не утверждает обратного.
2. При `fire_immediate_confidence=0.9` observation score `0.7` после повторения получает priority `3` из-за литерала `0.65`.
3. Контролируемый огонь и опасный огонь с одинаковыми detector observations неразличимы для текущего `FireRules`. Это проверка отсутствующего входного контекста, **не** измеренная частота ложных пожарных тревог.
4. При настоящем SQLite write failure (`PRAGMA query_only=ON`) извлечённый `save_packet` возвращает `True`, JPEG остаётся на диске, metadata rows — **0**, `metadata_write_failed` установлен. Исправление должно учитывать оба слоя сохранения.

### 18.3. JPEG/quality microbenchmark

Синтетические кадры 1280×720, JPEG quality 80, один поток OpenCV. 120 входных кадров на повтор, 3 повтора. Сравнивался одинаковый preprocessing/quality/JPEG этап: каждый кадр против encode только каждого шестого кадра. Hash-ы выбранных JPEG совпали.

| Показатель | Каждый кадр | Предварительный sampling 1/6 |
|---|---:|---:|
| JPEG encodes на повтор | 120 | 20 |
| Медиана времени этапа | 0.8483 s | 0.1511 s |
| Относительное ускорение этапа | 1× | **5.62×** |

**Интерпретация:** перенос encode после решения о потребности способен убрать существенную лишнюю работу. Это не ускорение всего Core в 5.62×. Не измерялись video decode, YOLO, качество детекции, реальные камеры, мобильные клиенты или облако. В штатном коде preprocessing надо дополнительно разделить: privacy/sanitization для анализа, quality sampling и JPEG не обязаны иметь одинаковую частоту.

### 18.4. Синтетическая модель перегрузки очереди

Условия: 8 камер × 10 входных кадров/с, 30 секунд, искусственная service time **50 ms** на задание. Это заданная стоимость для моделирования, не результат измерения YOLO.

| Показатель | Общая FIFO | Per-camera latest + round-robin |
|---|---:|---:|
| Входных кадров | 2400 | 2400 |
| Обработанных | 600 | 600 |
| Pending к концу | 1800 | 8 |
| p95 возраста кадра **до обработки** | 21 337.5 ms | 87.5 ms |
| Обработано на каждую камеру | 75 | 75 |
| Заменено необработанных старых кадров | 0 | 1792 |

Это аргумент в пользу bounded freshness, не доказательство сохранения recall при пропуске кадров. Текущий single-camera захват уже использует latest sample; сравнение показывает, **как не испортить это свойство при новой многокамерной очереди**, а не наличие FIFO в старом runtime.

### 18.5. Rust-оптимизация: порядок и ограничения

1. **Сначала эквивалентность, затем скорость.** Зафиксировать Python baseline, выполнить ONNX FP32 parity, затем сравнить Rust pipeline на тех же observations/frames. Исправления fire policy тестировать отдельным изменением, не маскировать их как «разницу языка».
2. Измерить decode, colour/resize, tensor conversion, ORT, postprocess, tracking, encode, DB и cloud отдельно. Отчёт: p50/p95/p99 latency, event delay, RSS/VRAM, allocations/bytes copied, thread count, power/CPU idle, а не только средний FPS.
3. Убрать JPEG для ненужных кадров, повторные DB/directory scans, model-per-camera и сериализацию raw frames через JSON. Это алгоритмические изменения; их выигрыш не приписывать Rust-компилятору.
4. Reuse input tensors и preallocated buffers с верхними пределами. Batch results сохраняют FrameKey/order; async поздний ответ не применяется к более новой сцене. Не форсировать unsafe/lock-free код, пока профиль не показал проблему.
5. **Общий thread budget:** Tokio I/O + capture/libav + ORT sessions + preprocessing/JPEG + storage. Настроить ORT intra/inter-op и spinning под измеренный hardware profile; «по всем ядрам на каждую камеру» — не оптимизация. При необходимости global ORT pool проверяется на конкретной связке API. [R19]
6. CPU FP32/batch=1 — эталон. GPU batch 1/2/4/8 измеряется с queue-age deadline, actual cadence и fairness. FP16/INT8/TensorRT — отдельные model/runtime artifacts с собственным parity/eval gate; quantization data не берутся из final test. [R20, R30]
7. Model outputs обрабатываются в Rust с проверенной геометрией/score/NMS; не применять один postprocessor ко всем YOLO. Отдельный тест near-threshold cases важнее красивого average tensor error. [R31]
8. Shared memory, GPU I/O binding/zero-copy, SIMD и allocator replacement вводятся только после measured bottleneck и memory-lifetime tests. Использование `Arc` не доказывает отсутствие копий; заявлять zero-copy только для конкретного проверенного hop.
9. Cloud savings — event-driven evidence/exact cache/per-camera fairness. Live — shared remux/transcode и visible tiles. Rust не уменьшает сетевую задержку Firebase или системную задержку push сам по себе.

Сравнение A/B: одинаковые веса, разрешение, cadence, число камер, threads/backend, test inputs и policy version. Отдельно показывать **language/runtime port**, **backend PyTorch→ORT** и **pipeline optimisation**. Иначе невозможно понять причину выигрыша или регрессии.

### 18.5.1. Release profile и tooling

Стартовый проектный профиль, который проверяется benchmark-ом, а не объявляется оптимальным:

```toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
debug = "line-tables-only"
overflow-checks = true
```

Символы/debug artifacts сохраняются отдельно для crash analysis. `panic=abort` не включать глобально ради меньшего файла без анализа recovery boundary; ожидаемые ошибки всё равно `Result`. `target-cpu=native` допустим для личного benchmark, **не** для универсального release на разные ПК/NAS. Точный CPU baseline и optional accelerated variants отражаются в build manifest. Cargo profile options — [R24].

Criterion/обычные controlled benchmarks — для чистых hot functions; `tracing` spans и OS profiler — для полного конвейера. Property tests проверяют invariants, fuzzing — parsers/timestamps/geometry/commands; Miri применим к подходящим pure-Rust модулям, не выдаётся за проверку ORT/CUDA. Конкретные dev tools закрепляются после проверки совместимости compiler/target.

### 18.6. Память и загрузка: формулы вместо обещаний

Приближённый compressed ring budget:

```text
ring_bytes ≈ cameras × seconds × evidence_fps × mean_jpeg_bytes
```

Пример планирования, не измеренный размер кадров: `32 × 30 × 5 × 60 KiB ≈ 281.25 MiB`. К этому прибавляются модели, decoder buffers, raw latest frames, pinned evidence и служебные структуры. Для raw 1080p BGR один кадр занимает примерно 5.93 MiB; 30 секунд × 10 FPS — около 1.74 GiB **на камеру**. Поэтому raw multi-second ring по умолчанию не подходит.

Нагрузка serial inference worker:

```text
utilization ≈ Σcamera(pose_fps × pose_inference_seconds
                    + fire_fps × fire_inference_seconds)
```

Она должна иметь запас для jitter, evidence и пиков; значение выше 1 означает физическую перегрузку. Например, при предположении 40 ms pose и 10 ms fire восемь камер с 5/2 FPS требуют `8 × (5×0.04 + 2×0.01) = 1.76` секунды вычислений на секунду одного serial worker. Это демонстрация расчёта, не benchmark конкретных моделей.

Общее время до alert раскладывается на sampling wait + queue age + local inference + temporal policy window + optional semantic request + persistence + transport. Push delivery может не иметь гарантированной верхней границы; его нельзя включать в обещанный hard real-time SLA.

### 18.7. Что здесь не проверено

Нет измерений нового Rust Core, compiled Rust worker, ONNX export parity, реального YOLO FPS/recall/FAR, GPU/NAS capacity, Firebase model quality, APNs/FCM доставки, WebRTC на телефоне, iOS signing и 24/7 soak. Не было целевых весов/размеченных роликов/credentials/устройств для этих тестов. В документе они перечислены как обязательные шаги реализации, а не представлены выдуманными результатами.

---

### 18.8. Проверки этой Rust-редакции

В этой итерации проверяются целостность Markdown, наличие всех P00–P17, согласованность Rust-архитектуры/packaging/SDK и отсутствие активных указаний ставить Python-Core/PyInstaller. Старый audit bundle перенесён с сохранением содержимого исходных отчётов и fixtures; новый архив имеет свой SHA-256 manifest.

**Новый Rust-код и Rust performance tests здесь не выполнялись.** Rust toolchain в текущем runtime не обнаружен; полноценный Core ещё только описан планом. Нет оснований утверждать коэффициент ускорения от смены языка. Синтетические Python-числа 5.62× и очередь 87.5 ms остаются только историческими ограниченными экспериментами из 18.1–18.4.

---

## 19. Проверка качества моделей и системные тесты

### 19.1. Данные и метрики

Собирать dataset manifest с source/license/consent, scene ID, camera ID, interval labels, hazard onset, visible escalation, controlled context, recovery, visibility и разрешением на cloud processing. Личные домашние кадры не должны автоматически попадать в public fixtures.

Два уровня eval:

- **Perception:** detection/pose error, маленькие объекты, lighting/crop conditions, backend parity.
- **Event pipeline:** event recall, false alerts на camera-hour, delay от видимого onset, доля unknown, дубли incident, корректность resolution и notification policy.

Основная метрика ложных тревог — не «accuracy по кадрам» на почти полностью спокойном видео. Считать false urgent/review отдельно, на длительных negative sessions. Публиковать число часов, число независимых сцен и доверительный интервал. Ноль ложных событий на двух коротких клипах не доказывает пригодность к 24/7.

Dataset split — по месту/эпизоду/камере, с отдельным night/IR и controlled-fire holdout. Threshold tuning и calibration выполняются на validation; final test не используется для повторного подбора. Для сравнения версий использовать одинаковый набор и bootstrap по эпизодам, а не считать все соседние кадры независимыми.

**Заранее заданный приёмочный принцип:** уменьшение false-alert rate не принимается, если оно достигнуто неприемлемой потерей event recall или ростом detection delay. Численные допустимые границы фиксируются после baseline и анализа риска; документ не выдумывает «99.9% точности» без данных.

### 19.2. Обязательная тестовая матрица

| Группа | Сценарии |
|---|---|
| Domain | stale packet, duplicate response, mismatched epoch, ACK≠resolved, repeated falls, cloud unavailable≠safe |
| Cameras | 1/4/8/16/32 configs; часть выключена; медленная/битая камера; reconnect; смена разрешения |
| Capture | PTS повтор/регресс, файл ended/loop, USB unplug, RTSP authentication fail, frozen video |
| Inference | missing/corrupt ONNX, native DLL/version mismatch, OOM, worker crash/hang, timeout≠cancel, backend/tensor/event parity, thread oversubscription |
| Controlled fire | свеча/плита/камин, выход за зону, дым, reflection/screen, shifted scene |
| Medical | старые regression fixtures + bystander, occlusion, deliberate activity, partial recovery |
| Provider | actual local/cloud route, malformed JSON, refusal, empty content, 429/5xx, invalid App Check, exhausted budget |
| Storage | disk full, DB locked/read-only, orphan blob, crash before/after commit, retention, backup/restore |
| Security | wrong role, expired/reused pairing, device revoke, DNS rebinding, SSRF, Origin/Host spoof, upload bomb |
| SDK | Rust/Python/Dart/TS contract parity, Python sync/async, optional Rust blocking isolation, generated diff, cursor expired, duplicates, reconnect, unknown enum |
| Rust invariants | `Send/Sync` boundaries, cancellation under load, checked size/time arithmetic, bounded channel + held-buffer bytes, malformed IPC/DTO, sequence precision, no unsafe outside reviewed adapters |
| Media | unsupported codec, no UDP, TURN only, revoked session, slow viewer, masks, reconnect |
| Native alerts | foreground/background/terminated, denied permissions, Focus/DND, silent mode, force-stop, battery restrictions |
| Distributed state | ACK с двух устройств, delayed action, stale push, Core offline, relay reconnect |
| Packaging | clean install, service autostart, update/rollback, uninstall keeps data unless explicit, no secrets |
| Soak | idle + burst episodes + viewer churn + cloud outages + memory/disk monitoring |

Нужны реальные iPhone/Android и Windows/Linux машины. Эмулятор полезен, но не доказывает поведение заблокированного телефона, системного звука или camera drivers.

### 19.3. Предлагаемые инженерные бюджеты

Это стартовые acceptance targets для тестового профиля, а не уже подтверждённые характеристики продукта:

- API read/control p95 до 250 ms на LAN при заявленной нагрузке, без media body.
- Устойчивый bounded queue и отсутствие обработки заведомо просроченных кадров.
- Per-camera actual inference cadence не ниже выбранного validated minimum; при невозможности — видимая деградация.
- Local pipeline продолжает работать при выключенных Companion и недоступном Firebase.
- Ни одного небезопасного перехода `unknown/unavailable → safe` из-за transport error.
- Idle RSS после warmup стабилен; проверяем отсутствие монотонного роста и превышения memory budget, а не абсолютную одинаковую память на разных backends.
- 24-часовой обязательный soak для заявленного hardware profile; расширенный 72-часовой перед заявлением о длительной автономности. Сам по себе soak не является сертификацией надёжности.

---

## 20. Поэтапная реализация

Все 18 этапов пересмотрены под Rust. Это план работ, не список уже готовых компонентов. Номер/название этапа недостаточны для закрытия: требуется указанное техническое доказательство.

### P00 — проверить Rust/native/Firebase границы

**Сделать:** собрать минимальный Rust binary на Windows x64/Linux x64; загрузить тестовый ONNX через выбранную `ort`/ORT связку; проверить libav capture с PTS и decoder timeout. В изолированном Python exporter восстановить locked pose/fire graph, экспортировать CPU FP32 и проверить layouts. Отдельно реальные Node/Web Firebase SDK requests с App Check, actual local/cloud route; Flutter WebRTC smoke на четырёх платформах. Зафиксировать G1–G5.

**Артефакты:** ADR runtime boundaries, точные toolchain/crate/native/JS версии, matrix OS×backend, первый export/parity report и redacted logs.

**Готово когда:** две существующие модели запускаются в Rust без Python runtime; output interpretation доказана на fixtures; выбран viable Firebase bridge. Незакрытые Firebase/правовые gates не мешают независимому Core, но блокируют соответствующие заявления о готовности. Если export неэквивалентен, исправить exporter/graph, а не оставлять скрытый Python-worker в готовом продукте.

### P01 — Cargo workspace, контракты и golden baseline

**Сделать:** структура раздела 5, `baseline.json`, точный `rust-toolchain.toml`, `Cargo.lock`, MSRV/edition/resolver; сохранение лицензий/attribution. OpenAPI v2/JSON Schemas, wire types, typed errors и version compatibility. Снять canonical observation/expected-event traces со старого Python baseline в отдельной среде; сохранить provenance и intentional-difference registry.

**Артефакты:** compiling Rust skeleton, контрактные fixtures, offline oracle, CI fmt/clippy/unit/schema/dependency checks. Конкретные tool versions закреплены, не `main/latest`.

**Готово когда:** чистая сборка воспроизводима; domain не тянет Tokio/ML/HTTP; SDK не тянет engine; генерация DTO не дрейфует; Python exporter/oracle отделён от release dependency graph.

### P02 — Rust domain и transactional storage

**Сделать:** чистые reducers для incidents/actions, отдельные ACK/resolve/silence; rusqlite dedicated writer, migrations, revision/idempotency/change log/outbox; типизированный результат evidence persistence. Исправить AUD-05, добавить reconciliation, quota accounting, backup/restore internals.

**Зависит от:** P01.

**Готово когда:** crash/failure injection перед/после DB commit не создаёт ложного success; read-only/disk-full сохраняют uncertainty; retry команды не дублирует intent. Domain tests не требуют camera/cloud/async runtime.

### P03 — Rust service и supervised workers

**Сделать:** `stillwatch-core`/CLI, Tokio runtime только для I/O; task ownership/cancellation, Windows SCM/systemd/Docker, instance lock, boot/readiness, watchdog; bounded child stdout/stderr; process generations и kill/wait cleanup. Local API пока loopback, до P08.

**Зависит от:** P02.

**Готово когда:** service переживает reboot без GUI/Python; worker crash/restart не гасит API; hung native process реально завершается; незавершённая работа не размножается после timeout. Тестировать process-tree cleanup и отсутствие orphan children.

### P04 — многокамерный Rust capture и буферы

**Сделать:** durable camera CRUD/probe, USB stable identity, RTSP/file capture adapters, отдельные epochs/PTS/scene revisions. Bounded native IPC, latest slots, global raw/evidence byte budgets, captured timing/quality и reconnect. Сохранять model-independent privacy transform.

**Зависит от:** P03/P00.

**Готово когда:** четыре synthetic sources параллельно проходят timestamp/epoch/reconnect tests; отказ одного не влияет на остальные. Worker не способен заставить Core выделить память по невалидному dimensions/length. File replay явно non-live; конфиги сохраняются.

### P05 — ONNX inference и перенос medical logic на Rust

**Сделать:** production ONNX manifest/loader, Rust preprocessing/postprocessing **для каждой модели**, shared sessions по device, fair scheduler. Переписать `tracking.py`/`motion.py`/medical rules в Rust; выполнить numerical, observation и event parity. Старый safetensors loader используется только в isolated export/oracle.

**Зависит от:** P04/P00/P01 fixtures.

**Готово когда:** acceptance раздела 22.2 выполнен; тот же человек/episode правильно определяется в legacy scenarios; no cross-camera track merge; Python/Torch не нужны на clean target. Near-threshold output differences разобраны, не замаскированы общим tolerance.

### P06 — Rust fire episode engine и evidence

**Сделать:** AUD-03/AUD-04, controlled-region context, elapsed-time continuity, growth/spread/smoke facts, camera-movement invalidation, immutable packets и coverage. Known bad legacy FireRules behaviour пометить intentional fix; сохранить medical parity.

**Зависит от:** P05/P02.

**Готово когда:** controlled flame не равен автоматическому urgent, но выход за зону/распространение не подавляются. Gaps, poor visibility, stale/unknown provider не означают safe. Полный candidate→incident→outbox путь покрыт детерминированными Rust tests.

### P07 — Rust↔Firebase bridge, без OpenAI runtime

**Сделать:** Rust adapter + official JS SDK workers по P00; bounded HTTP/IPC, auth/capabilities, App Check, response schema, immutable request binding, quotas/deadlines/circuit breaker и route telemetry. Удалить OpenAI transport/config/dependencies/secret references из production.

**Зависит от:** P06/P00.

**Готово когда:** actual permitted local/cloud image calls подтверждены; late/malformed/duplicate replies не повреждают state; JS worker может перезапуститься независимо. Никакой подмены Firebase AI Logic прямым REST из Rust; G1 остаётся явным external gate.

### P08 — LAN trust, pairing и RBAC

**Сделать:** axum API/Tower limits, registered device identities, QR secret+fingerprint, TLS/pinning и public-CA profile; refresh/revoke, camera scopes, Host/Origin checks, SSRF/network restrictions. Только после этого разрешить LAN bind.

**Зависит от:** P03/P04/P02.

**Готово когда:** viewer не меняет config; unauthenticated client не видит evidence; device revoke закрывает активный доступ. Credentials не появляются в Rust `Debug`, logs, process argv с неограниченным доступом или error body. Native decoder не обходит SSRF policy.

### P09 — media plane под управлением Rust

**Сделать:** MediaMTX lifecycle/auth, shared upstream, WebRTC/WHEP sessions, codec profiles, shared transcoding, raw/redacted ACL, thumbnails и fallback. Revocation закрывает уже действующие media sessions.

**Зависит от:** P08/P04.

**Готово когда:** несколько viewers не размножают ML и capture upstream; медленный viewer не блокирует detection; masks не обходятся pass-through потоком; отсутствующий live имеет stale indicator. Media state не хранится исключительно в UI.

### P10 — Rust/Python/Dart/TypeScript SDK

**Сделать:** generated DTO/endpoints и ручные facades четырёх SDK; Rust async client/optional blocking, typed errors, capability checks, resumable SSE, idempotency/retry, bounded streams и common contract fixtures.

**Зависит от:** P01/P08/P09.

**Готово когда:** shared fixtures одинаково трактуются всеми SDK; u64/revision не теряет точность; SSE chunking/reconnect/cancellation тестируются. Rust SDK dependency tree не содержит ORT/FFmpeg/engine; Python SDK не использует FFI Core. Plugin contract — отдельный documented IPC, не Rust DLL ABI.

### P11 — Companion desktop + Rust AlarmAgent

**Сделать:** общий Flutter shell, профили Core, discovery/manual/QR, camera CRUD/settings/zones, grid/live, incident timeline/evidence/actions, health. Rust AlarmAgent в пользовательской Windows/Linux сессии; user-visible opt-in.

**Зависит от:** P10/P09.

**Готово когда:** desktop управляет NAS без установки движка; закрытие UI не останавливает Core; действия имеют authoritative revisions; English поддерживается. AlarmAgent использует SDK, не открывает DB напрямую; model/runtime assets отсутствуют в Companion.

### P12 — iOS/Android без inference

**Сделать:** platform permissions, secure storage, LAN/Bonjour, foreground/background notification actions, lifecycle/network-change reconnect, limited video subscriptions, offline UX. Сборка iOS на macOS/Xcode runner.

**Зависит от:** P11 и разрешённый transport profile.

**Готово когда:** реальные устройства проходят live/pairing/actions, denied permissions/Focus/DND/force-stop корректно отражены. Ни Rust engine, ни ORT, ни Firebase AI worker не встроены в телефон ради конкурсного local inference.

### P13 — Rust alert delivery/outbox

**Сделать:** dispatcher, transport adapters, device receipts, ACK sync, silence/TTL/dedup/escalation, host health и hardware adapter contract. Explicit error taxonomy и redacted audit; FCM/gateway только согласно G3.

**Зависит от:** P02/P08/P11/P12.

**Готово когда:** provider accepted≠client seen≠user ACK; replay не включает сирену давно resolved события; дубли delivery не создают новый incident. Crash между send и status commit не нарушает at-least-once/dedup semantics; OS limitations сохранены.

### P14 — Rust remote gateway и эксплуатационные профили

**Сделать:** VPN/direct HTTPS doctor, optional authenticated outbound relay, TURN setup, quotas, separate heartbeat monitor. Основной gateway — Rust; optional Firebase token broker — официальный SDK за narrow authenticated interface.

**Зависит от:** P08/P09/P13.

**Готово когда:** control/media routes проверены раздельно; remote/NAT/UDP blocked/offline состояния корректны. Gateway не получает camera credentials и не выдаёт offline ACK за сохранённый Core state. Node broker не становится открытым token dispenser.

### P15 — measured Rust optimisation и quality gates

**Сделать:** reproducible baseline на 1/4/8/16 cameras; latency/RSS/CPU/VRAM/copy/thread profiles; controlled-fire hard negatives и медицинский holdout. Tensor reuse, lazy JPEG, bounded fairness; затем batching/FP16/INT8 и shared memory только по профилю.

**Зависит от:** P05–P14.

**Готово когда:** разделены выигрыш Rust, смена backend и pipeline redesign; recall/FAR/unknown/delay проверены для каждого профиля. Нет «ускорения», полученного снижением cadence без отражения деградации. Нет обещаний фиксированного FPS без hardware report.

### P16 — native устанавливаемые артефакты

**Сделать:** Rust release binaries + locked native libs/model packs, Windows Core/Companion installers, `.deb`/`.rpm`, CPU/GPU OCI profiles, Android APK/AAB и доступный iOS signed artifact. Checksums/SBOM/licenses, secure library load, upgrade/rollback/uninstall; никаких store uploads.

**Зависит от:** P11/P12/P15/G4/G5.

**Готово когда:** чистые Windows/Linux без Python/Cargo запускают Core; недостающий model pack/EP отражён в doctor; CPU package не требует CUDA. Service survives reboot; iOS signing limits описаны честно. Нет PyInstaller/Torch/Python runtime в Core bundle.

### P17 — сквозная приёмка Rust-версии

**Сделать:** E2E actual cameras/codecs/devices, all SDK contract tests, failure injection, backup/import/restore и upgrade. 24h soak validated profiles; extended 72h отдельно. Dependency/native provenance, no unsafe escapes, N/N−1 compatibility и EN technical instructions.

**Зависит от:** P00–P16.

**Готово когда:** requirement→Rust module/adapter→test→artifact matrix заполнена, sections 23 и G1–G5 имеют фактические evidence/status. Отсутствие теста не заменяется compiled-success или словами «Rust безопасный». Реальная точность и external gates остаются явно ограниченными, пока не проверены.

### Критический порядок

P00 параллельно проверяет **две независимые неопределённости**: Rust/ONNX/native перенос и Firebase platform integration. Далее P01–P06 дают локальную цепочку `source → Rust inference → Rust policy → committed incident`. Узкий read-only desktop/SDK smoke допустим раньше полного P10/P11. После этого довести trust/media/notifications и полный UI, не начиная с четырёх красивых пустых приложений.

---

## 21. Сборки и установочные артефакты

### 21.1. Матрица

Два Linux-пакета — **`.deb` и `.rpm`**, то есть форматы для разных семейств дистрибутивов, а не две архитектуры процессора. AppImage можно добавить как удобную переносимую упаковку Companion, но это отдельный опциональный артефакт.

| Цель | Core | Companion | Где собирается / что проверяется |
|---|---|---|---|
| Windows x86_64 | `stillwatch-core-<v>-windows-x64.msi` | `stillwatch-companion-<v>-windows-x64.exe` installer | Windows CI; SCM, GPU/CPU profiles, audio agent, VC runtime |
| Debian/Ubuntu x86_64 | `stillwatch-core_<v>_amd64.deb` | `stillwatch-companion_<v>_amd64.deb` | Linux CI + чистая VM/container для package smoke, реальный desktop для UI |
| RPM-family x86_64 | `stillwatch-core-<v>.x86_64.rpm` | `stillwatch-companion-<v>.x86_64.rpm` | Отдельная проверенная distro matrix и desktop session |
| NAS/Linux server | OCI/Compose CPU image; отдельно validated GPU image | Не нужен на NAS | Persistent volume, devices/permissions, local filesystem DB |
| Android arm64 | — | подписанный APK; AAB как дополнительный build artifact | Linux/macOS CI + emulator + физический Android |
| iOS arm64 | — | `.xcarchive` и подписанная под доступный профиль `.ipa` | macOS/Xcode CI + физический iPhone |

Linux ARM64/Core — расширение только после проверки Rust target, native ORT/FFmpeg/EP сборок, decoder и моделей; наличие target у rustc само по себе не доказывает работоспособность всей цепочки. Не маркировать x86_64 image как «подходит для любого NAS».

### 21.2. Native Core packaging без Python

**Убираем PyInstaller, Python interpreter, `uv sync` на хосте и Torch bundle из обязательной пользовательской установки.** Release состоит из Rust executables, необходимых native runtime libraries, service definitions/config templates и отдельно versioned model packs. TypeScript Firebase worker/его runtime — отдельный optional compute component; в локальном offline профиле он не нужен.

| Состав | Решение |
|---|---|
| `stillwatch-core`, `stillwatch-worker`, CLI, AlarmAgent | Native Rust binaries, точные source/toolchain/profile metadata |
| ORT | Закреплённая библиотека/EP pack с совместимым crate ABI; CPU обязательный профиль |
| FFmpeg/libav | Минимальная проверенная codec/device сборка; лицензии именно включённых компонентов |
| MediaMTX | Pinned отдельный executable; не «скомпилированный Rust-модуль» |
| Model pack | Проверенные ONNX + manifest + hashes + export provenance; не произвольный `.pt` |
| Firebase bridge | Optional Node/Web package с JS lock/runtime requirements |

Начальный Windows target — `x86_64-pc-windows-msvc`; Linux — `x86_64-unknown-linux-gnu` с выбранным minimum glibc baseline. Не обещать статический musl exe для произвольного ORT/CUDA/FFmpeg набора. ARM64 — отдельная native CI/acceptance строка. Version target/support matrix отражается в manifest, а не угадывается по расширению пакета.

Для `ort` выбрать и проверить способ linking; runtime loading из **конкретного защищённого install path** позволяет явно диагностировать отсутствующий/несовместимый ORT. Не искать DLL в CWD/temp или user-writable directory; native pack проверять по hash/manifest, rpath/loader policy ограничивать каталогом установки. Проверить transitive EP libraries: успешная загрузка основного ORT не означает доступность CUDA/TensorRT. [R30]

Model pack валидируется **до** создания session: class/keypoint map, tensor dtype/layout, opset, размер, внешние data-файлы, hashes и разрешённые operators. External-data paths не выходят из pack; custom-op DLL не скачивается из модели автоматически. Ошибка модели — explicit unavailable, не loading fallback неизвестных весов.

Граф/model pack сначала полностью готовится в staging directory; переключение active version — атомарно после warmup и health test. Новые incidents маркируются новой model/policy version. Нельзя менять модель в процессе inference, уничтожив используемую session или старые буферы.

Windows MSI включает SCM integration, ACL и отдельный user-session AlarmAgent. `.deb`/`.rpm` создают непривилегированного service user, systemd unit и права на data/model directories. Доступ к USB/GPU — только необходимые device permissions, не `privileged: true` по умолчанию. OCI CPU/GPU builds отдельны; в final image нет export toolchain или compiler.

`cargo build --release --locked` не равен готовому portable artifact: обязательны clean-machine smoke без Cargo/Python, DLL/so inventory, license/SBOM scan и install/update/rollback tests. Debug symbols хранятся отдельным CI artifact. Core и Companion устанавливаются раздельно; пользовательское приложение не требует admin/root для обычного подключения.

### 21.3. iOS: важное ограничение артефакта

Файл `.ipa` на GitHub **не означает**, что любой человек сможет его установить как Android APK. Development/ad hoc signing ограничен профилем и зарегистрированными устройствами; store distribution — другая схема. В техническом build manifest указывать signing type, supported devices/profile и expiry, без секретов. `xcarchive`/unsigned output не называть готовым пользовательским установщиком. [R22]

Developer membership не заменяет macOS/Xcode для сборки. Отсутствие собственного Mac можно закрыть macOS CI, но runtime notification/media tests всё равно требуют подходящего iPhone. [R13, R22]

### 21.4. CI без публикации

PR pipeline: `cargo fmt --check`, Clippy для выбранной feature/target matrix, Rust unit/property/contracts, Python oracle comparison, JS/Dart lint/tests, dependency/security checks → synthetic integration → generated SDK diff → native build smoke. CI не использует `--all-features` вместо проверки взаимоисключающих CPU/GPU profiles. На hot loops отдельно benchmarks с controlled runners; обычный noisy PR runner не доказывает performance regression. Protected build pipeline: platform matrix → native tests → package → signature/checksum/SBOM → artifact retention. Signing secrets доступны только доверенным branches/environments, не fork PR.

Каждый artifact содержит version, source commit, dependency lock digest, model manifest digest, API compatibility range, enabled capabilities и usage profile. Build не должен активировать production cloud billing или отправлять живые уведомления по умолчанию.

---

## 22. Миграция старого проекта на Rust

### 22.1. Карта переноса

| Сейчас | Новый Rust/изолированный компонент | Что именно переносим |
|---|---|---|
| `config.py` | `sw-engine::config`, `sw-protocol` | Typed config + semantic validation, отдельно instance/camera/model/budget |
| `capture.py`, `buffer.py` | `sw-capture`, `sw-engine::evidence` | Source clocks/epochs/retention invariants; новый bounded native capture |
| `perception.py` | `tools/model-export` + `sw-inference` | Locked graph reconstruction **только при export**; Rust ORT runtime и собственный проверенный pre/postprocess |
| `tracking.py`, `motion.py` | `sw-domain::tracking`, `medical` | Поведение по recorded observations; explicit identity/history rules |
| `triggers.py` | `sw-domain::medical`, `fire` | Medical parity; FireRules заменить temporal/context engine с intentional change log |
| `evidence.py` | `sw-engine::evidence`, protocol packets | Immutable per-camera packets, onset/coverage, limited byte ownership |
| `incidents.py` | `sw-domain::incidents`, `sw-storage` | Lifecycle/assessment/attention/ACK отдельно; persist transition atomically |
| `scheduler.py` | `sw-engine::scheduling`, adapters | Local inference scheduling отдельно от semantic requests/budgets |
| `decisions.py` | Rust Firebase bridge + JS SDK workers | Provider-neutral guards/schema; без OpenAI transport |
| `storage.py` | `sw-storage` | Writer thread, migrations, outbox, counters/reconciliation |
| `app.py` | `sw-api`, `bins/stillwatch-core` | API v2; daemon lifecycle независимо от web framework |
| `sources.py` | `sw-engine::cameras`, capture adapters | Concurrent durable sources вместо single selection |
| `web/*` | archived legacy UI | Заменить лёгким Flutter Companion |
| `sdk/python` | `packages/sdk-python`, новый `stillwatch-sdk` | HTTP SDK сохранить; Rust SDK добавить, не linking Python Core |

Перенос — не построчный transpilation. Важно сохранить инварианты и намеренное поведение; несколько огромных Python-классов не превращаются в несколько огромных mutex-protected Rust structs.

### 22.2. Три уровня parity и экспорт моделей

**Уровень A — model graph/tensors.** В изолированной environment восстановить **исходный locked graph**, включая architecture/module overrides из старого safe loader. Проверить source hashes до export. Экспорт из произвольного стандартного `YOLO(name)` с похожими весами не является эквивалентной заменой. Lock Python/Ultralytics/Torch/exporter/opset/flags и запретить auto-install в reproducible export.

Сначала CPU FP32, фиксированный validated shape; dynamic shapes/batches/FP16 — последующие profiles. Сравнивать Python reference и ONNX на одном и том же входном tensor: dtype, shape, finite checks, max/mean errors и high-impact boundary cases. Tolerances устанавливаются по фактическим outputs, не универсальная выдуманная константа для всех моделей.

**Уровень B — preprocessing/observations.** Проверить RGB/BGR, resize interpolation, letterbox ratio/padding/rounding, stride, normalization, CHW/HWC, uint8→float, inverse coordinate transform, box clamp, confidence/keypoints. Разница JPEG decode/resize не теряется в «модель примерно похожа»: хранить canonical decoded RGB/tensor fixtures для разделения причин.

Pose YOLO26 и fire/smoke YOLOv8 получают разные output-layout descriptors. NMS нужен либо не нужен согласно **фактическому export graph**, не семейству в строке имени. Текущая export документация различает raw, embedded-NMS и NMS-free outputs; для pose проверяются также keypoint shape/order/confidence. Не применять NMS дважды и не забывать его на raw outputs. [R20, R31]

Постпроцессинг сравнивать с reference по сопоставленным detections: labels, scores, IoU, keypoint error и missing/extra detections; raw output index order не считать единственным matching. Near-threshold изменения рассматриваются отдельно.

**Уровень C — event/state parity.** Python baseline экспортирует JSONL/CBOR observation trace без private pixels: camera/epoch, source timestamp, boxes/keypoints, quality/identity flags. Rust прогоняет те же observations через trackers/policies и сравнивает candidate/episode/assessment transitions, onset, recovery, stale guards и output actions. UUID/wall-clock nondeterminism нормализуется; порядок реально связанных событий не отбрасывается.

Expected failures старой FireRules/storage логики сохраняются как counterexamples. Исправленный Rust outcome описывается в intentional-difference registry; нельзя требовать буквального повторения известного дефекта. Новые golden outputs утверждаются по конкретной причине и regression scenario, не перезаписываются автоматически после каждого провала.

**Release gate:** уровни A/B/C пройдены для заявленного model profile плюс event eval на holdout. Даже идеальный tensor parity не доказывает частоту ложных тревог в реальном доме; это отдельная секция 19. Если G5 не пройден, отмечаем backend experimental, а не подсовываем Python runtime без объявления.

### 22.3. Model manifest v2

Каждый артефакт содержит: source repository/revision/weight SHA; source graph/overrides fingerprint; exporter commit и dependency lock; ONNX SHA-256, opset и проверенную ORT compatibility; input dtype/shape/layout/colour/normalization/letterbox; output layouts/class/keypoint maps/NMS mode; CPU/GPU profile и thresholds policy reference; parity report digest; licenses; hash каждого external-data файла.

`safetensors` не является готовым вычислительным ONNX-графом: loader исходного проекта восстанавливает архитектуру отдельно. Exporter переносит это восстановление, а не меняет расширение файла. Conversion формата **не снимает** лицензионные ограничения весов или исходных компонентов. [B11, B14, B16]

### 22.4. Данные и безопасный cutover

Rust CLI importer открывает старую schema1 read-only, пишет новую БД в другом каталоге и формирует migration manifest. Старый source становится первой камерой; оригинальные IDs сохраняются, где это совместимо с новым контрактом. Historical assessments имеют `legacy_provider=openai` и не активируют старый runtime.

Shared API token не переносится как общий пароль: новое per-device pairing. Cloud provider change показывается владельцу и не включает новую передачу кадров молча. Privacy masks конвертируются только с известной geometry; неизвестная геометрия → needs review. Незавершённые incidents переносятся как `needs_reassessment`, без новой сирены из-за прочтения исторического файла.

Cutover: backup → read-only importer/dry-run → compare counts/hashes/history → isolated replay Rust с отключённой реальной доставкой → остановить старый writer → запустить Rust на новой директории → smoke/rollback check. Старый и новый Core не открывают одну writable SQLite одновременно. Rollback восстанавливает соответствующий старый backup и конфигурацию; бинарный downgrade не читает новую schema наугад.

**Python reference остаётся dev-only до завершения parity, а не удаляется до появления заменяющих тестов.** Конечный release gate проверяет реальный install package на отсутствие interpreter/import/dynamic Python subprocess requirement.

---

## 23. Условия технической готовности

### 23.1. Проверяемая итоговая приёмка

| Область | Доказательство завершения |
|---|---|
| Автономность | Native Rust Core работает без UI/Python/Cargo, после reboot, без Firebase и при выключенных телефонах в рамках validated local profile |
| Rust migration | ONNX tensor/observation/event parity, intentional differences и legacy trace provenance; никакого скрытого CPython/Ultralytics subprocess |
| Разделение | Ни в одном Companion нет inference/model assets; compute workers не являются скрытой зависимостью открытого UI |
| Камеры | Concurrent CRUD/reconnect; независимые timestamps/epochs; unlimited configs + честный capacity control |
| Vision | Controlled-fire cases, medical identity/recovery и source-quality failures покрыты тестами |
| Firebase | Реальные SDK-вызовы, local/cloud evidence, App Check, pinned versions; G1/G2 status зафиксирован |
| Safety semantics | Unknown/error не означает safe; ACK не означает resolved; urgent latch и re-alert policy проверены |
| Состояние | Durable incidents/actions/outbox; crash recovery; evidence metadata consistency |
| Доступ | LAN pairing, TLS, роли, revoke, SSRF/upload defense, secret scanning |
| Live | Кодеки, masks, WHEP, multi-viewer, NAT/TURN и stale indication проверены |
| SDK | Rust async/optional blocking, Python sync/async, Dart, TS; общие contract/compatibility/reconnect/idempotency tests; no engine/ML deps |
| Приложения | Windows/Linux/iOS/Android реализуют заявленные функции на реальных устройствах |
| Алерты | Реальные capabilities ОС и terms G3; accepted/seen/ACK различаются; no delivery guarantee claims |
| Оптимизация | Reproducible benchmark/eval; bounded queues/memory; нет выигрыша latency за счёт скрытой потери recall |
| Recovery | Backup/restore/migration/upgrade/rollback и целостность data directory |
| Сборки | Native Windows/deb/rpm Core без Python, Android/iOS Companion; подписанные/проверенные native deps, SBOM/licenses G4/G5 и честная iOS installability |
| Операции | English technical instructions, diagnostic bundle без секретов, health watchdog/limitations |

### 23.2. Нерешённые сейчас вопросы и принятые действия

**Конкурсная трактовка вспомогательных YOLO:** подготовить точное описание конвейера и подтвердить допустимость у организатора. До подтверждения статус G1 — open, не «почти наверняка да».

**Firebase background runtime:** Node cloud entry point найден, но actual credentials/App Check/runtime call здесь не тестировались. Web hybrid browser lifecycle на NAS/service также не доказан. P00 обязан дать фактическую совместимость, а не ещё один абстрактный diagram.

**Реальный аварийный transport:** запрет high-risk применения FCM требует отдельного решения по реальному назначению. Не строить заявленную единственную защиту жизни на сервисе, условия которого это запрещают. Прототип/дополнительное наблюдение — отдельный profile с фактическими ограничениями, не только новым названием.

**Лицензии:** оригинальный код и inference dependencies имеют разные условия. При сохранении AGPL компонентов выбрать соответствующую лицензионную композицию/исходники или получить подходящие права; самостоятельно написанные независимые SDK/клиенты лицензировать осознанно. Process split сам по себе не является универсальным юридическим освобождением. Не считать просмотр vendor FAQ полноценным юридическим заключением. [B14, B16, R21]

**Реальная точность:** контролируемый огонь не исправлен одним новым prompt. Нужны данные, temporal policy, calibrated evaluation и наблюдение за unknown/false-negative tradeoff. Локальные эксперименты этого документа проверили кодовые свойства, но не безопасность конечной системы.

**Rust/native parity:** уровни A/B/C и clean-target packaging пока не выполнены — это отдельный G5, не следствие выбора языка. Сначала проверить обе locked модели и libav/ORT deployment; не объявлять Rust безопаснее или быстрее для этого проекта без соответствующих тестов.

**Итог:** строить `stillwatch-next` как переносимый **Rust Core** с независимыми лёгкими компаньонами. Firebase — отдельный честно проверяемый compute layer. Не жертвовать архитектурой продукта ради декоративной конкурсной галочки и не скрывать внешние ограничения за обещанием «100%».

---

## 24. Источники

Ссылки B — исходный код, просмотренный на неизменяемом commit. Ссылки R — первичные документы провайдеров/стандартов. Рекомендации по структуре, thresholds, этапам и внутренним контрактам являются проектными решениями этого плана, а не цитатами из источников. Baseline-аудит и ссылки B/R01–R23 сохранены из предыдущей редакции; новые Rust-источники R24–R37 проверены при этой доработке. Официальные правила и список Firebase SDK также открыты повторно. Это не повторный построчный аудит текущей ветки GitHub. Внешние страницы могут обновляться; перед фиксацией toolchain перепроверить выбранные версии.

### Исходная репа

- **[B00]** [Baseline/tree](https://github.com/mikhailbovt/stillwatch/tree/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3)
- **[B01]** [`runtime.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/runtime.py)
- **[B02]** [`capture.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/capture.py)
- **[B03]** [`buffer.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/buffer.py)
- **[B04]** [`triggers.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/triggers.py)
- **[B05]** [`decisions.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/decisions.py)
- **[B06]** [`incidents.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/incidents.py)
- **[B07]** [`scheduler.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/scheduler.py)
- **[B08]** [`storage.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/storage.py)
- **[B09]** [`app.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/app.py)
- **[B10]** [`sources.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/sources.py)
- **[B11]** [`perception.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/perception.py)
- **[B12]** [`config.py`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/src/stillwatch/config.py)
- **[B13]** [Python SDK client](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/sdk/python/src/stillwatch_sdk/client.py)
- **[B14]** [Third-party notices](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/THIRD_PARTY_NOTICES.md)
- **[B15]** [`pyproject.toml`](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/pyproject.toml)
- **[B16]** [Model manifest / licenses](https://github.com/mikhailbovt/stillwatch/blob/b0d20a20d38a5bbc22178d150999e57cc9e7bfb3/models.lock.json)

### Конкурс, Firebase, платформы и стандарты

- **[R01]** [#BuildHybridAI official rules](https://firebasehybridaihackathon.devpost.com/rules)
- **[R02]** [#BuildHybridAI overview](https://firebasehybridaihackathon.devpost.com/)
- **[R03]** [Firebase AI Logic: hybrid Web setup](https://firebase.google.com/docs/ai-logic/hybrid/web/get-started)
- **[R04]** [Chrome built-in AI requirements and availability](https://developer.chrome.com/docs/ai/get-started)
- **[R05]** [Firebase hybrid configuration options](https://firebase.google.com/docs/ai-logic/hybrid/web/configuration-options)
- **[R06]** [Official Firebase AI package: Node/browser exports and tests](https://github.com/firebase/firebase-js-sdk/blob/main/packages/ai/package.json) — просмотрен `main`; намеренно не считается dependency lock.
- **[R07]** [FCM priority: Life critical use cases](https://firebase.google.com/docs/cloud-messaging/customize-messages/setting-message-priority)
- **[R08]** [Custom App Check provider](https://firebase.google.com/docs/app-check/custom-provider) и [Web custom provider integration](https://firebase.google.com/docs/app-check/web/custom-provider)
- **[R09]** [Apple Critical Alerts entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.usernotifications.critical-alerts)
- **[R10]** [Android full-screen intent limits](https://source.android.com/docs/core/permissions/fsi-limits)
- **[R11]** [Microsoft: interactive services / user-session separation](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)
- **[R12]** [MediaMTX WebRTC/WHEP](https://mediamtx.org/docs/read/webrtc) и [connectivity/codec specifics](https://mediamtx.org/docs/features/webrtc-specific-features)
- **[R13]** [Flutter platform integration](https://docs.flutter.dev/platform-integration) и [supported platforms](https://docs.flutter.dev/reference/supported-platforms)
- **[R14]** [flutter-webrtc official repository](https://github.com/flutter-webrtc/flutter-webrtc)
- **[R15]** [SQLite WAL constraints](https://www.sqlite.org/wal.html)
- **[R16]** [OpenAPI 3.1 specification](https://spec.openapis.org/oas/v3.1) и [specification version index](https://spec.openapis.org/oas/)
- **[R17]** [RFC 9457: Problem Details](https://www.rfc-editor.org/rfc/rfc9457.html)
- **[R18]** [OWASP SSRF Prevention](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html)
- **[R19]** [ONNX Runtime thread management](https://onnxruntime.ai/docs/performance/tune-performance/threading.html)
- **[R20]** [Ultralytics model export and quantization](https://docs.ultralytics.com/modes/export)
- **[R21]** [Ultralytics licensing guidance](https://www.ultralytics.com/license)
- **[R22]** [Apple: distributing to registered devices](https://developer.apple.com/documentation/xcode/distributing-your-app-to-registered-devices)
- **[R23]** [Firebase AI Logic getting started / supported client integrations](https://firebase.google.com/docs/ai-logic/get-started)
- **[R24]** [Cargo release profiles](https://doc.rust-lang.org/cargo/reference/profiles.html) — заменяет устаревшую для новой архитектуры ссылку на PyInstaller.
- **[R25]** [Tokio spawn_blocking: CPU work and cancellation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
- **[R26]** [Tokio channels](https://tokio.rs/tokio/tutorial/channels), [watch](https://docs.rs/tokio/latest/tokio/sync/watch/index.html), [broadcast and lag](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html).
- **[R27]** [Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown), [process ownership / cancellation](https://docs.rs/tokio/latest/tokio/process/index.html).
- **[R28]** [axum: Tokio/Tower integration](https://docs.rs/axum/latest/axum/).
- **[R29]** [rusqlite Connection](https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html).
- **[R30]** [ort repository](https://github.com/pykeio/ort), [maintainer linking documentation](https://github.com/pykeio/ort/blob/main/docs/content/setup/linking.mdx), [execution-provider requirements](https://github.com/pykeio/ort/blob/main/docs/content/perf/execution-providers.mdx).
- **[R31]** [Ultralytics end-to-end/NMS export layouts](https://docs.ultralytics.com/guides/end2end-detection).
- **[R32]** [Rust 2024 Cargo resolver](https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html), [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html).
- **[R33]** [reqwest blocking API and async-context restriction](https://docs.rs/reqwest/latest/reqwest/blocking/index.html).
- **[R34]** [windows-service Rust crate](https://docs.rs/windows-service/latest/windows_service/).
- **[R35]** [ffmpeg-next native binding API](https://docs.rs/ffmpeg-next/latest/ffmpeg_next/).
- **[R36]** [Rust FFI safety and ownership](https://doc.rust-lang.org/nomicon/ffi.html).
- **[R37]** [Official Firebase AI Logic SDK reference index](https://firebase.google.com/docs/ai-logic/ref-docs) — подтверждает перечень официальных платформ, не содержит Rust SDK.

### Приложенные результаты

Предыдущие `audit_harness.py`, `tests/buffer_original.py`, `tests/types.py`, `results.json`, `audit-run.txt`, `TEST_REPORT_RU.md`, environment/provenance и README находятся в новом архиве в `audit/legacy-python/`. Они позволяют перепроверить ограниченные **старые Python** эксперименты; это не Rust Core и не полный upstream checkout.

`RUST_REVISION_CHANGELOG_RU.md` описывает замену архитектуры. `document_validation.json` содержит результат структурной проверки именно документа, не компиляции или исполнения Core. `SHA256SUMS.txt` относится к новому комплекту. Ни один старый benchmark не переименован в Rust benchmark.

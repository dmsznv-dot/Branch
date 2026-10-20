# Rust Core Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Создать исполнимое и протестированное Rust-ядро комнаты с авторитетным состоянием, алгоритмом синхронизации и защищённой потоковой выдачей только выбранного файла через loopback HTTP Range.

**Architecture:** Четыре независимых от UI crate: shared-protocol → sync-engine → room-server; media-stream использует их публичные контракты. RoomServer проверяет сессию и права, выдаёт отзываемый MediaPermit; HTTP-адаптер проверяет локальную capability и разрешение до чтения и перед каждым отправляемым чанком. Время задаётся явно и не зависит от системной даты.

**Tech Stack:** Rust 1.99.1, edition 2024; Tokio 1, axum =0.8.9, hyper 1/server/http1 и hyper-util 0.1/tokio/service, serde 1/serde_json 1, uuid 1, bytes 1, tokio-util 0.7, getrandom 0.4, arc-swap 1, sha2 0.10, subtle 2, base64 0.22, thiserror 2. Dev: reqwest 0.12 с rustls, tempfile 3, futures-util 0.3. Cargo.lock фиксируется в первой задаче. Прямые зависимости добавляются только использующим их crate.

**Spec:** [Утверждённая архитектура](../specs/2026-10-20-watch-together-design.md), [карта подпроектов и покрытия](2026-10-20-prototype-roadmap.md).

**UI supplement:** [PDF и перенос требований](../../requirements/2026-10-20-interface-notes.md). Для A новых UI-задач нет; UI покрывается B/C/D в карте, а не отбрасывается.

## Global Constraints

- Целевые платформы продукта: macOS ARM64 и Windows x64; Linux используется для разработки ядра, не для приёмки этих ОС.
- Максимум пять участников вместе с хостом; режимы `everyone` и `host-only`.
- Глобальные скорости: `0.5`, `0.75`, `1`, `1.25`, `1.5`, `2`; громкость, mute, fullscreen, аудио и субтитры локальны.
- Проверка позиции каждые 1–2 секунды; `|drift|≤100 ms` без коррекции, `100<|drift|≤500 ms` локальная коррекция скорости, `>500 ms` seek.
- Полный приёмочный результат прототипа ≤250 мс в реальной стабильной сети; детерминированные тесты A этот результат не подтверждают.
- Только явно выбранный файл, без общего файлового API и произвольного проксирования; нет постоянного гостевого кэша.
- Loopback: `127.0.0.1:0`, точный `Host`, секрет в заголовке на каждом GET/HEAD, без широкого CORS и redirects.
- Закрытие и исключение отменяют активные передачи; уже доставленные байты не считаются отозванными.
- Передача оригинальных байтов без транскодирования. WebRTC/TURN, libmpv/Tauri и целевые bundle реализуются следующими подпроектами B/C/D.
- Установка сохраняет TLS, checksum и подписи; секреты не выводятся и не коммитятся.
- Использовать `/workspace/Branch`; не создавать worktree. Архитектура и план утверждены пользователем; задачи A1–A6 реализованы и прошли task review.

## Review Focus

1. Числа >2^53 и переполнения: wire не теряет точность, JS в будущем не получает u64 как number — тесты задачи 1.
2. Перезапуск часов/старое mediaId: старое состояние и запросы не применяются к новой эпохе/фильму — задачи 2–3, 6.
3. Удаление/подмена/усечение выбранного пути: не открывается другой файл и не выдаются неинициализированные байты — задача 4.
4. Медленный HTTP-клиент и исключение посреди ответа: память ограничена, следующие чтения прекращаются — задачи 5–6.
5. HEAD, пустой файл, malformed/multiple range и Host rebinding: нет утечки метаданных до авторизации — задачи 4–5.

---

## Файлы и ответственность

| Путь | Ответственность |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.gitignore` | Workspace, воспроизводимый toolchain и зависимости, исключение outputs |
| `scripts/setup-core.sh` | Повторяемая установка toolchain в `/workspace/.toolchains` без shell-profile mutations |
| `crates/shared-protocol/src/{lib,ids,numbers,playback,commands,error}.rs` | Wire-типы, проверенные значения, ошибки |
| `crates/sync-engine/src/{lib,clock,projection,estimator,correction}.rs` | Монотонное время и независимая логика синхронизации |
| `crates/room-server/src/{lib,room,authority,sessions,limits}.rs` | Членство, авторитет, права, дедупликация, отзываемые разрешения |
| `crates/media-stream/src/{lib,range,source,http,body}.rs` | Ограниченное чтение выбранного дескриптора и HTTP |
| `crates/media-stream/src/bin/core-harness.rs` | Локальный проверочный запуск, открытие выбранного файла и lifecycle |
| `crates/*/tests/*.rs` | Контрактные и интеграционные тесты рядом с владельцем поведения |
| `README.md`, `docs/development/core.md`, `.github/workflows/core.yml` | Проверенные команды разработки и CI ядра |

Создавать только используемые пакеты; не создавать пустые desktop/network/services. Фикстуры генерируются тестами в tempfile, без чужих фильмов в Git.

## Общие интерфейсы

Эти определения распределены по задачам ниже; executor читает Interfaces каждой зависимости. Ошибки принадлежат crate-владельцу и преобразуются в протокольный ErrorCode на границе. Не сериализовать токены или секреты через стандартные Debug/Display.

При добавлении нового crate или прямой зависимости первая RED-команда `cargo test -p <crate>` может обновить Cargo.lock; это штатное разрешение зависимостей, не проверка frozen lock. Все GREEN, повторные запуски и CI используют `--locked`. В RED создаются минимальные компилируемые пустые lib.rs, чтобы падение было на отсутствующем поведении/публичном API, а не на некорректном manifest. Lockfile коммитится при каждом обоснованном добавлении зависимости.

### Task 1: Workspace и проверяемые wire-контракты

**Files:** создать корневые файлы и `scripts/setup-core.sh`; `crates/shared-protocol/Cargo.toml`; перечисленные shared-protocol source-файлы; `crates/shared-protocol/tests/wire_contract.rs`.

**Interfaces:**
- Produces UUIDv4 newtypes `RoomId`, `MediaId`, `ParticipantId`, `CommandId`, `ClockEpochId`: `new() -> Self`, `parse(&str) -> Result<Self, ProtocolError>`; serde-строки UUID.
- Produces `WireU64(u64)` с `new(u64) -> Self`, `get(self) -> u64`; JSON — только каноническая десятичная строка `"0"` или `[1-9][0-9]*`, длина ≤20, без пробела/знака. Используется для позиции/длительности/ticks/revision/byte offset/length.
- Produces `Speed::try_from(f64) -> Result<Speed, ProtocolError>`, `ratio(self) -> (u32,u32)`, `as_f64(self) -> f64`; ровно шесть скоростей из Global Constraints; JSON number.
- Produces `HostTime { epoch: ClockEpochId, ticks_ms: WireU64 }` и `Playback { status: PlaybackStatus, position_ms: WireU64, speed: Speed, updated_at: HostTime }`.
- Produces `MediaDescriptor { media_id: MediaId, size_bytes: WireU64, duration_ms: WireU64 }`, `ControlMode::{Everyone,HostOnly}`, `PlaybackStatus::{Paused,Playing}`.
- Produces `RoomSnapshot { room_id, media: MediaDescriptor, revision: WireU64, playback: Playback, control: ControlMode }`.
- Produces `PlaybackCommand::{Play,Pause,Seek{position_ms:WireU64},SetSpeed{speed:Speed}}` и `CommandEnvelope { room_id:RoomId, media_id:MediaId, command_id:CommandId, issued_at:HostTime, action:PlaybackCommand }`. Автор не приходит из JSON; issued_at — оценка монотонного времени хоста, не календарная дата.
- Produces `ErrorCode::{InvalidInput,Unauthorized,Forbidden,RoomFull,RoomClosed,MediaChanged,CommandConflict,CommandExpired,RateLimited,ClockMismatch,CounterExhausted}`. Входные объекты имеют camelCase поля и `deny_unknown_fields`.

- [x] **Step 1: Создать минимальный workspace и контрактные тесты до реализации типов.** В setup использовать официальный rustup-init для `x86_64-unknown-linux-gnu`, скачать соответствующий `.sha256`, проверить `sha256sum -c` с включённым TLS; `RUSTUP_HOME=/workspace/.toolchains/rustup`, `CARGO_HOME=/workspace/.toolchains/cargo`, `--profile minimal --default-toolchain 1.99.1 --no-modify-path -y`. Скрипт проверяет уже установленную версию и не переустанавливает её. `rust-toolchain.toml` закрепляет 1.99.1 + rustfmt/clippy. Установить toolchain только после review плана. Создать тесты `wire_u64_preserves_precision`, `wire_u64_rejects_noncanonical`, `speed_accepts_only_prd_values`, `input_cannot_claim_actor`:

```rust
assert_eq!(serde_json::to_string(&WireU64::new(9_007_199_254_740_993)).unwrap(), "\"9007199254740993\"");
assert!(serde_json::from_str::<WireU64>("9007199254740993").is_err());
assert!(serde_json::from_str::<WireU64>("\"18446744073709551616\"").is_err());
assert!(serde_json::from_str::<WireU64>("\"01\"").is_err());
assert_eq!(Speed::try_from(1.25).unwrap().ratio(), (5, 4));
assert!(Speed::try_from(f64::NAN).is_err());
```

Дополнительно: `-1`, infinity, 0.1, JSON unknown action, `participantId` в CommandEnvelope и невалидный UUID отклоняются; полное RoomSnapshot roundtrip сохраняет типы и значения.

- [x] **Step 2: Зафиксировать RED.** Из корня с активированным toolchain: `cargo test -p shared-protocol --test wire_contract`. Ожидается ошибка компиляции отсутствующих типов/методов; сетевой сбой установки не считается RED теста.
- [x] **Step 3: Реализовать указанные типы и serde.** Рациональные скорости без произвольных float; разбор u64 через checked parsing. Создать Cargo.lock обычным разрешением зависимостей один раз; последующие команды `--locked`.
- [x] **Step 4: Зафиксировать GREEN и повторяемость setup.** Повторить setup, затем `cargo test --locked -p shared-protocol --test wire_contract` и `cargo fmt --all -- --check`; все названные тесты выполнены, второй setup не меняет toolchain или tracked manifests.
- [x] **Step 5: Commit.** Только файлы этой задачи: `chore: establish Rust workspace and protocol contracts`.

### Task 2: Проекция позиции, часы и решения о коррекции

**Files:** `crates/sync-engine/Cargo.toml`, перечисленные sync-engine source-файлы; `crates/sync-engine/tests/{projection,clock_sync,correction}.rs`.

**Interfaces:**
- Consumes `HostTime`, `Playback`, `Speed`, `RoomSnapshot` из задачи 1.
- Produces `trait Clock: Send+Sync { fn now(&self)->HostTime; }`, `MonotonicClock::new(epoch:ClockEpochId)->Self` на std::time::Instant; `ManualClock::new(now:HostTime)->Self`, `advance_ms(&self,delta:u64)` под feature `test-support`. Все task3–6 тесты внедряют `Arc<ManualClock>`, без sleep.
- Produces `position_at(playback: &Playback, now: HostTime, duration_ms: u64) -> Result<u64, SyncError>`; время другой эпохи или меньше updated_at — ошибка, вычисления через u128, итог ограничен duration.
- Produces `ClockSample { client_send_ms:u64, host_recv_ms:u64, host_send_ms:u64, client_recv_ms:u64, host_epoch:ClockEpochId }`, `ClockEstimator::new()`, `observe(sample) -> Result<(), SyncError>`, `estimate() -> Option<ClockEstimate>`; `ClockEstimate { offset_ms:i128, rtt_ms:u64, host_epoch }`.
- Produces `Correction::{Keep,Rate{effective_speed:f64},Seek{position_ms:u64}}`, `correction(target_ms:u64, observed_ms:u64, status:PlaybackStatus, base:Speed, effective_speed:f64) -> Correction`.
- Produces `SnapshotCursor::new()`, `accept(&RoomSnapshot) -> Result<bool, SyncError>`: для текущей комнаты применяет только более высокую revision; новый room/clock epoch требует `reset(&RoomSnapshot)`, выполненного после доверенного snapshot reconnect.

- [x] **Step 1: Написать тесты.** `playing_projects_rational_speed`: 125000 ms, 1.5×, ticks 1000→3000 дают `assert_eq!(...,128000)`; paused остаётся 125000. `projection_clamps_end_and_rejects_epoch` проверяет duration и другую эпоху; назад по ticks — ошибка, системное UTC в API отсутствует. `clock_estimator_ignores_high_rtt`: sample (1000,1510,1512,1022) даёт offset 500, RTT 20; более медленная следующая выборка не заменяет лучшую. Проверить отрицательные интервалы и смену эпохи.

`correction_boundaries` проверяет оба знака drift: 100→Keep, 101/500→Rate, 501→Seek. При отставании Rate = base×1.02, опережении base×0.98. После схода к ≤100 и effective≠base возвращается Rate с base. Для paused drift>100 сразу Seek; rate не запускает паузу. `stale_snapshot_and_epoch_require_reset` проверяет старую revision, новый mediaId с новой revision и явный reset после reconnect.

- [x] **Step 2: RED.** `cargo test -p sync-engine`; ожидается отсутствие новых API, а не ноль тестов.
- [x] **Step 3: Реализовать алгоритмы.** ClockEstimator хранит восемь последних валидных samples, выбирает минимальный RTT; samples с RTT>2000 ms отклоняет. Offset = `((host_recv-client_send)+(host_send-client_recv))/2` в i128, RTT исключает host processing. Новая host epoch сбрасывает старые samples. Дубликаты snapshot возвращают false, рассогласование room/epoch — ошибку. Коррекция не генерирует PlaybackCommand.
- [x] **Step 4: GREEN.** `cargo test --locked -p sync-engine`; `cargo test --locked -p shared-protocol`; тесты не используют sleep/системные часы.
- [x] **Step 5: Commit.** `feat: add deterministic playback synchronization`.

### Task 3: Авторитет комнаты, сессии, права и отзыв

**Files:** `crates/room-server/Cargo.toml`, перечисленные room-server source-файлы; `crates/room-server/tests/{authority,sessions,lifecycle,limits}.rs`.

**Interfaces:**
- Consumes типы задачи 1 и `position_at` задачи 2.
- Produces `SessionToken`, `InvitationToken`: отдельные secret newtypes с 32 случайными байтами, base64url без padding, `expose(&self)->&str`; Debug/Display redacted, serde отсутствует. Хранилище держит SHA-256 digest, сравниваемый constant-time, а не сырые токены; ошибки секреты не содержат.
- Produces `RoomHandle::create(media:MediaDescriptor, host_name:&str, clock:Arc<dyn Clock>) -> Result<CreatedRoom,RoomError>`; `CreatedRoom { room:RoomHandle, host_id:ParticipantId, host_session:SessionToken, invitation:InvitationToken }`. Clock внедряется один раз; публичные методы самостоятельно берут now из него, а не принимают время от клиента.
- Produces async методы `join(&self, invitation:&InvitationToken, name:&str)->Result<JoinedRoom,RoomError>`; `JoinedRoom { participant_id:ParticipantId, session:SessionToken, snapshot:RoomSnapshot }`; `apply(&self, session:&SessionToken, command:CommandEnvelope)->Result<RoomSnapshot,RoomError>`; `snapshot(&self, session:&SessionToken)->Result<RoomSnapshot,RoomError>`.
- Produces async host-only методы `set_control(&self,session:&SessionToken,mode:ControlMode)->Result<RoomSnapshot,RoomError>`, `rotate_invitation(&self,session:&SessionToken)->Result<InvitationToken,RoomError>`, `kick(&self,session:&SessionToken,target:ParticipantId)->Result<(),RoomError>`, `replace_media(&self,session:&SessionToken,media:MediaDescriptor)->Result<RoomSnapshot,RoomError>`, `close(&self,session:&SessionToken)->Result<(),RoomError>`. Закрытая комната возвращает RoomClosed, не возобновляет session.
- Produces async `authorize_media(&self,session:&SessionToken,media_id:MediaId)->Result<MediaPermit,RoomError>`; `MediaPermit::check_active(&self)->Result<(),RoomError>`, `cancelled(&self)` async. Проверка активности использует внедрённые часы и атомарное состояние сессии без блокирующего room lock. Закрытие/исключение/смена media отменяют соответствующие permits; успешное чтение продлевает активность живой сессии.
- Produces `set_buffering(session, bool)->Result<(),RoomError>`; playback/revision не меняет. `subscribe(session)->Result<tokio::sync::watch::Receiver<RoomSnapshot>,RoomError>` для будущей доставки состояния.

- [x] **Step 1: Написать тесты политики.** `fifth_allowed_sixth_rejected`, `guest_control_depends_on_mode`, `host_uses_same_apply_path`, `pause_and_speed_project_before_update`, `seek_clamped_to_duration`, `duplicate_is_idempotent`, `duplicate_payload_conflict`, `expired_command_is_not_reapplied`, `buffering_does_not_pause_room`, `wrong_room_or_media_rejected`, `revoke_invite_keeps_existing_session`, `kick_revokes_media_permit`, `close_revokes_all`, `replacement_cancels_old_media`.

При paused room revision=0 host Play делает revision=1; повтор такого же `(session,commandId,payload)` возвращает тот же результат и revision=1; другой payload с тем же ID — CommandConflict. Stale mediaId/unauthorized команда не меняет state. Revision::MAX → CounterExhausted без wraparound. `limits_are_bounded` проверяет 21-ю команду за 1000 ms, шестого участника, имя >64 UTF-8 байт и запросы с недопустимыми control characters.

- [x] **Step 2: RED.** `cargo test -p room-server`; ошибки отсутствующих API должны соответствовать текущей задаче.
- [x] **Step 3: Реализовать комнату.** Сериализовать мутации одним mutex, не держать lock во время I/O. Зафиксировать monotonic clock epoch при create; initial Paused/position=0/speed=1/revision=0. Команды ограничены 20/s/session fixed window; новые joins — 10/min/room. Дедупликация хранит максимум 256 команд на сессию до issued_at+120000 ms (не более 122000 ms после принятия при разрешённом будущем timestamp). Команды с issued_at старше 120000 ms или из будущего более чем на 2000 ms отклонять до выполнения, а не повторять после eviction. Смена payload, включая issued_at, при прежнем commandId даёт CommandConflict. Если cache заполнен неистёкшими результатами, возвращать RateLimited для нового commandId. Все wall-clock timestamps отсутствуют.

Сессии имеют период неактивности 300000 ms; apply/authorize/snapshot/status и активное чтение обновляют activity через внедрённый Clock. До продления всегда проверяется, что срок не истёк. Монотонное время внутри комнаты не идёт назад; другая epoch требует новой комнаты. watch не включает токены; один слот last-state на сессию, максимум8 подписок на комнату. Новый mediaId обязателен при replace, initial playback reset, revision растёт. Ключи приглашения отдельно от session; закрытие завершает watch, удаляет секретные привязки и отменяет permits.

- [x] **Step 4: GREEN.** `cargo test --locked -p room-server` и существующие тесты protocol/sync. Детерминированно продвинуть ManualClock на 300001 ms — сессия больше не получает permit. Отдельно продвинуть на 120001 ms — старая команда не применяется снова.
- [x] **Step 5: Commit.** `feat: enforce authoritative rooms and revocable sessions`.

### Task 4: Range и ограниченное чтение выбранного файла

**Files:** `crates/media-stream/Cargo.toml`, `src/{lib,range,source}.rs`; `tests/{range,selected_file}.rs`.

**Interfaces:**
- Produces `ByteRange { start:u64, len:u64 }`; `resolve_range(header:Option<&str>, size:u64)->Result<Option<ByteRange>,RangeError>`: None = GET 200 full body, Some =206. `RangeError::{Malformed,Multiple,Unsatisfiable}`.
- Produces `SelectedMedia::open(path:&Path, media_id:MediaId)->Result<Self,StreamError>`, `descriptor(duration_ms:u64)->MediaDescriptor`, `read_chunk(&self, permit:&MediaPermit, offset:u64, max_len:usize)->Result<bytes::Bytes,StreamError>`; файл открывается один раз только из доверенного Rust-вызова.
- Produces `CHUNK_BYTES:usize=65536`, `MAX_READERS_PER_SESSION:usize=2`, `MAX_READERS_PER_ROOM:usize=8`. Разрешение на reader удерживается RAII до завершения потока.

- [x] **Step 1: Написать табличные тесты.** Для size=10: `bytes=0-0`→(0,1), `bytes=9-`→(9,1), `bytes=-3`→(7,3), `bytes=7-999`→(7,3), `bytes=-99`→(0,10), `bytes=10-`/`bytes=-0`/`bytes=8-7`→Unsatisfiable. Для size=0 любой удовлетворяющий синтаксису range→Unsatisfiable. `bytes=0-1,5-6`→Multiple; unknown unit, лишний дефис, знак, u64 overflow→Malformed. Внешний HTTP пробел разрешён только до/после всего значения.

`sparse_offset_over_4gib` создаёт sparse fixture с маркером на offset=4294967312 и читает точные байты; `path_replacement_does_not_reopen` открывает выбранный файл, затем меняет путь, подтверждает чтение старого дескриптора; `truncated_source_reports_change` усечение даёт MediaChanged/UnexpectedEof без выдачи некорректных байтов; `cancelled_permit_stops_read` исключает участника до чтения. Система без поддержки sparse/rename отмечается отдельно, а не как прошедший тест.

- [x] **Step 2: RED.** `cargo test -p media-stream --test range --test selected_file`.
- [x] **Step 3: Реализовать source и parser.** checked arithmetic, конец включителен только в заголовке; внутри start+len проверяется без переполнения. Читать не более min(max_len,65536,remaining), не весь range в Vec. Один mutex на открытый tokio File охватывает seek+read: клонированные дескрипторы могут делить file offset, поэтому параллельные seek без синхронизации запрещены. Проверять permit до чтения и после await; изменение длины/mtime отменяет выдачу этого mediaId как MediaChanged. Удаление пути не разрешает переоткрыть другой файл. Размер 0 — нормальная метаинформация.
- [x] **Step 4: GREEN.** Запустить тесты media-stream и room-server; тест `concurrent_ranges_do_not_share_cursor` читает параллельно два разных маркера и проверяет оба результата.
- [x] **Step 5: Commit.** `feat: stream bounded ranges from selected file handles`.

### Task 5: Реальный защищённый loopback HTTP

**Files:** `crates/media-stream/src/{http,body}.rs`; `tests/{http_range,http_security}.rs`.

**Interfaces:**
- Produces `LoopbackCapability` (отдельный секрет 32 bytes); `LoopbackServer::start(source:Arc<SelectedMedia>, room:RoomHandle, session:SessionToken)->Result<Self,StreamError>`, `address()->SocketAddr`, `capability()->&LoopbackCapability`, `shutdown(self)->Result<(),StreamError>`.
- Endpoint только `/media/{mediaId}`, методы GET/HEAD. Авторизация: точный `Host=127.0.0.1:<assigned-port>`, `Authorization: Bearer <local capability>`, активная session и mediaId. SessionToken хранится в сервере, не передаётся в URL. capability не равна InvitationToken/SessionToken. Другие маршруты→404 без файлового API.
- Body: pull-driven stream, очередь максимум два чанка на запрос. Перед чтением и перед yield проверяются permit и отмена; источники/сессии не клонируются с расширением прав.

- [x] **Step 1: Написать HTTP-тесты через реальный TCP.** `get_exact_range` для файла `0123456789`, range `bytes=2-4`: status=206, body=`234`, Content-Range=`bytes 2-4/10`, Content-Length=`3`, Accept-Ranges=`bytes`. `head_has_full_length_and_no_body`: HEAD с Range→200, длина10, пустое тело. GET без range→200/полные10; size0 без range→200/длина0; unsatisfiable→416 с `bytes */10`; malformed/multiple→400. Content-Type=`application/octet-stream`, Cache-Control=`no-store`.

`unauthorized_head_does_not_leak_size` и GET без/со старым токеном→401 без Content-Range/размера; неверный Host→403 до чтения; path traversal→404; Origin не получает Allow-Origin; POST→405. Слишком большой заголовок (>8192 bytes)→431, не range parser. Redirects отсутствуют. Закрытая room запрещает выдачу даже с прежним локальным токеном.

- [x] **Step 2: RED.** `cargo test -p media-stream --test http_range --test http_security`.
- [x] **Step 3: Реализовать axum adapter и body.** Listener только 127.0.0.1:0, принимать Host без вариантов доменного имени. Авторизация раньше Range/HEAD metadata. При 416 сообщать размер только уже авторизованному клиенту; при mismatch source/room mediaId отклонять до I/O. Для transport-лимита использовать hyper http1 Builder с max_buf_size(8192) и axum Router через TowerToHyperService; отслеживать owned connection tasks. Численные лимиты читателей задачи4 применяются до открытия body (429). Нет trace middleware с токенами; временные имена файлов не выводятся в ошибках.
- [x] **Step 4: GREEN.** Два тестовых server инстанса имеют разные capability. Запрос capability первого ко второму→401. `cargo test --locked -p media-stream` выполняет все тесты, порты берутся динамически, ни один тест не зависит от порядка.
- [x] **Step 5: Commit.** `feat: add authenticated loopback range server`.

### Task 6: Отзыв активного ответа, проверочный запуск и воспроизводимость

**Files:** `crates/media-stream/tests/{revocation,backpressure,workflow}.rs`, `src/bin/core-harness.rs`, `README.md`, `docs/development/core.md`, `.github/workflows/core.yml`.

**Interfaces:**
- Consumes все публичные API предыдущих задач.
- Дополнение по проверке lifecycle: `CreatedRoom.owner: RoomOwner` — локальная nonserializable close-only capability; `RoomOwner::close(self)` async отзывает комнату без проверки истёкшей host session. Сетевой `RoomHandle::close` сохраняет прежнюю авторизацию.
- Produces `LocalRoomSession::start(file:&Path,duration_ms:u64,host_name:&str,clock:Arc<dyn Clock>)->Result<Self,StreamError>` и `shutdown(self)->Result<(),StreamError>`; владеет room, выбранным источником и loopback server. Сервер создаётся только после создания комнаты с descriptor выбранного файла.
- Produces CLI `cargo run --locked -p media-stream --bin core-harness -- --file <path> --duration-ms <u64>`. Только локальный тестовый harness: создаёт room, открывает выбранный файл, запускает host loopback adapter; Ctrl+C → shutdown и close. В stdout только безопасные события `room-created`, `range-server-ready`, `room-closed`; секреты/путь/URL с секретом не печатаются. Capability тестовый harness передаёт только прямо создаваемому mpv-процессу на этапе B; сейчас реальные HTTP smoke выполняются Rust integration tests, не копированием токена из лога.
- Produces local health probe через внутренний authenticated request в harness; `range-server-ready` выводится после корректного HEAD. Нулевой PID/открытый порт не считается readiness.

- [x] **Step 1: Написать детерминированные integration tests.** `kick_cancels_between_chunks` и `close_cancels_between_chunks`: управляемый body barrier после первого чанка, затем kick/close, следующий read/yield запрещён, server task завершена. Bytes, уже попавшие в socket buffer, не объявлять отозванными. `closing_drops_media_and_tasks`: shutdown.await возвращается только после завершения owned reader tasks; файл больше не удерживается владельцами комнаты. `old_media_cannot_serve_new_generation`: replace_media, старый URL/token отвергнут, новый требует новый adapter/capability. `slow_reader_has_bounded_queue`: paused consumer удерживает не более2 queued+1 active чанков, не читает фильм целиком; cancellation завершает producer. Проверять счётчики выделенных chunks тестовым instrumentation, а не нестабильный process RSS.

`room_to_http_workflow` создаёт host+guest с ManualClock: host-only запрещает гостю seek, everyone разрешает; после Play и +2000 ms snapshot соответствует проекции, поздний гость получает текущее состояние; HEAD/Range возвращают выбранные байты; guest kick прекращает доступ; host close заканчивает server. Это локальный integration test, не утверждение о синхронности настоящих декодеров.

- [x] **Step 2: RED.** `cargo test --locked -p media-stream --test revocation --test backpressure --test workflow`. Если найдены ошибки предыдущих задач, воспроизвести тестом перед исправлением; не подменять ожидания успешным stub.
- [x] **Step 3: Реализовать завершение и harness.** Barrier hooks и instrumentation только под тестовой feature. Составной owner `LocalRoomSession` в `media-stream/src/http.rs` управляет RoomHandle, source и LoopbackServer: `shutdown(self)` сначала отзывает room, отменяет readers, ожидает HTTP tasks, затем освобождает source. RoomHandle::close отзывает authority, но не владеет HTTP/файлами; именно LocalRoomSession гарантирует полный lifecycle. Установить таймаут ожидания2 s и возвращать явную ошибку при незавершённой задаче, не оставлять detached readers.
- [x] **Step 4: GREEN и CI.** Из корня: `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`; `cargo test --locked --workspace --all-features`; `cargo build --locked --workspace --all-targets`. CI запускает эти же команды; Linux обязателен, Windows/macOS core-test matrix проверяет переносимость Rust-ядра, не выдаётся за native UI-тест. Зафиксировать фактическое число tests passed/failed/skipped. Sparse/native ограничения объяснить отдельно.
- [x] **Step 5: Проверить повторяемый старт и сохранить инструкции среды.** Дважды запустить harness с сгенерированным временным fixture, дождаться authenticated HEAD readiness, штатно остановить каждый экземпляр; процесс не переживает рестарт среды. В `docs/development/core.md` записать PATH/CARGO_HOME/RUSTUP_HOME и точные команды. Через environment draft сохранить проверенный `install_script` (активация+setup+locked build) и `start_skill` (использовать существующий checkout, не worktree; команды tests/harness; readiness; никакого ожидания сохранённого процесса). Публикация и применение draft пользователем не считаются выполненными агентом.
- [x] **Step 6: Commit и передача в B/C.** `feat: verify core lifecycle and development workflow`. Отчёт перечисляет что проверено локально и какие native/network gates остаются. Не начинать произвольный UI вместо технических проверок.

## Контроль зависимостей и выполнения

Порядок A1→A2→A3→A4→A5→A6. У каждого исполнителя свежий контекст и ссылки на spec/plan/ECC-role; сначала проверяется соответствие задаче, потом качество. Общие типы меняются владельцем Task1 и сопровождаются wire regression tests. Interfaces каждого владельца задаёт единственный контракт, последующие задачи не вводят другой вариант той же сигнатуры.

Чистая среда A требует только Rust и системного C linker. Если linker отсутствует, это отдельная подтверждаемая предпосылка: установить стандартный build-essential через поддержанный пакетный менеджер с проверкой подписей. libmpv, GTK, WebKit, frontend и WebRTC не устанавливать для A. Настройки signing, cloud accounts, TURN и production endpoints пока не требуются.

## Результат review плана

Архитектура и план подтверждены пользователем до реализации. Метод: агенты с ролями ECC и task review по Superpowers. Все шесть задач реализованы; [отчёт проверок и уточнений](../../development/core-validation.md) отделяет локальные результаты от ещё не выполненной native/network приёмки.

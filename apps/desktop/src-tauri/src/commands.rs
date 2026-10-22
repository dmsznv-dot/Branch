use crate::state::{PlayerCommandDto, PlayerController, PlayerView, PublicError};
use crate::surface::NativeSurface;
use media_engine::EngineError;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::path::PathBuf;
use network::{signaling_router, EmbeddedSignalServer, GuestSession, HostSession, JoinInvitation, RouterPortLease, SignalHub, WatchError};
use room_server::RoomHandle;
use shared_protocol::{ClockEpochId, MediaId, PlaybackCommand, PlaybackStatus, RoomSnapshot};
use sync_engine::MonotonicClock;
use tauri::{Emitter, Manager, State, Window};

pub enum DesktopWatchSession {
    Host { session: HostSession, _server: EmbeddedSignalServer, _port_lease: RouterPortLease },
    Guest(GuestSession),
}
pub struct AppState {
    pub controller: Arc<tokio::sync::Mutex<PlayerController>>,
    pub surface: Mutex<Option<NativeSurface>>,
    pub closing: AtomicBool,
    pub close: tokio::sync::watch::Sender<bool>,
    pub chooser: AtomicBool,
    pub selected_path: Mutex<Option<PathBuf>>,
    pub watch_session: tokio::sync::Mutex<Option<DesktopWatchSession>>,
}
fn main_window<R: tauri::Runtime>(window: &Window<R>, state: &AppState) -> Result<(), PublicError> {
    if window.label() != "main" {
        return Err(EngineError::InvalidCommand.into());
    }
    if state.closing.load(Ordering::Acquire) {
        return Err(EngineError::Cancelled.into());
    }
    Ok(())
}
#[tauri::command]
pub async fn choose_media<R: tauri::Runtime>(
    window: Window<R>,
    state: State<'_, AppState>,
) -> Result<Option<PlayerView>, PublicError> {
    main_window(&window, &state)?;
    if state.watch_session.lock().await.is_some() {
        return Err(EngineError::Busy.into());
    }
    if state.chooser.swap(true, Ordering::AcqRel) {
        return Err(EngineError::Busy.into());
    }
    struct Choosing<'a>(&'a AtomicBool);
    impl Drop for Choosing<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let choosing = Choosing(&state.chooser);
    // The dialog and descriptor open live in Rust. No caller-supplied path exists.
    let file = rfd::AsyncFileDialog::new()
        .add_filter("Фильмы", &["mp4", "mov", "mkv"])
        .pick_file()
        .await;
    drop(choosing);
    main_window(&window, &state)?;
    let Some(file) = file else { return Ok(None) };
    let selected_path = file.path().to_owned();
    let selected = media_engine::SelectedFile::open(file.path())?;
    let (serial, basename, request) = state.controller.lock().await.begin_open(selected)?;
    // The owned reply contains no borrowed native handle. A second selection
    // can enqueue its load while this request awaits FILE_LOADED.
    let result = crate::state::until_closed(state.close.subscribe(), request.wait()).await;
    main_window(&window, &state)?;
    let (result, view) = {
        let mut controller = state.controller.lock().await;
        let result = controller.finish_open(serial, basename, result);
        (result, controller.view.clone())
    };
    let _ = window.emit("player-state", view);
    if result.is_ok() {
        *state.selected_path.lock().unwrap_or_else(|p| p.into_inner()) = Some(selected_path);
    }
    result.map(Some).map_err(Into::into)
}

fn watch_error(error: WatchError) -> PublicError {
    let _ = error;
    media_engine::EngineError::Unavailable.into()
}

async fn selected_descriptor(state: &AppState, include_duration: bool) -> Result<shared_protocol::MediaDescriptor, PublicError> {
    let path = state.selected_path.lock().unwrap_or_else(|p| p.into_inner()).clone()
        .ok_or_else(|| media_engine::EngineError::InvalidCommand)?;
    let duration = if include_duration {
        state.controller.lock().await.view.duration_ms.map_or(0, |value| value.get())
    } else { 0 };
    tauri::async_runtime::spawn_blocking(move || {
        let source = media_stream::SelectedMedia::open(&path, MediaId::new())
            .map_err(|_| media_engine::EngineError::Unavailable)?;
        Ok::<_, media_engine::EngineError>(source.descriptor(duration))
    }).await.map_err(|_| media_engine::EngineError::Unavailable)?.map_err(Into::into)
}

#[tauri::command]
pub async fn watch_create<R: tauri::Runtime>(
    window: Window<R>, state: State<'_, AppState>,
) -> Result<String, String> {
    main_window(&window, &state).map_err(|_| "Окно Branch недоступно.".to_owned())?;
    let mut session = state.watch_session.lock().await;
    if session.is_some() { return Err("Сначала завершите текущую комнату.".into()); }
    let media = selected_descriptor(&state, true).await.map_err(|_| "Сначала откройте видеофайл.".to_owned())?;
    let created = RoomHandle::create(media, "Хост", Arc::new(MonotonicClock::new(ClockEpochId::new())))
        .map_err(|_| "Не удалось создать комнату.".to_owned())?;
    let (public_host, port_lease) = RouterPortLease::open().await?;
    let server = EmbeddedSignalServer::bind(8787, &public_host, signaling_router(Arc::new(SignalHub::new())))
        .await?;
    let public_endpoint = format!("wss://{public_host}:8787/v1/signal");
    let host = HostSession::start_embedded(&public_endpoint, "wss://localhost:8787/v1/signal", server.certificate(), created)
        .await.map_err(|_| "Порт открыт, но Branch не смогла запустить защищённую комнату. Попробуйте ещё раз.".to_owned())?;
    let share = host.invitation.to_share_string().map_err(|_| "Не удалось сформировать ссылку приглашения.".to_owned())?;
    follow_host_snapshots(window.clone(), host.snapshots.clone());
    *session = Some(DesktopWatchSession::Host { session: host, _server: server, _port_lease: port_lease });
    Ok(share)
}

#[tauri::command]
pub async fn watch_join<R: tauri::Runtime>(
    window: Window<R>, state: State<'_, AppState>, invitation: String,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
    let mut session = state.watch_session.lock().await;
    if session.is_some() { return Err(media_engine::EngineError::Busy.into()); }
    let invitation = JoinInvitation::parse_share_string(&invitation)
        .map_err(|_| media_engine::EngineError::InvalidCommand)?;
    let media = selected_descriptor(&state, false).await?;
    let guest = GuestSession::join(invitation, media.fingerprint).await.map_err(watch_error)?;
    follow_guest_snapshots(window.clone(), guest.snapshots.clone());
    *session = Some(DesktopWatchSession::Guest(guest));
    Ok(())
}

#[tauri::command]
pub async fn watch_playback<R: tauri::Runtime>(
    window: Window<R>, state: State<'_, AppState>, action: PlaybackCommand,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
    let session = state.watch_session.lock().await;
    match session.as_ref() {
        Some(DesktopWatchSession::Host { session: host, .. }) => host.command(action).await.map_err(watch_error),
        Some(DesktopWatchSession::Guest(guest)) => guest.command(action).await.map_err(watch_error),
        None => Err(media_engine::EngineError::Unavailable.into()),
    }
}

#[tauri::command]
pub async fn watch_leave<R: tauri::Runtime>(
    window: Window<R>, state: State<'_, AppState>,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
    let session = state.watch_session.lock().await.take();
    if let Some(DesktopWatchSession::Host { session: host, .. }) = session { host.close().await; }
    Ok(())
}

#[tauri::command]
pub async fn watch_status<R: tauri::Runtime>(
    window: Window<R>, state: State<'_, AppState>,
) -> Result<String, PublicError> {
    main_window(&window, &state)?;
    let session = state.watch_session.lock().await;
    Ok(match session.as_ref() {
        Some(DesktopWatchSession::Host { session: host, .. }) if host.is_active() => "host".to_owned(),
        Some(DesktopWatchSession::Guest(guest)) if guest.is_active() => "guest".to_owned(),
        Some(_) => "disconnected".to_owned(),
        None => "idle".to_owned(),
    })
}

fn follow_host_snapshots<R: tauri::Runtime>(window: Window<R>, mut snapshots: tokio::sync::watch::Receiver<RoomSnapshot>) {
    tauri::async_runtime::spawn(async move {
        let first = *snapshots.borrow();
        apply_snapshot(window.clone(), first).await;
        while snapshots.changed().await.is_ok() {
            let next = *snapshots.borrow_and_update();
            apply_snapshot(window.clone(), next).await;
        }
    });
}

fn follow_guest_snapshots<R: tauri::Runtime>(window: Window<R>, mut snapshots: tokio::sync::watch::Receiver<Option<RoomSnapshot>>) {
    tauri::async_runtime::spawn(async move {
        loop {
            if snapshots.changed().await.is_err() { break; }
            let next = *snapshots.borrow_and_update();
            if let Some(snapshot) = next { apply_snapshot(window.clone(), snapshot).await; }
        }
    });
}

async fn apply_snapshot<R: tauri::Runtime>(window: Window<R>, snapshot: RoomSnapshot) {
    let state = window.state::<AppState>();
    let controller = state.controller.clone();
    let close = state.close.subscribe();
    let generation = controller.lock().await.view.generation.clone();
    let paused = snapshot.playback.status != PlaybackStatus::Playing;
    let speed = snapshot.playback.speed;
    let position_ms = snapshot.playback.position_ms;
    let label = window.label().to_owned();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(crate::state::until_closed(close, async move {
            let mut controller = controller.blocking_lock();
            for command in [
                PlayerCommandDto::SetSpeed(speed),
                PlayerCommandDto::SeekToMs(position_ms),
                PlayerCommandDto::SetPaused(paused),
            ] {
                controller.command(&label, &generation, command).await?;
            }
            Ok::<(), EngineError>(())
        }))
    }).await;
}

#[tauri::command]
pub async fn player_command<R: tauri::Runtime>(
    window: Window<R>,
    state: State<'_, AppState>,
    generation: String,
    command: PlayerCommandDto,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
    let shared_action = match &command {
        PlayerCommandDto::SetPaused(true) => Some(PlaybackCommand::Pause),
        PlayerCommandDto::SetPaused(false) => Some(PlaybackCommand::Play),
        PlayerCommandDto::SeekToMs(position_ms) => Some(PlaybackCommand::Seek { position_ms: *position_ms }),
        PlayerCommandDto::SetSpeed(speed) => Some(PlaybackCommand::SetSpeed { speed: *speed }),
        _ => None,
    };
    if let Some(action) = shared_action {
        let watch_session = state.watch_session.lock().await;
        match watch_session.as_ref() {
            Some(DesktopWatchSession::Host { session: host, .. }) => return host.command(action).await.map_err(watch_error),
            Some(DesktopWatchSession::Guest(guest)) => return guest.command(action).await.map_err(watch_error),
            None => {}
        }
    }
    let controller = state.controller.clone();
    let label = window.label().to_owned();
    // EngineHandle is Send but deliberately not Sync. Its borrowed async API is
    // polled on this owned blocking thread, never made Send with an unsafe impl.
    let close = state.close.subscribe();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(crate::state::until_closed(
            close,
            controller
                .blocking_lock()
                .command(&label, &generation, command),
        ))
    })
    .await
    .map_err(|_| PublicError::from(EngineError::Unavailable))?
    .map_err(Into::into)
}
#[tauri::command]
pub async fn set_fullscreen<R: tauri::Runtime>(
    window: Window<R>,
    state: State<'_, AppState>,
    value: bool,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
    window
        .set_fullscreen(value)
        .map_err(|_| EngineError::Unavailable.into())
}
#[tauri::command]
pub async fn player_snapshot<R: tauri::Runtime>(
    window: Window<R>,
    state: State<'_, AppState>,
) -> Result<PlayerView, PublicError> {
    main_window(&window, &state)?;
    Ok(state.controller.lock().await.view.clone())
}

#[cfg(all(test, feature = "ipc-test"))]
mod acl_tests {
    use super::*;
    use tauri::{
        ipc::{CallbackFn, InvokeBody},
        webview::InvokeRequest,
    };
    #[test]
    fn generated_manifest_and_actual_ipc_deny_other_windows_and_remote_origins() {
        let app = tauri::test::mock_builder()
            .manage(AppState {
                controller: Arc::new(tokio::sync::Mutex::new(PlayerController::unavailable())),
                surface: Mutex::new(None),
                closing: AtomicBool::new(false),
                close: tokio::sync::watch::channel(false).0,
                chooser: AtomicBool::new(false),
                selected_path: Mutex::new(None),
                watch_session: tokio::sync::Mutex::new(None),
            })
            .invoke_handler(tauri::generate_handler![
                choose_media,
                watch_create,
                watch_join,
                watch_playback,
                watch_leave,
                watch_status,
                player_command,
                set_fullscreen,
                player_snapshot
            ])
            .build(tauri::generate_context!())
            .unwrap();
        let data = std::env::temp_dir().join(format!(
            "branch-ipc-{}",
            media_engine::PlaybackGeneration::new()
        ));
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .data_directory(data.clone())
            .build()
            .unwrap();
        let other = tauri::WebviewWindowBuilder::new(&app, "other", Default::default())
            .data_directory(data.clone())
            .build()
            .unwrap();
        let request = |cmd: &str, url: &str, body: serde_json::Value| InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        };
        let local = if cfg!(windows) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        };
        let view = tauri::test::get_ipc_response(
            &main,
            request("player_snapshot", local, serde_json::json!({})),
        )
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
        assert_eq!(view["unavailable"], true);
        let denied = tauri::test::get_ipc_response(
            &other,
            request("player_snapshot", local, serde_json::json!({})),
        )
        .unwrap_err();
        assert!(denied.to_string().contains("not allowed"), "{denied}");
        let denied = tauri::test::get_ipc_response(
            &main,
            request(
                "player_snapshot",
                "https://example.com",
                serde_json::json!({}),
            ),
        )
        .unwrap_err();
        assert!(denied.to_string().contains("not allowed"), "{denied}");
        for command in [
            "open_path",
            "mpv_command",
            "plugin:fs|read_file",
            "plugin:shell|execute",
        ] {
            assert!(
                tauri::test::get_ipc_response(
                    &main,
                    request(command, local, serde_json::json!({}))
                )
                .is_err()
            );
        }
        assert!(tauri::test::get_ipc_response(&main,request("player_command",local,serde_json::json!({"generation":"A","command":{"type":"loadfile","value":"secret"}}))).is_err());
        drop(other);
        drop(main);
        drop(app);
        let _ = std::fs::remove_dir_all(data);
    }
}

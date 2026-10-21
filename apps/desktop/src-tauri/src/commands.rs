use crate::state::{PlayerCommandDto, PlayerController, PlayerView, PublicError};
use crate::surface::NativeSurface;
use media_engine::EngineError;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Emitter, State, Window};
pub struct AppState {
    pub controller: Arc<tokio::sync::Mutex<PlayerController>>,
    pub surface: Mutex<Option<NativeSurface>>,
    pub closing: AtomicBool,
    pub close: tokio::sync::watch::Sender<bool>,
    pub chooser: AtomicBool,
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
    result.map(Some).map_err(Into::into)
}
#[tauri::command]
pub async fn player_command<R: tauri::Runtime>(
    window: Window<R>,
    state: State<'_, AppState>,
    generation: String,
    command: PlayerCommandDto,
) -> Result<(), PublicError> {
    main_window(&window, &state)?;
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
            })
            .invoke_handler(tauri::generate_handler![
                choose_media,
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

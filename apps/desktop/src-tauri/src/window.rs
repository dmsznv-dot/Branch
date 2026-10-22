//! All use of Tauri's pinned unstable child-WebView API stays in this module.
use crate::surface::NativeSurface;
use crate::{
    commands::{self, AppState},
    state::PlayerController,
    surface::geometry::SurfaceGeometry,
};
use media_engine::EngineError;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{Emitter, Manager, Window, WindowEvent};
fn native_geometry(window: &Window) -> Result<SurfaceGeometry, EngineError> {
    let scale = window
        .scale_factor()
        .map_err(|_| EngineError::Unavailable)?;
    let size = window.inner_size().map_err(|_| EngineError::Unavailable)?;
    let minimized = window.is_minimized().unwrap_or(false);
    Ok(SurfaceGeometry {
        width: if minimized {
            0.
        } else {
            f64::from(size.width) / scale
        },
        height: if minimized {
            0.
        } else {
            f64::from(size.height) / scale
        },
        scale,
    })
}
fn begin_close(window: &Window) {
    let state = window.state::<AppState>();
    if state.closing.swap(true, Ordering::AcqRel) {
        return;
    }
    state.close.send_replace(true);
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        let state = window.state::<AppState>();
        let watch_session = state.watch_session.lock().await.take();
        if let Some(commands::DesktopWatchSession::Host { session: host, .. }) = watch_session {
            host.close().await;
        }
        let engine = state.controller.lock().await.engine.take();
        let surface = state
            .surface
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        // Poll engine shutdown and surface shutdown together: engine first cancels
        // any source read while GL is still alive, then waits for the render lease.
        // Both use the same bounded five-second policy and retain live resources on
        // failure. The visible native parent is never released after a failed join.
        let engine_shutdown = async {
            if let Some(engine) = engine {
                engine.shutdown().await
            } else {
                Ok(())
            }
        };
        let surface_shutdown = async {
            if let Some(surface) = surface {
                surface.shutdown().await
            } else {
                Ok(())
            }
        };
        let (core, render) = tokio::join!(engine_shutdown, surface_shutdown);
        if core.is_ok() && render.is_ok() && !NativeSurface::has_retained_resources() {
            let close = window.clone();
            let _ = window.run_on_main_thread(move || {
                let _ = close.destroy();
                close.app_handle().exit(0);
            });
        } else {
            let mut controller = state.controller.lock().await;
            controller.view.status = "Failed";
            let _ = window.emit("player-state", controller.view.clone());
            // Closing remains set. Repeated close is idempotent; no unsafe retry can
            // destroy a retained native hierarchy. Process exit remains OS-controlled.
        }
    });
}
pub fn run() {
    let app = tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::choose_media,
            commands::watch_create,
            commands::watch_join,
            commands::watch_playback,
            commands::watch_leave,
            commands::watch_status,
            commands::player_command,
            commands::set_fullscreen,
            commands::player_snapshot
        ])
        .setup(|app| {
            let window = tauri::window::WindowBuilder::new(app, "main")
                .title("Branch — локальный просмотр")
                .inner_size(1100., 700.)
                .min_inner_size(640., 420.)
                .visible(false)
                .background_color(tauri::window::Color(8, 9, 11, 255))
                .build()?;
            #[allow(unused_mut)]
            let mut surface = None;
            #[allow(unused_mut)]
            let mut controller = PlayerController::unavailable();
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                if let Ok(mut engine) = tauri::async_runtime::block_on(
                    media_engine::EngineHandle::start(media_engine::EngineMode::Native),
                ) {
                    match engine
                        .take_render_endpoint()
                        .and_then(|endpoint| NativeSurface::attach(&window, endpoint))
                    {
                        Ok(attached) => {
                            surface = Some(attached);
                            controller = PlayerController::new(engine);
                        }
                        Err(_) => {
                            tauri::async_runtime::spawn(async move {
                                let _ = engine.shutdown().await;
                            });
                        }
                    }
                }
            }
            let errors = surface.as_ref().map(NativeSurface::errors);
            let mut snapshots = controller.engine.as_ref().map(|engine| engine.subscribe());
            app.manage(AppState {
                controller: Arc::new(tokio::sync::Mutex::new(controller)),
                surface: Mutex::new(surface),
                closing: AtomicBool::new(false),
                close: tokio::sync::watch::channel(false).0,
                chooser: AtomicBool::new(false),
                selected_path: Mutex::new(None),
                watch_session: tokio::sync::Mutex::new(None),
            });
            let webview = tauri::webview::WebviewBuilder::new(
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .transparent(true)
            .auto_resize()
            .on_navigation(|url| {
                // The production privileged view accepts only Tauri's local asset origin.
                if url.scheme() == "tauri" && url.host_str() == Some("localhost") {
                    return true;
                }
                if url.scheme() == "http" && url.host_str() == Some("tauri.localhost") {
                    return true;
                }
                #[cfg(debug_assertions)]
                if url.scheme() == "http"
                    && url.host_str() == Some("127.0.0.1")
                    && url.port() == Some(1420)
                {
                    return true;
                }
                false
            });
            let size = window.inner_size()?;
            window.add_child(webview, tauri::LogicalPosition::new(0., 0.), size)?;
            window.show()?;
            let events = window.clone();
            window.on_window_event(move |event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    begin_close(&events);
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if let Ok(geometry) = native_geometry(&events) {
                        let state = events.state::<AppState>();
                        if let Some(surface) = state
                            .surface
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_ref()
                        {
                            let _ = surface.resize(geometry);
                        }
                    }
                }
                _ => {}
            });
            if let Some(mut errors) = errors {
                let events = window.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        if errors.borrow_and_update().is_some() {
                            let state = events.state::<AppState>();
                            let mut controller = state.controller.lock().await;
                            controller.surface_unavailable();
                            let _ = events.emit("player-state", controller.view.clone());
                            break;
                        }
                        if errors.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
            if let Some(mut snapshots) = snapshots.take() {
                let events = window.clone();
                tauri::async_runtime::spawn(async move {
                    while snapshots.changed().await.is_ok() {
                        let snapshot = snapshots.borrow_and_update().clone();
                        let state = events.state::<AppState>();
                        let mut controller = state.controller.lock().await;
                        if controller.update(&snapshot) {
                            let _ = events.emit("player-state", controller.view.clone());
                        }
                    }
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("desktop initialization unavailable");
    app.run(|handle, event| {
        if let tauri::RunEvent::ExitRequested { code, api, .. } = event
            && code.is_none()
            && let Some(window) = handle.get_window("main")
        {
            api.prevent_exit();
            begin_close(&window);
        }
    });
}

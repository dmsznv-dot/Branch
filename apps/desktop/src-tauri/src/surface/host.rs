use super::geometry::{PhysicalSize, SurfaceGeometry};
#[cfg(target_os = "macos")]
use super::macos as platform;
#[cfg(target_os = "windows")]
use super::windows as platform;
use media_engine::{EngineError, MpvRenderer, RenderEndpoint, RenderTarget};
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};
static RETAINED_NATIVE: AtomicBool = AtomicBool::new(false);
struct Wake {
    cancelled: AtomicBool,
    size: Mutex<Option<PhysicalSize>>,
    changed: Condvar,
}
/// Native handles are opaque integer tokens, used only by their documented UI
/// or GL thread. No unsafe Send/Sync implementation crosses the FFI boundary.
/// This owner and its Window are deliberately retained if shutdown times out.
pub struct NativeSurface {
    window: Option<tauri::Window>,
    native: Option<platform::Native>,
    wake: Arc<Wake>,
    thread: Option<JoinHandle<()>>,
    done: Option<tokio::sync::oneshot::Receiver<()>>,
    errors: tokio::sync::watch::Receiver<Option<EngineError>>,
}
impl NativeSurface {
    pub fn errors(&self) -> tokio::sync::watch::Receiver<Option<EngineError>> {
        self.errors.clone()
    }
    pub fn has_retained_resources() -> bool {
        RETAINED_NATIVE.load(Ordering::Acquire)
    }
    /// Native hierarchy creation is always dispatched to Tauri's UI thread.
    pub fn attach(window: &tauri::Window, endpoint: RenderEndpoint) -> Result<Self, EngineError> {
        let ui_window = window.clone();
        on_ui(window, move || Self::attach_ui(&ui_window, endpoint))?
    }
    fn attach_ui(window: &tauri::Window, endpoint: RenderEndpoint) -> Result<Self, EngineError> {
        let scale = window
            .scale_factor()
            .map_err(|_| EngineError::Unavailable)?;
        let physical = window.inner_size().map_err(|_| EngineError::Unavailable)?;
        let geometry = SurfaceGeometry {
            width: f64::from(physical.width) / scale,
            height: f64::from(physical.height) / scale,
            scale,
        };
        let size = geometry.physical()?;
        let native = platform::Native::attach(window, geometry)?;
        let wake = Arc::new(Wake {
            cancelled: AtomicBool::new(false),
            size: Mutex::new(size),
            changed: Condvar::new(),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done) = tokio::sync::oneshot::channel();
        let (error_tx, errors) = tokio::sync::watch::channel(None);
        let thread_wake = wake.clone();
        let token = native.token();
        let thread = match std::thread::Builder::new()
            .name("branch-video".into())
            .spawn(move || {
                // The GL owner is created inside this closure, never transferred after a
                // context is made current. Renderer Drop precedes GL owner Drop on all paths.
                let result = platform::Gl::current(token).and_then(|gl| {
                    // SAFETY: platform owner just made its compatible context current; its
                    // genuine system resolver is retained, and gl outlives the renderer below.
                    let mut renderer = unsafe { MpvRenderer::create(endpoint, platform::resolve) }?;
                    let _ = ready_tx.send(Ok(()));
                    let mut outcome = Ok(());
                    while !thread_wake.cancelled.load(Ordering::Acquire) {
                        let size = *thread_wake.size.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(size) = size {
                            gl.lock();
                            let rendered = renderer
                                .render(RenderTarget {
                                    fbo: 0,
                                    width: size.width as i32,
                                    height: size.height as i32,
                                    flip_y: true,
                                })
                                .and_then(|()| gl.swap());
                            if rendered.is_ok() {
                                renderer.report_swap();
                            }
                            gl.unlock();
                            if let Err(error) = rendered {
                                outcome = Err(error);
                                break;
                            }
                        }
                        let size = thread_wake.size.lock().unwrap_or_else(|p| p.into_inner());
                        if !thread_wake.cancelled.load(Ordering::Acquire) {
                            let _ = thread_wake
                                .changed
                                .wait_timeout(size, Duration::from_millis(16));
                        }
                    }
                    // Context remains current and owned while mpv frees callbacks/GL objects.
                    gl.lock();
                    drop(renderer);
                    gl.unlock();
                    drop(gl);
                    outcome
                });
                if let Err(error) = result {
                    error_tx.send_replace(Some(error));
                    let _ = ready_tx.send(Err(error));
                }
                let _ = done_tx.send(());
            }) {
            Ok(thread) => thread,
            Err(_) => {
                native.release();
                return Err(EngineError::Unavailable);
            }
        };
        let surface = Self {
            window: Some(window.clone()),
            native: Some(native),
            wake,
            thread: Some(thread),
            done: Some(done),
            errors,
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(surface),
            Ok(Err(error)) => {
                surface.dispose_after_failed_start();
                Err(error)
            }
            Err(_) => {
                surface.wake.cancelled.store(true, Ordering::Release);
                surface.wake.changed.notify_all();
                RETAINED_NATIVE.store(true, Ordering::Release);
                std::mem::forget(surface);
                Err(EngineError::ShutdownTimeout)
            }
        }
    }
    fn dispose_after_failed_start(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(native) = self.native.take() {
            native.release();
        }
    }
    pub fn resize(&self, geometry: SurfaceGeometry) -> Result<(), EngineError> {
        let size = geometry.physical()?;
        let native = *self.native.as_ref().ok_or(EngineError::Cancelled)?;
        on_ui(
            self.window.as_ref().ok_or(EngineError::Cancelled)?,
            move || native.resize(geometry),
        )??;
        *self.wake.size.lock().unwrap_or_else(|p| p.into_inner()) = size;
        self.wake.changed.notify_all();
        Ok(())
    }
    pub async fn shutdown(mut self) -> Result<(), EngineError> {
        let mut order = super::lifecycle::CloseSequence::default();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let result = tokio::time::timeout_at(deadline, order.close(&mut self))
            .await
            .unwrap_or(Err(EngineError::ShutdownTimeout));
        if let Err(error) = result {
            // No caller may release this window/DC/view/context while native code can
            // still touch them, including a failed render join or main-thread dispatch.
            RETAINED_NATIVE.store(true, Ordering::Release);
            std::mem::forget(self);
            return Err(error);
        }
        Ok(())
    }
}
impl super::lifecycle::CloseActions for NativeSurface {
    fn cancel(&mut self) {
        self.wake.cancelled.store(true, Ordering::Release);
        self.wake.changed.notify_all();
    }
    async fn join_render(&mut self) -> Result<(), EngineError> {
        let done = self.done.take().ok_or(EngineError::Cancelled)?;
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .map_err(|_| EngineError::ShutdownTimeout)?
            .map_err(|_| EngineError::DecoderFailed)?;
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| EngineError::DecoderFailed)?;
        }
        Ok(())
    }
    async fn destroy_gl(&mut self) -> Result<(), EngineError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let native = self.native.take().ok_or(EngineError::Cancelled)?;
        // The render thread has dropped MpvRenderer and detached its GL context.
        // Cocoa drawable/context and native hierarchy release is UI-thread-only.
        self.window
            .as_ref()
            .ok_or(EngineError::Cancelled)?
            .run_on_main_thread(move || {
                native.release();
                let _ = tx.send(());
            })
            .map_err(|_| EngineError::Unavailable)?;
        rx.await.map_err(|_| EngineError::Unavailable)
    }
    fn release_window(&mut self) {
        drop(self.window.take());
    }
}

fn on_ui<T: Send + 'static>(
    window: &tauri::Window,
    action: impl FnOnce() -> T + Send + 'static,
) -> Result<T, EngineError> {
    let (tx, rx) = mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = tx.send(action());
        })
        .map_err(|_| EngineError::Unavailable)?;
    rx.recv().map_err(|_| EngineError::Unavailable)
}

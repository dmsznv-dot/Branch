//! Development-only surfaceless EGL proof; this is not the product's Linux
//! surface, nor evidence for CGL/WGL, WebView composition or native bundles.
#![cfg(all(feature = "native-mpv", target_os = "linux"))]
use media_engine::{EngineHandle, EngineMode, MpvRenderer, RenderTarget, SelectedFile};
use std::{
    ffi::{CStr, c_char, c_void},
    path::Path,
    time::{Duration, Instant},
};
type Ptr = *mut c_void;
#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetPlatformDisplay(platform: u32, native: Ptr, attrs: *const isize) -> Ptr;
    fn eglInitialize(display: Ptr, major: *mut i32, minor: *mut i32) -> u32;
    fn eglBindAPI(api: u32) -> u32;
    fn eglChooseConfig(
        display: Ptr,
        attrs: *const i32,
        config: *mut Ptr,
        size: i32,
        count: *mut i32,
    ) -> u32;
    fn eglCreatePbufferSurface(display: Ptr, config: Ptr, attrs: *const i32) -> Ptr;
    fn eglCreateContext(display: Ptr, config: Ptr, share: Ptr, attrs: *const i32) -> Ptr;
    fn eglMakeCurrent(display: Ptr, draw: Ptr, read: Ptr, context: Ptr) -> u32;
    fn eglGetProcAddress(name: *const c_char) -> *const c_void;
    fn eglDestroyContext(display: Ptr, context: Ptr) -> u32;
    fn eglDestroySurface(display: Ptr, surface: Ptr) -> u32;
    fn eglTerminate(display: Ptr) -> u32;
}
struct Egl {
    display: Ptr,
    context: Ptr,
    surface: Ptr,
}
impl Egl {
    fn create() -> Self {
        // SAFETY: known EGL constants; null native display is correct for the
        // MESA surfaceless platform. All outputs point into initialized locals.
        unsafe {
            let display = eglGetPlatformDisplay(0x31DD, std::ptr::null_mut(), std::ptr::null());
            assert!(!display.is_null(), "egl_display");
            let mut owner = Self {
                display,
                context: std::ptr::null_mut(),
                surface: std::ptr::null_mut(),
            };
            let (mut major, mut minor) = (0, 0);
            assert_ne!(
                eglInitialize(display, &mut major, &mut minor),
                0,
                "egl_initialize"
            );
            assert!((major, minor) >= (1, 5));
            assert_ne!(eglBindAPI(0x30A2), 0, "egl_bind_opengl");
            let attrs = [
                0x3033, 1, 0x3040, 8, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3038,
            ];
            let (mut config, mut count) = (std::ptr::null_mut(), 0);
            assert_ne!(
                eglChooseConfig(display, attrs.as_ptr(), &mut config, 1, &mut count),
                0
            );
            assert_eq!(count, 1, "egl_config");
            let surface_attrs = [0x3057, 160, 0x3056, 90, 0x3038];
            owner.surface = eglCreatePbufferSurface(display, config, surface_attrs.as_ptr());
            assert!(!owner.surface.is_null(), "egl_pbuffer");
            let context_attrs = [0x3098, 3, 0x30FB, 3, 0x30FD, 1, 0x3038];
            owner.context = eglCreateContext(
                display,
                config,
                std::ptr::null_mut(),
                context_attrs.as_ptr(),
            );
            assert!(!owner.context.is_null(), "egl_context");
            assert_ne!(
                eglMakeCurrent(display, owner.surface, owner.surface, owner.context),
                0,
                "egl_current"
            );
            owner
        }
    }
}
impl Drop for Egl {
    fn drop(&mut self) {
        // SAFETY: ownership is unique and Drop runs on the same test thread after
        // renderer free. Unbind before destroying surface/context; no users remain.
        unsafe {
            eglMakeCurrent(
                self.display,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if !self.context.is_null() {
                eglDestroyContext(self.display, self.context);
            }
            if !self.surface.is_null() {
                eglDestroySurface(self.display, self.surface);
            }
            eglTerminate(self.display);
        }
    }
}
fn resolve(name: &CStr) -> *const c_void {
    // SAFETY: a current initialized EGL OpenGL context remains on this thread;
    // function names supplied by mpv are terminated and borrowed for this call.
    unsafe { eglGetProcAddress(name.as_ptr()) }
}
#[test]
fn linux_offscreen_render_lifecycle() {
    // Native calls run in a bounded child; the test runner cannot hang forever.
    if std::env::var_os("BRANCH_EGL_PROBE_CHILD").is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "linux_offscreen_render_lifecycle", "--nocapture"])
            .env("BRANCH_EGL_PROBE_CHILD", "1")
            .env("MESA_SHADER_CACHE_DISABLE", "true")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("egl_probe_timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("render_pixels_verified"));
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let egl = Egl::create();
        let mut engine = EngineHandle::start(EngineMode::Native).await.unwrap();
        let endpoint = engine.take_render_endpoint().unwrap();
        // SAFETY: Egl created a current compatible GL context on this thread.
        // resolve returns genuine EGL GL entry points. Current-thread runtime
        // cannot move this renderer; egl remains current/valid until after Drop.
        let mut renderer = unsafe { MpvRenderer::create(endpoint, resolve) }.unwrap();
        let file = SelectedFile::open(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/player-fixtures/h264.mp4"),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), engine.open(file))
            .await
            .unwrap()
            .unwrap();
        let read_ptr = resolve(c"glReadPixels");
        let error_ptr = resolve(c"glGetError");
        assert!(!read_ptr.is_null() && !error_ptr.is_null());
        // SAFETY: EGL returned genuine functions with these standard GL ABIs;
        // context is current and live. Function pointers never outlive egl.
        let read: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, Ptr) =
            unsafe { std::mem::transmute(read_ptr) };
        // SAFETY: same verified EGL GL entry-point and ABI guarantee as above.
        let error: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(error_ptr) };
        // A dedicated texture FBO avoids platform-specific front/back pbuffer
        // semantics and directly exercises the public RenderTarget.fbo contract.
        macro_rules! gl {
            ($name:literal, $ty:ty) => {{
                let ptr = resolve($name);
                assert!(!ptr.is_null());
                // SAFETY: exact standard GL name/ABI below, genuine EGL resolver
                // and same current live GL context for the entire test scope.
                unsafe { std::mem::transmute::<*const c_void, $ty>(ptr) }
            }};
        }
        let gen_fbo = gl!(c"glGenFramebuffers", unsafe extern "C" fn(i32, *mut u32));
        let bind_fbo = gl!(c"glBindFramebuffer", unsafe extern "C" fn(u32, u32));
        let delete_fbo = gl!(
            c"glDeleteFramebuffers",
            unsafe extern "C" fn(i32, *const u32)
        );
        let gen_tex = gl!(c"glGenTextures", unsafe extern "C" fn(i32, *mut u32));
        let bind_tex = gl!(c"glBindTexture", unsafe extern "C" fn(u32, u32));
        let image = gl!(
            c"glTexImage2D",
            unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void)
        );
        let attach = gl!(
            c"glFramebufferTexture2D",
            unsafe extern "C" fn(u32, u32, u32, u32, i32)
        );
        let check = gl!(
            c"glCheckFramebufferStatus",
            unsafe extern "C" fn(u32) -> u32
        );
        let delete_tex = gl!(c"glDeleteTextures", unsafe extern "C" fn(i32, *const u32));
        let (mut fbo, mut texture) = (0, 0);
        // SAFETY: current context owns these newly allocated objects, and texture
        // dimensions match the readback buffer. Data=null allocates storage only.
        unsafe {
            gen_fbo(1, &mut fbo);
            bind_fbo(0x8D40, fbo);
            gen_tex(1, &mut texture);
            bind_tex(0x0DE1, texture);
            image(
                0x0DE1,
                0,
                0x8058,
                160,
                90,
                0,
                0x1908,
                0x1401,
                std::ptr::null(),
            );
            attach(0x8D40, 0x8CE0, 0x0DE1, texture, 0);
            assert_eq!(check(0x8D40), 0x8CD5, "fbo_complete");
            bind_tex(0x0DE1, 0);
            bind_fbo(0x8D40, 0);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pixels = vec![0u8; 160 * 90 * 4];
        loop {
            renderer
                .render(RenderTarget {
                    fbo: fbo as i32,
                    width: 160,
                    height: 90,
                    flip_y: true,
                })
                .unwrap();
            renderer.report_swap();
            // SAFETY: current owned color framebuffer is 160x90; RGBA/UNSIGNED_BYTE
            // writes exactly 160*90*4 bytes into the live writable vector.
            unsafe {
                bind_fbo(0x8D40, fbo);
                read(0, 0, 160, 90, 0x1908, 0x1401, pixels.as_mut_ptr().cast());
                assert_eq!(error(), 0);
                bind_fbo(0x8D40, 0);
            }
            let diagnostics = engine.diagnostics();
            if diagnostics.estimated_frame_number >= 2
                && pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[..3].iter().any(|c| *c > 20))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no_rendered_pixels width={} height={} frames={} max={} position_ms={} state={:?} output={} hwdec={}",
                diagnostics.width,
                diagnostics.height,
                diagnostics.estimated_frame_number,
                pixels.iter().max().unwrap(),
                engine.subscribe().borrow().position_ms,
                engine.subscribe().borrow().status,
                engine.subscribe().borrow().decoder.as_ref().map_or("none", |d| d.video_output.as_str()),
                engine.subscribe().borrow().decoder.as_ref().and_then(|d| d.hwdec.as_deref()).unwrap_or("no")
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            engine
                .subscribe()
                .borrow()
                .decoder
                .as_ref()
                .unwrap()
                .video_output,
            "libmpv"
        );
        drop(renderer); // render context/client freed before core or GL context
        // SAFETY: test owns these GL objects; no renderer references them now,
        // and the compatible EGL context is still current on this same thread.
        unsafe {
            bind_fbo(0x8D40, 0);
            delete_fbo(1, &fbo);
            delete_tex(1, &texture);
        }
        engine.shutdown().await.unwrap(); // actual worker/core joined
        drop(egl);
        println!("render_pixels_verified");
    });
}

use super::ffi::{Client, check, pointer};
use crate::EngineError;
use libmpv2_sys as sys;
use std::{
    ffi::{CStr, c_void},
    marker::PhantomData,
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
};
pub type GlResolver = fn(&CStr) -> *const c_void;
#[derive(Clone, Copy, Debug)]
pub struct RenderTarget {
    pub fbo: i32,
    pub width: i32,
    pub height: i32,
    pub flip_y: bool,
}
#[derive(Default)]
pub(super) struct RenderLease {
    released: Mutex<bool>,
    wake: Condvar,
}
impl RenderLease {
    pub fn wait(&self) {
        let mut released = self.released.lock().unwrap_or_else(|p| p.into_inner());
        while !*released {
            released = self.wake.wait(released).unwrap_or_else(|p| p.into_inner());
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.wake.notify_all();
    }
}
/// A unique client of the same core; may be transferred once to the GL thread.
/// Drop the endpoint/renderer before awaiting engine shutdown. A held endpoint
/// causes ShutdownTimeout; live core/registration are then retained, never freed.
pub struct RenderEndpoint {
    client: Option<Client>,
    lease: Arc<RenderLease>,
}
impl RenderEndpoint {
    pub(super) fn new(client: Client, lease: Arc<RenderLease>) -> Self {
        Self {
            client: Some(client),
            lease,
        }
    }
}
impl Drop for RenderEndpoint {
    fn drop(&mut self) {
        drop(self.client.take());
        self.lease.release();
    }
}
/// Create, render, report_swap and drop on one thread with the same current GL
/// context. Rc marker deliberately prevents Send AND Sync after construction.
pub struct MpvRenderer {
    context: NonNull<sys::mpv_render_context>,
    _endpoint: RenderEndpoint,
    _resolver: Box<GlResolver>,
    _thread: PhantomData<Rc<()>>,
}
impl MpvRenderer {
    /// # Safety
    /// The resolver must return genuine GL entry points with the requested ABI.
    /// A compatible GL context must be current on this thread and remain valid
    /// and current through every render/report_swap call and this renderer's Drop.
    /// Incoming GL state must match mpv render_gl.h's documented standard defaults
    /// (including framebuffer/texture bindings); mpv may alter viewport/scissor,
    /// blend function and clear color state.
    pub unsafe fn create(
        endpoint: RenderEndpoint,
        resolver: GlResolver,
    ) -> Result<Self, EngineError> {
        let mut resolver = Box::new(resolver);
        let mut init = sys::mpv_opengl_init_params {
            get_proc_address: Some(resolve),
            get_proc_address_ctx: pointer(&mut *resolver),
        };
        let mut params = [
            sys::mpv_render_param {
                type_: 1,
                data: c"opengl".as_ptr().cast_mut().cast(),
            },
            sys::mpv_render_param {
                type_: 2,
                data: pointer(&mut init),
            },
            end(),
        ];
        let mut context = std::ptr::null_mut();
        // SAFETY: endpoint owns a live render-client handle. Init params live
        // through create; resolver Box remains allocated until context is freed.
        // Native surface must make the GL context current on this calling thread.
        check(unsafe {
            sys::mpv_render_context_create(
                &mut context,
                endpoint
                    .client
                    .as_ref()
                    .ok_or(EngineError::Unavailable)?
                    .ptr(),
                params.as_mut_ptr(),
            )
        })?;
        Ok(Self {
            context: NonNull::new(context).ok_or(EngineError::DecoderFailed)?,
            _endpoint: endpoint,
            _resolver: resolver,
            _thread: PhantomData,
        })
    }
    pub fn render(&mut self, target: RenderTarget) -> Result<(), EngineError> {
        if target.width <= 0 || target.height <= 0 || target.fbo < 0 {
            return Err(EngineError::InvalidCommand);
        }
        let mut fbo = sys::mpv_opengl_fbo {
            fbo: target.fbo,
            w: target.width,
            h: target.height,
            internal_format: 0,
        };
        let mut flip = i32::from(target.flip_y);
        let mut params = [
            sys::mpv_render_param {
                type_: 3,
                data: pointer(&mut fbo),
            },
            sys::mpv_render_param {
                type_: 4,
                data: pointer(&mut flip),
            },
            end(),
        ];
        // SAFETY: renderer cannot cross threads, owns live context and endpoint;
        // target refers to caller's current GL framebuffer for this call only.
        // SAFETY: process pending renderer dispatch work on the current GL thread
        // before drawing; this never calls back into the event worker or UI.
        unsafe {
            sys::mpv_render_context_update(self.context.as_ptr());
        }
        check(unsafe { sys::mpv_render_context_render(self.context.as_ptr(), params.as_mut_ptr()) })
    }
    pub fn report_swap(&mut self) {
        // SAFETY: same current GL thread, context lives until Drop.
        unsafe { sys::mpv_render_context_report_swap(self.context.as_ptr()) }
    }
}
impl Drop for MpvRenderer {
    fn drop(&mut self) {
        // SAFETY: !Send/!Sync confines context destruction to its creation thread.
        // Caller keeps GL current through drop. Endpoint drops AFTER context free,
        // then releases lease so worker may destroy the final mpv core handle.
        unsafe { sys::mpv_render_context_free(self.context.as_ptr()) }
    }
}
fn end() -> sys::mpv_render_param {
    sys::mpv_render_param {
        type_: 0,
        data: std::ptr::null_mut(),
    }
}
unsafe extern "C" fn resolve(ctx: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: stable resolver Box retained by renderer; mpv passes valid
        // NUL function name for this synchronous resolver invocation.
        let resolver = unsafe { &*ctx.cast::<GlResolver>() };
        resolver(unsafe { CStr::from_ptr(name) }).cast_mut()
    }))
    .unwrap_or(std::ptr::null_mut())
}

use super::geometry::SurfaceGeometry;
use media_engine::EngineError;
use objc::{Encode, Encoding, class, msg_send, runtime::Object, sel, sel_impl};
use std::ffi::{CStr, c_void};
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    origin: Point,
    size: Size,
}
unsafe impl Encode for Rect {
    fn encode() -> Encoding {
        unsafe { Encoding::from_str("{CGRect={CGPoint=dd}{CGSize=dd}}") }
    }
}
#[link(name = "OpenGL", kind = "framework")]
unsafe extern "C" {
    fn CGLSetCurrentContext(context: *mut c_void) -> i32;
    fn CGLLockContext(context: *mut c_void) -> i32;
    fn CGLUnlockContext(context: *mut c_void) -> i32;
    fn CGLFlushDrawable(context: *mut c_void) -> i32;
}
unsafe extern "C" {
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
}
// NSView/NSOpenGLContext retain ownership on UI. Their tokens cross threads;
// only CGL operations use the GL worker, protected against UI drawable updates.
#[derive(Clone, Copy)]
pub struct Native {
    view: usize,
    context: usize,
    cgl: usize,
}
impl Native {
    pub fn attach(window: &tauri::Window, geometry: SurfaceGeometry) -> Result<Self, EngineError> {
        geometry.physical()?;
        let parent = window.ns_view().map_err(|_| EngineError::Unavailable)? as *mut Object;
        // SAFETY: UI-thread-only hierarchy changes; Cocoa owns the parent. Every
        // alloc/init below is retained until the GL worker has dropped its renderer.
        unsafe {
            let frame = Rect {
                origin: Point { x: 0., y: 0. },
                size: Size {
                    width: geometry.width,
                    height: geometry.height,
                },
            };
            let view: *mut Object = msg_send![class!(NSView), alloc];
            let view: *mut Object = msg_send![view,initWithFrame:frame];
            if view.is_null() {
                return Err(EngineError::Unavailable);
            }
            let _: () = msg_send![view,setAutoresizingMask:18usize];
            let _: () = msg_send![view,setWantsBestResolutionOpenGLSurface:true];
            let attributes: [u32; 12] = [99, 0x3200, 8, 24, 11, 8, 12, 24, 5, 0, 0, 0];
            let format: *mut Object = msg_send![class!(NSOpenGLPixelFormat), alloc];
            let format: *mut Object = msg_send![format,initWithAttributes:attributes.as_ptr()];
            if format.is_null() {
                let _: () = msg_send![view, release];
                return Err(EngineError::Unavailable);
            }
            let context: *mut Object = msg_send![class!(NSOpenGLContext), alloc];
            let context: *mut Object = msg_send![context,initWithFormat:format shareContext:std::ptr::null_mut::<Object>()];
            let _: () = msg_send![format, release];
            if context.is_null() {
                let _: () = msg_send![view, release];
                return Err(EngineError::Unavailable);
            }
            // Place the video view below the sibling transparent WKWebView.
            let _: () = msg_send![parent,addSubview:view positioned:-1isize relativeTo:std::ptr::null_mut::<Object>()];
            let _: () = msg_send![context,setView:view];
            let swap: i32 = 1;
            let _: () = msg_send![context,setValues:&swap forParameter:222i32];
            let cgl: *mut c_void = msg_send![context, CGLContextObj];
            if cgl.is_null() {
                let _: () = msg_send![context, clearDrawable];
                let _: () = msg_send![view, removeFromSuperview];
                let _: () = msg_send![context, release];
                let _: () = msg_send![view, release];
                return Err(EngineError::Unavailable);
            }
            Ok(Self {
                view: view as usize,
                context: context as usize,
                cgl: cgl as usize,
            })
        }
    }
    pub fn token(&self) -> usize {
        self.cgl
    }
    pub fn resize(&self, geometry: SurfaceGeometry) -> Result<(), EngineError> {
        geometry.physical()?;
        unsafe {
            CGLLockContext(self.cgl as *mut c_void);
            let frame = Rect {
                origin: Point { x: 0., y: 0. },
                size: Size {
                    width: geometry.width,
                    height: geometry.height,
                },
            };
            let _: () = msg_send![self.view as *mut Object,setFrame:frame];
            let _: () = msg_send![self.context as *mut Object, update];
            CGLUnlockContext(self.cgl as *mut c_void);
        }
        Ok(())
    }
    pub fn release(self) {
        unsafe {
            let _: () = msg_send![self.context as *mut Object, clearDrawable];
            let _: () = msg_send![self.view as *mut Object, removeFromSuperview];
            let _: () = msg_send![self.context as *mut Object, release];
            let _: () = msg_send![self.view as *mut Object, release];
        }
    }
}
pub struct Gl {
    context: *mut c_void,
}
impl Gl {
    pub fn current(token: usize) -> Result<Self, EngineError> {
        let context = token as *mut c_void;
        if unsafe { CGLSetCurrentContext(context) } != 0 {
            return Err(EngineError::Unavailable);
        }
        Ok(Self { context })
    }
    pub fn lock(&self) {
        unsafe {
            CGLLockContext(self.context);
        }
    }
    pub fn unlock(&self) {
        unsafe {
            CGLUnlockContext(self.context);
        }
    }
    pub fn swap(&self) -> Result<(), EngineError> {
        if unsafe { CGLFlushDrawable(self.context) } != 0 {
            Err(EngineError::Unavailable)
        } else {
            Ok(())
        }
    }
}
impl Drop for Gl {
    fn drop(&mut self) {
        unsafe {
            CGLSetCurrentContext(std::ptr::null_mut());
        }
    }
}
pub fn resolve(name: &CStr) -> *const c_void {
    // Resolve strictly from the absolute system OpenGL framework, not process
    // global symbols. The loader handle stays live for every renderer lifetime.
    static FRAMEWORK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let handle = *FRAMEWORK.get_or_init(|| unsafe {
        dlopen(
            c"/System/Library/Frameworks/OpenGL.framework/OpenGL".as_ptr(),
            1 | 4,
        ) as usize
    });
    if handle == 0 {
        return std::ptr::null();
    }
    unsafe { dlsym(handle as *mut c_void, name.as_ptr()).cast_const() }
}

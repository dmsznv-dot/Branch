use super::geometry::SurfaceGeometry;
use media_engine::EngineError;
use std::ffi::{CStr, c_void};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Gdi::*, OpenGL::*},
    System::LibraryLoader::*,
    UI::WindowsAndMessaging::*,
};
// HWND and HDC are owned by the UI; the HGLRC exists only on the GL worker.
#[derive(Clone, Copy)]
pub struct Native {
    hwnd: usize,
    dc: usize,
}
impl Native {
    pub fn attach(window: &tauri::Window, geometry: SurfaceGeometry) -> Result<Self, EngineError> {
        let parent = window.hwnd().map_err(|_| EngineError::Unavailable)?.0;
        let class: Vec<u16> = "BranchNativeVideo\0".encode_utf16().collect();
        let size = geometry
            .physical()?
            .unwrap_or(super::geometry::PhysicalSize {
                width: 0,
                height: 0,
            });
        // SAFETY: executed on UI thread, fixed class/proc and native parent belong to
        // this window. Own DC is released only after renderer/context have exited.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                style: CS_OWNDC,
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                size.width as i32,
                size.height as i32,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err(EngineError::Unavailable);
            }
            let dc = GetDC(hwnd);
            if dc.is_null() {
                DestroyWindow(hwnd);
                return Err(EngineError::Unavailable);
            }
            let format = PIXELFORMATDESCRIPTOR {
                nSize: std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u16,
                nVersion: 1,
                dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
                iPixelType: PFD_TYPE_RGBA,
                cColorBits: 32,
                cAlphaBits: 8,
                cDepthBits: 24,
                iLayerType: PFD_MAIN_PLANE as u8,
                ..std::mem::zeroed()
            };
            let index = ChoosePixelFormat(dc, &format);
            if index == 0 || SetPixelFormat(dc, index, &format) == 0 {
                ReleaseDC(hwnd, dc);
                DestroyWindow(hwnd);
                return Err(EngineError::Unavailable);
            }
            Ok(Self {
                hwnd: hwnd as usize,
                dc: dc as usize,
            })
        }
    }
    pub fn token(&self) -> usize {
        self.dc
    }
    pub fn resize(&self, geometry: SurfaceGeometry) -> Result<(), EngineError> {
        let size = geometry
            .physical()?
            .unwrap_or(super::geometry::PhysicalSize {
                width: 0,
                height: 0,
            });
        // SAFETY: UI thread, retained child, checked GL-sized pixel dimensions.
        if unsafe {
            SetWindowPos(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                0,
                0,
                size.width as i32,
                size.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        } == 0
        {
            Err(EngineError::Unavailable)
        } else {
            Ok(())
        }
    }
    pub fn release(self) {
        unsafe {
            ReleaseDC(self.hwnd as HWND, self.dc as HDC);
            DestroyWindow(self.hwnd as HWND);
        }
    }
}
pub struct Gl {
    dc: HDC,
    context: HGLRC,
}
impl Gl {
    pub fn current(token: usize) -> Result<Self, EngineError> {
        unsafe {
            let dc = token as HDC;
            let context = wglCreateContext(dc);
            if context.is_null() {
                return Err(EngineError::Unavailable);
            }
            if wglMakeCurrent(dc, context) == 0 {
                wglDeleteContext(context);
                return Err(EngineError::Unavailable);
            }
            Ok(Self { dc, context })
        }
    }
    pub fn lock(&self) {}
    pub fn unlock(&self) {}
    pub fn swap(&self) -> Result<(), EngineError> {
        if unsafe { SwapBuffers(self.dc) } == 0 {
            Err(EngineError::Unavailable)
        } else {
            Ok(())
        }
    }
}
impl Drop for Gl {
    fn drop(&mut self) {
        unsafe {
            wglMakeCurrent(std::ptr::null_mut(), std::ptr::null_mut());
            wglDeleteContext(self.context);
        }
    }
}
pub fn resolve(name: &CStr) -> *const c_void {
    unsafe {
        // wglGetProcAddress may return 1,2,3 or -1 on failure. Never pass these as
        // functions to mpv; OpenGL1.1 symbols come from the genuine system DLL.
        let address = wglGetProcAddress(name.as_ptr().cast())
            .map(|f| f as *const c_void)
            .unwrap_or(std::ptr::null());
        if !matches!(address as usize, 0 | 1 | 2 | 3 | usize::MAX) {
            return address;
        }
        let module = GetModuleHandleW(
            [
                111u16, 112, 101, 110, 103, 108, 51, 50, 46, 100, 108, 108, 0,
            ]
            .as_ptr(),
        );
        if module.is_null() {
            return std::ptr::null();
        }
        GetProcAddress(module, name.as_ptr().cast())
            .map(|f| f as *const c_void)
            .unwrap_or(std::ptr::null())
    }
}

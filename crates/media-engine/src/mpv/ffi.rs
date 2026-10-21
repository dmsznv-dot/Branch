//! Small ownership boundary over pinned client API 2.5. No raw diagnostics escape.
use crate::EngineError;
use libmpv2_sys as sys;
use std::{
    ffi::{CStr, CString, c_void},
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

// mpv 0.41's FFmpeg log callback is process-global and is reset when the
// first core dies, even if another core still lives (common/av_log.c). B1 has
// one player: deny overlapping cores rather than allowing raw log leakage.
static CORE_LIVE: AtomicBool = AtomicBool::new(false);
struct CorePermit;
impl CorePermit {
    fn acquire() -> Result<Self, EngineError> {
        CORE_LIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| EngineError::Busy)?;
        Ok(Self)
    }
}
impl Drop for CorePermit {
    fn drop(&mut self) {
        CORE_LIVE.store(false, Ordering::Release);
    }
}
pub(super) struct Client {
    ptr: NonNull<sys::mpv_handle>,
    primary: bool,
    _permit: Option<CorePermit>,
}
// SAFETY: mpv's client API permits calls from any thread. Each Client exclusively
// owns a different handle; it is moved, never aliased or shared (no Sync impl).
// Only the event worker waits for events on its handle. A render client is moved
// once into RenderEndpoint and then permanently confined to its GL thread.
unsafe impl Send for Client {}
impl Client {
    pub fn create() -> Result<Self, EngineError> {
        let permit = CorePermit::acquire()?;
        // SAFETY: no arguments; the non-null result is uniquely owned below.
        let ptr = NonNull::new(unsafe { sys::mpv_create() }).ok_or(EngineError::Unavailable)?;
        let client = Self {
            ptr,
            primary: true,
            _permit: Some(permit),
        };
        // SAFETY: query is global and does not borrow a handle.
        if unsafe { sys::mpv_client_api_version() } != (2 << 16 | 5) {
            return Err(EngineError::Unavailable);
        }
        Ok(client)
    }
    pub fn ptr(&self) -> *mut sys::mpv_handle {
        self.ptr.as_ptr()
    }
    pub fn option(&self, name: &CStr, value: &CStr) -> Result<(), EngineError> {
        // SAFETY: both strings stay alive through this copying synchronous call.
        check(unsafe { sys::mpv_set_option_string(self.ptr(), name.as_ptr(), value.as_ptr()) })
    }
    pub fn init(&self) -> Result<(), EngineError> {
        // SAFETY: initialized once, on the owning worker, after policy installation.
        check(unsafe { sys::mpv_initialize(self.ptr()) })
    }
    pub fn render_client(&self) -> Result<Self, EngineError> {
        // SAFETY: core is live; new uniquely owned client shares that core.
        let ptr =
            NonNull::new(unsafe { sys::mpv_create_client(self.ptr(), c"branch-render".as_ptr()) })
                .ok_or(EngineError::Unavailable)?;
        Ok(Self {
            ptr,
            primary: false,
            _permit: None,
        })
    }
    pub fn set(&self, name: &CStr, value: &str) -> Result<(), EngineError> {
        let value = CString::new(value).map_err(|_| EngineError::InvalidCommand)?;
        // SAFETY: synchronous function copies strings, both pointers valid until return.
        check(unsafe { sys::mpv_set_property_string(self.ptr(), name.as_ptr(), value.as_ptr()) })
    }
    pub fn command(&self, args: &[&str]) -> Result<Node, EngineError> {
        let strings: Result<Vec<_>, _> = args.iter().map(|s| CString::new(*s)).collect();
        let strings = strings.map_err(|_| EngineError::InvalidCommand)?;
        let mut pointers: Vec<_> = strings.iter().map(|s| s.as_ptr()).collect();
        pointers.push(std::ptr::null());
        let mut node = Node::empty();
        // SAFETY: null-terminated argument list and output node live through call.
        check(unsafe { sys::mpv_command_ret(self.ptr(), pointers.as_mut_ptr(), &mut node.0) })?;
        Ok(node)
    }
    pub fn number(&self, name: &CStr) -> Option<f64> {
        let mut value = 0.0f64;
        // SAFETY: output matches MPV_FORMAT_DOUBLE and lives through the call.
        let result = unsafe {
            sys::mpv_get_property(
                self.ptr(),
                name.as_ptr(),
                sys::mpv_format_MPV_FORMAT_DOUBLE,
                (&mut value as *mut f64).cast(),
            )
        };
        (result >= 0 && value.is_finite()).then_some(value)
    }
    pub fn string(&self, name: &CStr) -> Option<String> {
        // SAFETY: returned allocation is owned by caller, copied then freed once.
        let ptr = unsafe { sys::mpv_get_property_string(self.ptr(), name.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: mpv supplies a NUL-terminated string valid until mpv_free.
        let value = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: this allocation came from mpv above and has not been freed.
        unsafe { sys::mpv_free(ptr.cast()) };
        Some(value)
    }
    pub fn node(&self, name: &CStr) -> Option<Node> {
        let mut node = Node::empty();
        // SAFETY: output node owns the returned allocation until its Drop.
        let result = unsafe {
            sys::mpv_get_property(
                self.ptr(),
                name.as_ptr(),
                sys::mpv_format_MPV_FORMAT_NODE,
                (&mut node.0 as *mut sys::mpv_node).cast(),
            )
        };
        (result >= 0).then_some(node)
    }
    pub fn event(&self) -> Event {
        // SAFETY: only owner waits on this handle; event payload is copied before
        // the next wait call invalidates it. No borrowed event leaves this method.
        let event = unsafe { &*sys::mpv_wait_event(self.ptr(), 0.0) };
        match event.event_id {
            sys::mpv_event_id_MPV_EVENT_NONE => Event::None,
            sys::mpv_event_id_MPV_EVENT_START_FILE => {
                // SAFETY: event ID specifies this payload type, valid until next wait.
                Event::Start(
                    unsafe { &*event.data.cast::<sys::mpv_event_start_file>() }.playlist_entry_id,
                )
            }
            sys::mpv_event_id_MPV_EVENT_FILE_LOADED => Event::Loaded,
            sys::mpv_event_id_MPV_EVENT_END_FILE => {
                // SAFETY: event ID specifies this payload type, copied before next wait.
                let e = unsafe { &*event.data.cast::<sys::mpv_event_end_file>() };
                Event::End {
                    id: e.playlist_entry_id,
                    error: e.error,
                    reason: e.reason,
                }
            }
            _ => Event::Other,
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: unique handle; renderer client is destroyed before primary by
        // the render lease. Primary destruction joins native threads before the
        // worker releases its stream registry. It may block, so never run on UI.
        unsafe {
            if self.primary {
                sys::mpv_terminate_destroy(self.ptr());
            } else {
                sys::mpv_destroy(self.ptr());
            }
        }
    }
}
pub(super) enum Event {
    None,
    Start(i64),
    Loaded,
    End { id: i64, error: i32, reason: u32 },
    Other,
}
pub(super) fn check(result: i32) -> Result<(), EngineError> {
    if result < 0 {
        Err(EngineError::DecoderFailed)
    } else {
        Ok(())
    }
}

pub(super) struct Node(sys::mpv_node);
impl Node {
    fn empty() -> Self {
        Self(sys::mpv_node {
            u: sys::mpv_node__bindgen_ty_1 { int64: 0 },
            format: sys::mpv_format_MPV_FORMAT_NONE,
        })
    }
    pub fn view(&self) -> View<'_> {
        View(&self.0)
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        // SAFETY: mpv allocated node contents; one RAII owner frees them after views end.
        unsafe { sys::mpv_free_node_contents(&mut self.0) }
    }
}
#[derive(Clone, Copy)]
pub(super) struct View<'a>(&'a sys::mpv_node);
impl<'a> View<'a> {
    pub fn int(self) -> Option<i64> {
        // SAFETY: the union member is accessed only when its tag matches.
        if self.0.format == sys::mpv_format_MPV_FORMAT_INT64 {
            Some(unsafe { self.0.u.int64 })
        } else {
            None
        }
    }
    pub fn flag(self) -> bool {
        // SAFETY: guarded by tag and borrowed from live Node.
        self.0.format == sys::mpv_format_MPV_FORMAT_FLAG && unsafe { self.0.u.flag != 0 }
    }
    pub fn text(self) -> Option<String> {
        if self.0.format != sys::mpv_format_MPV_FORMAT_STRING {
            return None;
        }
        // SAFETY: tag identifies mpv-owned terminated string; copied within Node lifetime.
        Some(
            unsafe { CStr::from_ptr(self.0.u.string) }
                .to_string_lossy()
                .chars()
                .take(1024)
                .collect(),
        )
    }
    pub fn items(self) -> Vec<View<'a>> {
        if self.0.format != sys::mpv_format_MPV_FORMAT_NODE_ARRAY {
            return vec![];
        }
        // SAFETY: matching tag means list/values valid for live Node; cap output.
        let list = unsafe { &*self.0.u.list };
        (0..list.num.clamp(0, 128) as usize)
            .map(|i| View(unsafe { &*list.values.add(i) }))
            .collect()
    }
    pub fn get(self, key: &CStr) -> Option<View<'a>> {
        if self.0.format != sys::mpv_format_MPV_FORMAT_NODE_MAP {
            return None;
        }
        // SAFETY: map tag guarantees keys and values arrays of num entries.
        let list = unsafe { &*self.0.u.list };
        for i in 0..list.num.clamp(0, 256) as usize {
            // SAFETY: index bounded by mpv list length; strings owned by live Node.
            if unsafe { CStr::from_ptr(*list.keys.add(i)) } == key {
                return Some(View(unsafe { &*list.values.add(i) }));
            }
        }
        None
    }
}
pub(super) fn pointer<T>(value: &mut T) -> *mut c_void {
    (value as *mut T).cast()
}

#[cfg(test)]
pub(super) static NATIVE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn second_live_core_fails_closed() {
        let _serial = NATIVE_TEST_LOCK.lock().await;
        let first = Client::create().unwrap();
        assert!(matches!(Client::create(), Err(EngineError::Busy)));
        drop(first);
        drop(Client::create().unwrap());
    }
    #[tokio::test]
    async fn unsupported_option_is_an_error() {
        let _serial = NATIVE_TEST_LOCK.lock().await;
        let client = Client::create().unwrap();
        assert_eq!(
            client.option(c"branch-unsupported-option", c"no"),
            Err(EngineError::DecoderFailed)
        );
    }
}

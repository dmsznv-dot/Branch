//! A registration outlives the mpv core. Each open cookie owns an Arc source;
//! close releases exactly that cookie. Cancellation never frees or waits.
use crate::{EngineError, PlaybackGeneration, SelectedFile};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
const READ_CAP: usize = 65_536;
trait Reader: Send + Sync {
    fn len(&self) -> u64;
    fn read(
        &self,
        offset: u64,
        dst: &mut [u8],
        cancel: &CancellationToken,
    ) -> Result<usize, EngineError>;
}
impl Reader for SelectedFile {
    fn len(&self) -> u64 {
        self.len()
    }
    fn read(
        &self,
        offset: u64,
        dst: &mut [u8],
        cancel: &CancellationToken,
    ) -> Result<usize, EngineError> {
        self.read_at(offset, dst, cancel)
    }
}
pub(super) struct Source {
    uri: String,
    reader: Box<dyn Reader>,
    pub cancel: CancellationToken,
    pub error: Mutex<Option<EngineError>>,
}
impl Source {
    pub fn selected(file: SelectedFile, generation: PlaybackGeneration) -> Arc<Self> {
        Arc::new(Self {
            uri: format!("branchmedia://{generation}"),
            reader: Box::new(file),
            cancel: CancellationToken::new(),
            error: Mutex::new(None),
        })
    }
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn error(&self) -> Option<EngineError> {
        *self.error.lock().unwrap_or_else(|p| p.into_inner())
    }
}
#[derive(Default)]
pub(super) struct Registry {
    pub current: Mutex<Option<Arc<Source>>>,
}
impl Registry {
    pub fn replace(&self, source: Arc<Source>) {
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = current.replace(source) {
            old.cancel.cancel();
        }
    }
    pub fn retire(&self) {
        if let Some(old) = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            old.cancel.cancel();
        }
    }
    pub fn cancel(&self) {
        if let Some(source) = self
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            source.cancel.cancel();
        }
    }
}
struct Cookie {
    source: Arc<Source>,
    offset: Mutex<u64>,
}
impl Cookie {
    fn read_to(&self, dst: &mut [u8]) -> Result<usize, EngineError> {
        let mut offset = self.offset.lock().map_err(|_| EngineError::Unavailable)?;
        if self.source.cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let mut scratch = [0u8; READ_CAP];
        let limit = dst.len().min(READ_CAP);
        let count = self
            .source
            .reader
            .read(*offset, &mut scratch[..limit], &self.source.cancel)?;
        if self.source.cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if count > limit {
            return Err(EngineError::DecoderFailed);
        }
        let next = offset
            .checked_add(count as u64)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(EngineError::InvalidCommand)?;
        dst[..count].copy_from_slice(&scratch[..count]);
        *offset = next;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Dirty;
    impl Reader for Dirty {
        fn len(&self) -> u64 {
            100
        }
        fn read(
            &self,
            _: u64,
            dst: &mut [u8],
            _: &CancellationToken,
        ) -> Result<usize, EngineError> {
            dst.fill(7);
            Err(EngineError::SourceChanged)
        }
    }
    fn source(reader: Box<dyn Reader>) -> Arc<Source> {
        Arc::new(Source {
            uri: "branchmedia://test".into(),
            reader,
            cancel: CancellationToken::new(),
            error: Mutex::new(None),
        })
    }
    #[test]
    fn failed_read_discards_dirty_destination() {
        let cookie = Cookie {
            source: source(Box::new(Dirty)),
            offset: Mutex::new(0),
        };
        let mut dst = [1; 20];
        assert_eq!(cookie.read_to(&mut dst), Err(EngineError::SourceChanged));
        assert_eq!(dst, [1; 20]);
    }
    #[test]
    fn retiring_generation_cancels_and_removes_old_error() {
        let registry = Registry::default();
        let old = source(Box::new(Dirty));
        *old.error.lock().unwrap() = Some(EngineError::SourceChanged);
        registry.replace(old.clone());
        registry.retire();
        assert!(old.cancel.is_cancelled());
        assert!(registry.current.lock().unwrap().is_none());
    }
    struct Waiting {
        dropped: Arc<AtomicUsize>,
        started: std::sync::mpsc::SyncSender<()>,
    }
    impl Drop for Waiting {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Reader for Waiting {
        fn len(&self) -> u64 {
            100
        }
        fn read(
            &self,
            _: u64,
            _: &mut [u8],
            cancel: &CancellationToken,
        ) -> Result<usize, EngineError> {
            self.started.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
                std::thread::park_timeout(std::time::Duration::from_millis(1));
            }
            if cancel.is_cancelled() {
                Err(EngineError::Cancelled)
            } else {
                Err(EngineError::DecoderFailed)
            }
        }
    }
    #[test]
    fn shutdown_cancels_read_before_free() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let registry = Arc::new(Registry::default());
        registry.replace(source(Box::new(Waiting {
            dropped: drops.clone(),
            started: tx,
        })));
        let cookie = Cookie {
            source: registry.current.lock().unwrap().as_ref().unwrap().clone(),
            offset: Mutex::new(0),
        };
        let reader = std::thread::spawn(move || cookie.read_to(&mut [0; 10]));
        rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        registry.cancel();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(reader.join().unwrap(), Err(EngineError::Cancelled));
        drop(registry);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[cfg(feature = "native-mpv")]
mod callbacks {
    use super::*;
    use crate::mpv::ffi::{Client, check};
    use libmpv2_sys as sys;
    use std::{
        ffi::{CStr, c_char, c_void},
        panic::{AssertUnwindSafe, catch_unwind},
    };
    pub(super) fn register(client: &Client, registry: &Arc<Registry>) -> Result<(), EngineError> {
        // SAFETY: Arc allocation is stable. Worker retains this Arc until AFTER
        // mpv_terminate_destroy joins every native thread using the registration.
        check(unsafe {
            sys::mpv_stream_cb_add_ro(
                client.ptr(),
                c"branchmedia".as_ptr(),
                Arc::as_ptr(registry).cast_mut().cast(),
                Some(open),
            )
        })
    }
    unsafe extern "C" fn open(
        data: *mut c_void,
        uri: *mut c_char,
        info: *mut sys::mpv_stream_cb_info,
    ) -> i32 {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: registered Arc is retained by worker through core teardown;
            // mpv provides NUL URI and writable info valid for this invocation.
            let registry = unsafe { &*data.cast::<Registry>() };
            let uri = unsafe { CStr::from_ptr(uri) }.to_bytes();
            let current = registry.current.lock().map_err(|_| ())?;
            let source = current
                .as_ref()
                .filter(|s| s.uri.as_bytes() == uri && !s.cancel.is_cancelled())
                .ok_or(())?
                .clone();
            let cookie = Box::new(Cookie {
                source,
                offset: Mutex::new(0),
            });
            // SAFETY: mpv owns this cookie until exactly one close call. Every
            // callback borrows it; cancel uses only thread-safe token signaling.
            unsafe {
                *info = sys::mpv_stream_cb_info {
                    cookie: Box::into_raw(cookie).cast(),
                    read_fn: Some(read),
                    seek_fn: Some(seek),
                    size_fn: Some(size),
                    close_fn: Some(close),
                    cancel_fn: Some(cancel),
                };
            }
            Ok::<_, ()>(())
        }))
        .ok()
        .and_then(Result::ok)
        .map_or(-13, |()| 0)
    }
    unsafe extern "C" fn read(data: *mut c_void, buf: *mut c_char, bytes: u64) -> i64 {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: live cookie guaranteed until close; mpv grants buffer bytes
            // writable bytes. Never form a slice larger than our fixed cap.
            let cookie = unsafe { &*data.cast::<Cookie>() };
            let count = bytes.min(READ_CAP as u64) as usize;
            if count == 0 {
                return 0;
            }
            let dst = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), count) };
            match cookie.read_to(dst) {
                Ok(n) => n as i64,
                Err(error) => {
                    *cookie
                        .source
                        .error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(error);
                    -1
                }
            }
        }))
        .unwrap_or(-1)
    }
    unsafe extern "C" fn seek(data: *mut c_void, offset: i64) -> i64 {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: cookie remains live until close, offset mutex serializes
            // cursor changes; cancel runs separately and only signals the token.
            let cookie = unsafe { &*data.cast::<Cookie>() };
            if offset < 0
                || offset as u64 > cookie.source.reader.len()
                || cookie.source.cancel.is_cancelled()
            {
                return -1;
            }
            match cookie.offset.lock() {
                Ok(mut value) => {
                    *value = offset as u64;
                    offset
                }
                Err(_) => -1,
            }
        }))
        .unwrap_or(-1)
    }
    unsafe extern "C" fn size(data: *mut c_void) -> i64 {
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: live immutable cookie; length is fixed at trusted selection.
            let cookie = unsafe { &*data.cast::<Cookie>() };
            i64::try_from(cookie.source.reader.len()).unwrap_or(-1)
        }))
        .unwrap_or(-1)
    }
    unsafe extern "C" fn cancel(data: *mut c_void) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: the required mpv quiesce-stream-cancel patch clears this
            // callback under mp_cancel.lock before invoking close_fn. It waits
            // for in-flight cancel and prevents later cookie access. No file/
            // cursor locks, join or mpv/GL call; only token signaling here.
            unsafe { &*data.cast::<Cookie>() }.source.cancel.cancel();
        }));
    }
    unsafe extern "C" fn close(data: *mut c_void) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: mpv transfers unique Box ownership back exactly once.
            // The required quiesce-stream-cancel dependency patch synchronizes/
            // unregisters cancel before close_fn; read/seek are on this same
            // demux thread. No native callback can still use this cookie.
            // Registration Arc remains separately owned through core teardown.
            drop(unsafe { Box::from_raw(data.cast::<Cookie>()) });
        }));
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted {
            calls: Arc<AtomicUsize>,
            max: Arc<AtomicUsize>,
        }
        impl Reader for Counted {
            fn len(&self) -> u64 {
                70_000
            }
            fn read(
                &self,
                offset: u64,
                dst: &mut [u8],
                _: &CancellationToken,
            ) -> Result<usize, EngineError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.max.fetch_max(dst.len(), Ordering::SeqCst);
                let n = dst.len().min((self.len() - offset) as usize);
                dst[..n].fill(42);
                Ok(n)
            }
        }
        #[test]
        fn actual_callbacks_enforce_exact_uri_cap_offsets_eof_and_cancel() {
            let calls = Arc::new(AtomicUsize::new(0));
            let max = Arc::new(AtomicUsize::new(0));
            let source = Arc::new(Source {
                uri: "branchmedia://selected".into(),
                reader: Box::new(Counted {
                    calls: calls.clone(),
                    max: max.clone(),
                }),
                cancel: CancellationToken::new(),
                error: Mutex::new(None),
            });
            let registry = Arc::new(Registry::default());
            registry.replace(source);
            let mut info = std::mem::MaybeUninit::<sys::mpv_stream_cb_info>::uninit();
            let mut wrong = b"branchmedia://unselected\0".to_vec();
            // SAFETY: registry Arc and URI allocation live throughout invocation;
            // rejected open must not initialize or own the output cookie.
            assert!(
                unsafe {
                    open(
                        Arc::as_ptr(&registry).cast_mut().cast(),
                        wrong.as_mut_ptr().cast(),
                        info.as_mut_ptr(),
                    )
                } < 0
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let mut uri = b"branchmedia://selected\0".to_vec();
            // SAFETY: valid live registry, terminated URI, writable output info.
            assert_eq!(
                unsafe {
                    open(
                        Arc::as_ptr(&registry).cast_mut().cast(),
                        uri.as_mut_ptr().cast(),
                        info.as_mut_ptr(),
                    )
                },
                0
            );
            // SAFETY: successful open fully initializes the mpv info structure.
            let info = unsafe { info.assume_init() };
            let mut buf = [0u8; READ_CAP];
            // SAFETY: live cookie until close below. The callback must limit even
            // oversized native requests before constructing its buffer slice.
            unsafe {
                assert_eq!(
                    read(info.cookie, buf.as_mut_ptr().cast(), u64::MAX),
                    READ_CAP as i64
                );
                assert_eq!(size(info.cookie), 70_000);
                assert_eq!(seek(info.cookie, -1), -1);
                assert_eq!(seek(info.cookie, i64::MAX), -1);
                assert_eq!(seek(info.cookie, 70_000), 70_000);
                assert_eq!(read(info.cookie, buf.as_mut_ptr().cast(), 100), 0);
                cancel(info.cookie);
                assert_eq!(read(info.cookie, buf.as_mut_ptr().cast(), 100), -1);
                close(info.cookie);
            }
            assert_eq!(max.load(Ordering::SeqCst), READ_CAP);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
        struct Panics;
        impl Reader for Panics {
            fn len(&self) -> u64 {
                1
            }
            fn read(
                &self,
                _: u64,
                _: &mut [u8],
                _: &CancellationToken,
            ) -> Result<usize, EngineError> {
                panic!("synthetic reader failure")
            }
        }
        #[test]
        fn panic_cannot_cross_read_abi_or_publish_bytes() {
            let source = Arc::new(Source {
                uri: "branchmedia://test".into(),
                reader: Box::new(Panics),
                cancel: CancellationToken::new(),
                error: Mutex::new(None),
            });
            let cookie = Box::into_raw(Box::new(Cookie {
                source,
                offset: Mutex::new(0),
            }))
            .cast();
            let mut buf = [7u8; 1];
            // SAFETY: uniquely owned live cookie and valid one-byte buffer. ABI
            // read catches the synthetic panic; close returns Box ownership once.
            unsafe {
                assert_eq!(read(cookie, buf.as_mut_ptr().cast(), 1), -1);
                close(cookie);
            }
            assert_eq!(buf, [7]);
        }
    }
}
#[cfg(feature = "native-mpv")]
pub(super) fn register(
    client: &super::ffi::Client,
    registry: &Arc<Registry>,
) -> Result<(), EngineError> {
    callbacks::register(client, registry)
}

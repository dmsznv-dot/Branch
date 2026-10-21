use super::{
    EngineMode,
    ffi::{Client, Event},
    policy,
    render::{RenderEndpoint, RenderLease},
    state::{State, milliseconds},
    stream::{self, Registry, Source},
};
use crate::{
    DecoderInfo, EngineCommand, EngineError, EngineSnapshot, PlaybackGeneration, PlayerStatus,
    SelectedFile, Track, TrackKind,
};
use shared_protocol::Speed;
use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
const QUEUE_CAPACITY: usize = 64;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// Typed observed decode counters. No decoder property names or arbitrary
/// queries are exposed at the public boundary.
#[derive(Clone, Debug, Default)]
pub struct DecodeDiagnostics {
    pub width: u32,
    pub height: u32,
    pub estimated_frame_number: u64,
    pub failure: Option<EngineError>,
}
struct Control {
    quit: CancellationToken,
    registry: Arc<Registry>,
    diagnostics: Mutex<DecodeDiagnostics>,
    returned_endpoint: Mutex<Option<RenderEndpoint>>,
}
impl Control {
    fn cancel(&self) {
        self.quit.cancel();
        self.registry.cancel();
    }
}
enum Request {
    Open(
        SelectedFile,
        oneshot::Sender<Result<PlaybackGeneration, EngineError>>,
    ),
    Command(
        PlaybackGeneration,
        EngineCommand,
        oneshot::Sender<Result<(), EngineError>>,
    ),
}
pub struct EngineHandle {
    tx: mpsc::Sender<Request>,
    snapshot: watch::Receiver<EngineSnapshot>,
    control: Arc<Control>,
    done: Option<oneshot::Receiver<()>>,
    thread: Option<JoinHandle<()>>,
    endpoint: Option<RenderEndpoint>,
}
impl EngineHandle {
    pub async fn start(mode: EngineMode) -> Result<Self, EngineError> {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let state = State::new();
        let (snap_tx, snapshot) = watch::channel(state.snapshot.clone());
        let control = Arc::new(Control {
            quit: CancellationToken::new(),
            registry: Arc::new(Registry::default()),
            diagnostics: Mutex::new(DecodeDiagnostics::default()),
            returned_endpoint: Mutex::new(None),
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        let (done_tx, done) = oneshot::channel();
        let worker_control = control.clone();
        let thread = std::thread::Builder::new()
            .name("branch-mpv".into())
            .spawn(move || {
                run(mode, rx, state, snap_tx, worker_control, ready_tx);
                let _ = done_tx.send(());
            })
            .map_err(|_| EngineError::Unavailable)?;
        let endpoint = match ready_rx.await {
            Ok(Ok(endpoint)) => endpoint,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(EngineError::Unavailable);
            }
        };
        Ok(Self {
            tx,
            snapshot,
            control,
            done: Some(done),
            thread: Some(thread),
            endpoint,
        })
    }
    pub fn submit_open(&self, file: SelectedFile) -> Result<super::OpenRequest, EngineError> {
        let (tx, receiver) = oneshot::channel();
        self.send(Request::Open(file, tx))?;
        Ok(super::OpenRequest { receiver })
    }
    pub async fn open(&self, file: SelectedFile) -> Result<PlaybackGeneration, EngineError> {
        self.submit_open(file)?.wait().await
    }
    pub async fn command(
        &self,
        generation: PlaybackGeneration,
        command: EngineCommand,
    ) -> Result<(), EngineError> {
        command.validate()?;
        let (tx, rx) = oneshot::channel();
        self.send(Request::Command(generation, command, tx))?;
        rx.await.map_err(|_| EngineError::Cancelled)?
    }
    fn send(&self, request: Request) -> Result<(), EngineError> {
        self.tx.try_send(request).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => EngineError::Busy,
            mpsc::error::TrySendError::Closed(_) => EngineError::Cancelled,
        })
    }
    pub fn subscribe(&self) -> watch::Receiver<EngineSnapshot> {
        self.snapshot.clone()
    }
    pub fn diagnostics(&self) -> DecodeDiagnostics {
        self.control
            .diagnostics
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
    pub fn take_render_endpoint(&mut self) -> Result<RenderEndpoint, EngineError> {
        self.endpoint.take().ok_or(EngineError::Unavailable)
    }
    fn return_endpoint(&mut self) {
        if let Some(endpoint) = self.endpoint.take() {
            *self
                .control
                .returned_endpoint
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(endpoint);
        }
    }
    pub async fn shutdown(mut self) -> Result<(), EngineError> {
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        self.return_endpoint();
        self.control.cancel();
        // One global deadline. On timeout dropping JoinHandle detaches; worker
        // retains its Arc control, core and registry until native code exits.
        let done = self.done.take().ok_or(EngineError::Cancelled)?;
        tokio::time::timeout_at(deadline, done)
            .await
            .map_err(|_| EngineError::ShutdownTimeout)?
            .map_err(|_| EngineError::DecoderFailed)?;
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| EngineError::DecoderFailed)?;
        }
        Ok(())
    }
}
impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.return_endpoint();
        self.control.cancel();
    }
}
type OpenReply = oneshot::Sender<Result<PlaybackGeneration, EngineError>>;
fn run(
    mode: EngineMode,
    mut rx: mpsc::Receiver<Request>,
    mut state: State,
    snapshots: watch::Sender<EngineSnapshot>,
    control: Arc<Control>,
    ready: oneshot::Sender<Result<Option<RenderEndpoint>, EngineError>>,
) {
    let setup = (|| {
        let client = Client::create()?;
        policy::apply(&client, mode)?;
        stream::register(&client, &control.registry)?;
        client.init()?;
        let lease = Arc::new(RenderLease::default());
        let endpoint = if mode == EngineMode::Native {
            Some(RenderEndpoint::new(client.render_client()?, lease.clone()))
        } else {
            None
        };
        Ok::<_, EngineError>((client, lease, endpoint))
    })();
    let (client, lease, endpoint) = match setup {
        Ok(parts) => parts,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let has_renderer = endpoint.is_some();
    if ready.send(Ok(endpoint)).is_err() {
        control.cancel();
    }
    let mut id = None;
    let mut started = None;
    let mut loaded = false;
    let mut pending: Option<OpenReply> = None;
    while !control.quit.is_cancelled() {
        match rx.try_recv() {
            Ok(Request::Open(file, reply)) => {
                if let Some(old) = pending.take() {
                    let _ = old.send(Err(EngineError::Cancelled));
                }
                control.registry.retire();
                let generation = PlaybackGeneration::new();
                state.open(generation);
                snapshots.send_replace(state.snapshot.clone());
                loaded = false;
                id = None;
                *control
                    .diagnostics
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = DecodeDiagnostics::default();
                let result = (|| {
                    let options = policy::load_options(&file, &control.quit)?;
                    client.set(c"demuxer-lavf-o", options)?;
                    let source = Source::selected(file, generation);
                    control.registry.replace(source.clone());
                    let node = client.command(&["loadfile", source.uri(), "replace"])?;
                    node.view()
                        .get(c"playlist_entry_id")
                        .and_then(|v| v.int())
                        .ok_or(EngineError::DecoderFailed)
                })();
                match result {
                    Ok(entry) => {
                        id = Some(entry);
                        pending = Some(reply);
                    }
                    Err(error) => {
                        fail(&mut state, &control, &snapshots, &mut Some(reply), error);
                    }
                }
            }
            Ok(Request::Command(generation, command, reply)) => {
                let result = state.validate(generation, command).and_then(|command| {
                    if let Some(command) = command {
                        apply_command(&client, command.clone())?;
                        if command == EngineCommand::Stop {
                            control.registry.cancel();
                            loaded = false;
                            id = None;
                            state.snapshot.status = PlayerStatus::Idle;
                            if let Some(p) = pending.take() {
                                let _ = p.send(Err(EngineError::Cancelled));
                            }
                        }
                    }
                    Ok(())
                });
                let _ = reply.send(result);
            }
            Err(mpsc::error::TryRecvError::Disconnected) => {
                control.cancel();
                break;
            }
            Err(mpsc::error::TryRecvError::Empty) => {}
        }
        loop {
            match client.event() {
                Event::None => break,
                Event::Start(entry) => started = Some(entry),
                Event::Loaded if started == id && id.is_some() => {
                    loaded = true;
                    poll(&client, &mut state, &control);
                    if state.snapshot.decoder.is_none() {
                        fail(
                            &mut state,
                            &control,
                            &snapshots,
                            &mut pending,
                            EngineError::UnsupportedMedia,
                        );
                        loaded = false;
                    } else if let Some(reply) = pending.take() {
                        snapshots.send_replace(state.snapshot.clone());
                        let _ = reply.send(Ok(state.snapshot.generation));
                    }
                }
                Event::End {
                    id: entry,
                    error,
                    reason,
                } if Some(entry) == id => {
                    loaded = false;
                    if error < 0 || reason == 4 {
                        let error = source_error(&control).unwrap_or(EngineError::UnsupportedMedia);
                        fail(&mut state, &control, &snapshots, &mut pending, error);
                    } else if reason == 0 {
                        let _ = state.event(state.snapshot.generation, super::state::Event::Ended);
                    }
                }
                _ => {}
            }
        }
        if let Some(error) = source_error(&control).filter(|e| *e != EngineError::Cancelled) {
            fail(&mut state, &control, &snapshots, &mut pending, error);
            loaded = false;
        }
        if loaded {
            poll(&client, &mut state, &control);
        }
        if *snapshots.borrow() != state.snapshot {
            snapshots.send_replace(state.snapshot.clone());
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
    control.registry.cancel();
    if let Some(reply) = pending.take() {
        let _ = reply.send(Err(EngineError::Cancelled));
    }
    // Renderer must free its context/client before the primary handle/core.
    // A held endpoint can stall this thread, but cannot make shutdown free it.
    // Return an unused endpoint to this worker rather than invoking potentially
    // blocking mpv_destroy on the async/UI caller before its shutdown deadline.
    let returned_endpoint = control
        .returned_endpoint
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    drop(returned_endpoint);
    if has_renderer {
        lease.wait();
    }
    drop(client); // joins all native callbacks before registration/source release
    control
        .registry
        .current
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
}
fn source_error(control: &Control) -> Option<EngineError> {
    control
        .registry
        .current
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|s| s.error())
}
fn fail(
    state: &mut State,
    control: &Control,
    snapshots: &watch::Sender<EngineSnapshot>,
    pending: &mut Option<OpenReply>,
    error: EngineError,
) {
    let _ = state.event(state.snapshot.generation, super::state::Event::Failed);
    control
        .diagnostics
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .failure = Some(error);
    // The error reply must never wake a caller with the previous generation
    // or a pre-failure status still in its watch receiver. Avoid re-emitting an
    // unchanged failure on subsequent source-error polls.
    if *snapshots.borrow() != state.snapshot {
        snapshots.send_replace(state.snapshot.clone());
    }
    if let Some(reply) = pending.take() {
        let _ = reply.send(Err(error));
    }
}
fn apply_command(client: &Client, command: EngineCommand) -> Result<(), EngineError> {
    match command {
        EngineCommand::SetPaused(v) => client.set(c"pause", if v { "yes" } else { "no" }),
        EngineCommand::SeekToMs(v) => client
            .command(&[
                "seek",
                &format!("{}.{:03}", v / 1000, v % 1000),
                "absolute+exact",
            ])
            .map(|_| ()),
        EngineCommand::SetSpeed(v) => client.set(c"speed", &v.as_f64().to_string()),
        EngineCommand::SetVolume(v) => client.set(c"volume", &v.to_string()),
        EngineCommand::SetMuted(v) => client.set(c"mute", if v { "yes" } else { "no" }),
        EngineCommand::SelectAudio(id) => client.set(c"aid", &id.to_string()),
        EngineCommand::SelectSubtitle(id) => {
            client.set(c"sid", &id.map_or_else(|| "no".into(), |v| v.to_string()))
        }
        EngineCommand::Stop => client.command(&["stop"]).map(|_| ()),
    }
}
fn poll(client: &Client, state: &mut State, control: &Control) {
    let snapshot = &mut state.snapshot;
    if let Some(seconds) = client.number(c"time-pos")
        && let Ok(ms) = milliseconds(seconds)
    {
        snapshot.position_ms = ms;
    }
    snapshot.duration_ms = client
        .number(c"duration")
        .and_then(|s| milliseconds(s).ok());
    if let Some(speed) = client
        .number(c"speed")
        .and_then(|s| Speed::try_from(s).ok())
    {
        snapshot.speed = speed;
    }
    if let Some(volume) = client
        .number(c"volume")
        .filter(|v| (0.0..=100.0).contains(v))
    {
        snapshot.volume = volume.round() as u8;
    }
    snapshot.muted = client.string(c"mute").as_deref() == Some("yes");
    snapshot.status = if client.string(c"eof-reached").as_deref() == Some("yes") {
        PlayerStatus::Ended
    } else if client.string(c"paused-for-cache").as_deref() == Some("yes") {
        PlayerStatus::Buffering
    } else if client.string(c"pause").as_deref() == Some("yes") {
        PlayerStatus::Paused
    } else {
        PlayerStatus::Playing
    };
    let codec = client.string(c"video-format").filter(|s| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    });
    snapshot.decoder = codec.map(|codec| DecoderInfo {
        codec,
        hwdec: client.string(c"hwdec-current").filter(|v| {
            v != "no" && v.len() <= 32 && v.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        }),
        video_output: client
            .string(c"current-vo")
            .filter(|v| v == "null" || v == "libmpv")
            .unwrap_or_default(),
    });
    if let Some(node) = client.node(c"track-list") {
        snapshot.tracks = node
            .view()
            .items()
            .into_iter()
            .filter_map(|v| {
                let kind = match v.get(c"type")?.text()?.as_str() {
                    "audio" => TrackKind::Audio,
                    "sub" => TrackKind::Subtitle,
                    _ => return None,
                };
                Some(Track {
                    id: v.get(c"id")?.int()?,
                    kind,
                    title: v.get(c"title").and_then(|v| v.text()),
                    language: v.get(c"lang").and_then(|v| v.text()),
                    selected: v.get(c"selected").is_some_and(|v| v.flag()),
                })
            })
            .collect();
    }
    let mut diagnostics = control
        .diagnostics
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    diagnostics.width = client
        .number(c"video-params/w")
        .filter(|v| *v > 0.0 && *v <= u32::MAX as f64)
        .unwrap_or(0.0) as u32;
    diagnostics.height = client
        .number(c"video-params/h")
        .filter(|v| *v > 0.0 && *v <= u32::MAX as f64)
        .unwrap_or(0.0) as u32;
    diagnostics.estimated_frame_number = client
        .number(c"estimated-frame-number")
        .filter(|v| *v >= 0.0 && *v < u64::MAX as f64)
        .unwrap_or(0.0) as u64;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_open_publishes_b_before_waking_error_reply() {
        use std::{
            future::Future,
            task::{Context, Poll, Wake, Waker},
        };
        struct Observer {
            current: watch::Receiver<EngineSnapshot>,
            seen: Mutex<Option<EngineSnapshot>>,
        }
        impl Wake for Observer {
            fn wake(self: Arc<Self>) {
                *self.seen.lock().unwrap() = Some(self.current.borrow().clone());
            }
        }
        let mut state = State::new();
        let a = state.snapshot.generation;
        let (snapshots, current) = watch::channel(state.snapshot.clone());
        let b = PlaybackGeneration::new();
        state.open(b);
        let control = Control {
            quit: CancellationToken::new(),
            registry: Arc::new(Registry::default()),
            diagnostics: Mutex::new(DecodeDiagnostics::default()),
            returned_endpoint: Mutex::new(None),
        };
        let (reply, mut response) = oneshot::channel();
        let mut pending = Some(reply);
        let observer = Arc::new(Observer {
            current,
            seen: Mutex::new(None),
        });
        let waker = Waker::from(observer.clone());
        let mut context = Context::from_waker(&waker);
        assert!(matches!(
            std::pin::Pin::new(&mut response).poll(&mut context),
            Poll::Pending
        ));
        fail(
            &mut state,
            &control,
            &snapshots,
            &mut pending,
            EngineError::SourceChanged,
        );
        let observed = observer
            .seen
            .lock()
            .unwrap()
            .clone()
            .expect("reply must wake observer synchronously");
        assert_ne!(a, b);
        assert_eq!(observed.generation, b);
        assert_eq!(observed.status, PlayerStatus::Failed);
        assert_eq!(
            response.try_recv().unwrap(),
            Err(EngineError::SourceChanged)
        );
        assert!(pending.is_none());
        drop(snapshots);
    }
    #[tokio::test]
    async fn owned_open_requests_submit_b_while_a_is_pending() {
        let (tx, mut rx) = mpsc::channel(QUEUE_CAPACITY);
        let (_, snapshot) = watch::channel(State::new().snapshot);
        let control = Arc::new(Control {
            quit: CancellationToken::new(),
            registry: Arc::new(Registry::default()),
            diagnostics: Mutex::new(DecodeDiagnostics::default()),
            returned_endpoint: Mutex::new(None),
        });
        let engine = EngineHandle {
            tx,
            snapshot,
            control,
            done: None,
            thread: None,
            endpoint: None,
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let a = engine
            .submit_open(SelectedFile::open(file.path()).unwrap())
            .unwrap();
        let b = engine
            .submit_open(SelectedFile::open(file.path()).unwrap())
            .unwrap();
        let Request::Open(_, a_reply) = rx.try_recv().unwrap() else {
            panic!("expected A")
        };
        let Request::Open(_, b_reply) = rx.try_recv().unwrap() else {
            panic!("expected B while A has no reply")
        };
        let generation = PlaybackGeneration::new();
        b_reply.send(Ok(generation)).unwrap();
        a_reply.send(Err(EngineError::Cancelled)).unwrap();
        let b = b.wait();
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&b);
        assert_eq!(b.await, Ok(generation));
        assert_eq!(a.wait().await, Err(EngineError::Cancelled));
    }
    #[tokio::test]
    async fn queue_is_bounded_and_returns_busy() {
        let (tx, _rx) = mpsc::channel(QUEUE_CAPACITY);
        let (_, snapshot) = watch::channel(State::new().snapshot);
        let control = Arc::new(Control {
            quit: CancellationToken::new(),
            registry: Arc::new(Registry::default()),
            diagnostics: Mutex::new(DecodeDiagnostics::default()),
            returned_endpoint: Mutex::new(None),
        });
        let engine = EngineHandle {
            tx,
            snapshot,
            control,
            done: None,
            thread: None,
            endpoint: None,
        };
        for _ in 0..64 {
            let (reply, _) = oneshot::channel();
            engine
                .send(Request::Command(
                    PlaybackGeneration::new(),
                    EngineCommand::Stop,
                    reply,
                ))
                .unwrap();
        }
        let (reply, _) = oneshot::channel();
        assert_eq!(
            engine.send(Request::Command(
                PlaybackGeneration::new(),
                EngineCommand::Stop,
                reply
            )),
            Err(EngineError::Busy)
        );
    }
    #[tokio::test]
    async fn held_renderer_times_out_without_freeing_live_core() {
        let _serial = super::super::ffi::NATIVE_TEST_LOCK.lock().await;
        let mut engine = EngineHandle::start(EngineMode::Native).await.unwrap();
        let endpoint = engine.take_render_endpoint().unwrap();
        assert!(matches!(
            engine.take_render_endpoint(),
            Err(EngineError::Unavailable)
        ));
        let worker = engine.thread.take().unwrap();
        assert!(matches!(
            EngineHandle::start(EngineMode::Headless).await,
            Err(EngineError::Busy)
        ));
        let result = engine.shutdown().await;
        assert_eq!(result, Err(EngineError::ShutdownTimeout));
        assert!(matches!(
            EngineHandle::start(EngineMode::Headless).await,
            Err(EngineError::Busy)
        ));
        assert!(
            !worker.is_finished(),
            "held client must retain core after deadline"
        );
        drop(endpoint);
        worker.join().unwrap();
        EngineHandle::start(EngineMode::Headless)
            .await
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
}

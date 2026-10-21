#![cfg(feature = "native-mpv")]
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/player-fixtures")
        .join(name)
}
fn probe(path: &Path) -> std::process::Output {
    assert!(
        path.exists(),
        "run scripts/generate-player-fixtures.sh first"
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_media-probe"))
        .args(["--file"])
        .arg(path)
        .arg("--headless")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("probe_timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn headless_decodes_supported_fixtures() {
    for name in [
        "h264.mp4",
        "h264.mov",
        "tracks.mkv",
        "hevc.mp4",
        "vp9.mkv",
        "av1.mkv",
    ] {
        let result = probe(&fixture(name));
        assert!(
            result.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(
            output.lines().any(|line| line.starts_with("decoded ")),
            "missing decode evidence"
        );
        if name == "tracks.mkv" {
            assert!(output.contains("audio_tracks=2 subtitle_tracks=1"));
        }
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("speed "))
                .count(),
            6
        );
        assert!(output.lines().any(|line| line == "paused"));
        assert!(
            output
                .lines()
                .any(|line| line.starts_with("seek position_ms="))
        );
        assert_eq!(output.lines().last(), Some("shutdown"));
    }
}

#[test]
fn unknown_codec_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = std::fs::read(fixture("h264.mp4")).unwrap();
    let index = bytes.windows(4).position(|w| w == b"avc1").unwrap();
    // The first avc1 can occur in compatible brands. Replace the actual sample
    // description too, so the codec is unavailable while MOV remains parseable.
    for i in 0..bytes.len().saturating_sub(4) {
        if &bytes[i..i + 4] == b"avc1" {
            bytes[i..i + 4].copy_from_slice(b"zzzz");
        }
    }
    assert!(index > 0);
    let path = dir.path().join("unknown.mp4");
    std::fs::write(&path, bytes).unwrap();
    let result = probe(&path);
    assert!(!result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap().trim(),
        "error code=UnsupportedMedia"
    );
}

#[tokio::test]
async fn changed_selection_is_source_changed_and_unlinked_descriptor_survives() {
    let _serial = DIRECT_NATIVE_LOCK.lock().await;
    use media_engine::{EngineError, EngineHandle, EngineMode, SelectedFile};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("selected.mp4");
    std::fs::copy(fixture("h264.mp4"), &path).unwrap();
    let file = SelectedFile::open(&path).unwrap();
    std::fs::write(&path, b"changed").unwrap();
    let engine = EngineHandle::start(EngineMode::Headless).await.unwrap();
    assert_eq!(engine.open(file).await, Err(EngineError::SourceChanged));
    engine.shutdown().await.unwrap();
    std::fs::copy(fixture("h264.mp4"), &path).unwrap();
    let file = SelectedFile::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let engine = EngineHandle::start(EngineMode::Headless).await.unwrap();
    let generation = engine.open(file).await.unwrap();
    assert_eq!(engine.subscribe().borrow().generation, generation);
    engine.shutdown().await.unwrap();
    assert_eq!(
        SelectedFile::open(&path).unwrap_err(),
        EngineError::Unavailable
    );
}

// Actual local-file open monitoring currently uses Linux IN_OPEN. Target-machine
// resource tracing remains part of the native gate; these cloud checks do not
// claim macOS/Windows read monitoring.
#[cfg(target_os = "linux")]
#[test]
fn references_are_not_followed() {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    let dir = tempfile::tempdir().unwrap();
    let prepared = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/make-policy-fixtures.py"))
        .arg(fixture("h264.mp4").parent().unwrap())
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "resource positive controls failed: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(
        String::from_utf8(prepared.stdout).unwrap().trim(),
        "resource_positive_controls_passed"
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (count, done) = (hits.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(e) => panic!("listener failure: {e}"),
            }
        }
    });
    // Positive control of the actual HTTP connection spy before locked decoding.
    let mut control = TcpStream::connect(address).unwrap();
    control
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut response = Vec::new();
    let _ = control.read_to_end(&mut response);
    assert_eq!(hits.swap(0, Ordering::SeqCst), 1);
    let spy = dir.path().join("spy.mp4");
    let unknown = "branchmedia://00000000-0000-0000-0000-000000000000";
    let references = [
        format!("file://{}", spy.display()),
        format!("http://{address}/spy.mp4"),
        unknown.into(),
    ];
    let mut inputs = vec![dir.path().join("dref.mov"), dir.path().join("ordered.mkv")];
    for (i, reference) in references.iter().enumerate() {
        let m3u = dir.path().join(format!("m3u-{i}.mp4"));
        std::fs::write(&m3u, format!("#EXTM3U\n{reference}\n")).unwrap();
        inputs.push(m3u);
        let pls = dir.path().join(format!("pls-{i}.mp4"));
        std::fs::write(
            &pls,
            format!("[playlist]\nNumberOfEntries=1\nFile1={reference}\n"),
        )
        .unwrap();
        inputs.push(pls);
    }
    let watch = OpenWatch::new(&[spy, dir.path().join("spy.mkv")]);
    for input in inputs {
        let output = probe(&input);
        // Resource denial and decode outcome are separate assertions: reference
        // media can decode its own data without following an external resource.
        assert!(output.stderr.is_empty(), "raw decoder output escaped");
        assert!(
            !watch.opened(),
            "resource-policy failure: unselected file opened"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "resource-policy failure: HTTP opened"
        );
        if !output.status.success() {
            let stdout = String::from_utf8(output.stdout).unwrap();
            let last = stdout.lines().last().unwrap();
            if input
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("m3u-")
                || input
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("pls-")
            {
                assert_eq!(
                    last, "error code=UnsupportedMedia",
                    "playlist decoder outcome"
                );
            } else {
                assert!(
                    matches!(
                        last,
                        "error code=UnsupportedMedia" | "error code=DecoderFailed"
                    ),
                    "reference decoder outcome"
                );
            }
        }
    }
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn configs_scripts_and_adjacent_subtitles_are_not_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("selected.mp4");
    std::fs::copy(fixture("h264.mp4"), &file).unwrap();
    let sub = dir.path().join("selected.srt");
    std::fs::write(&sub, "1\n00:00:00,000 --> 00:00:03,000\nSpy\n").unwrap();
    let script = dir.path().join("spy.lua");
    std::fs::write(&script, "error('script must not execute')\n").unwrap();
    let config = dir.path().join("mpv.conf");
    std::fs::write(&config, format!("volume=23\nscript={}\n", script.display())).unwrap();
    let scripts = dir.path().join("scripts");
    std::fs::create_dir(&scripts).unwrap();
    let autoload = scripts.join("autoload.lua");
    std::fs::write(&autoload, "error('autoload must not execute')\n").unwrap();
    let watch = OpenWatch::new(&[sub, script, config, autoload]);
    // The process environment is per-child; no concurrent global env mutation.
    let mut child = Command::new(env!("CARGO_BIN_EXE_media-probe"))
        .arg("--file")
        .arg(file)
        .arg("--headless")
        .env("MPV_HOME", dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("probe_timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "decoder failed");
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("subtitle_tracks=0")
    );
    assert!(
        !watch.opened(),
        "resource-policy failure: config/script/subtitle opened"
    );
}

#[cfg(target_os = "linux")]
struct OpenWatch(std::os::fd::OwnedFd);
#[cfg(target_os = "linux")]
impl OpenWatch {
    fn new(paths: &[PathBuf]) -> Self {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        // SAFETY: no pointers, returned descriptor uniquely owned by OwnedFd.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: valid newly returned owned fd, transferred exactly once.
        let this = Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
        for path in paths {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: live fd and valid NUL pathname for synchronous syscall.
            assert!(
                unsafe {
                    libc::inotify_add_watch(this.0.as_raw_fd(), path.as_ptr(), libc::IN_OPEN)
                } >= 0
            );
        }
        this
    }
    fn opened(&self) -> bool {
        use std::os::fd::AsRawFd;
        let mut buffer = [0u8; 65536];
        // SAFETY: valid fd and writable buffer bounded by its real allocation.
        let n = unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if n < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            false
        } else {
            n > 0
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_reply_has_published_generation() {
    let _serial = DIRECT_NATIVE_LOCK.lock().await;
    use media_engine::{EngineHandle, EngineMode, SelectedFile};
    let engine = EngineHandle::start(EngineMode::Headless).await.unwrap();
    for _ in 0..40 {
        let generation = engine
            .open(SelectedFile::open(&fixture("h264.mp4")).unwrap())
            .await
            .unwrap();
        assert_eq!(engine.subscribe().borrow().generation, generation);
    }
    engine.shutdown().await.unwrap();
}

static DIRECT_NATIVE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn failed_b_reply_has_published_failed_generation_for_both_failure_paths() {
    let _serial = DIRECT_NATIVE_LOCK.lock().await;
    use media_engine::{EngineError, EngineHandle, EngineMode, PlayerStatus, SelectedFile};
    let engine = EngineHandle::start(EngineMode::Headless).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("B.mp4");
    for immediate in [true, false] {
        let a = engine
            .open(SelectedFile::open(&fixture("h264.mp4")).unwrap())
            .await
            .unwrap();
        std::fs::write(&selected, b"invalid media").unwrap();
        let b = SelectedFile::open(&selected).unwrap();
        if immediate {
            std::fs::write(&selected, b"changed after selection").unwrap();
        }
        let expected = if immediate {
            EngineError::SourceChanged
        } else {
            EngineError::UnsupportedMedia
        };
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            engine.submit_open(b).unwrap().wait(),
        )
        .await
        .unwrap();
        assert_eq!(error, Err(expected));
        let failed = engine.subscribe().borrow().clone();
        assert_ne!(
            failed.generation, a,
            "B error reply must not expose A's snapshot"
        );
        assert_eq!(failed.status, PlayerStatus::Failed);
        assert_eq!(engine.diagnostics().failure, Some(expected));
    }
    engine.shutdown().await.unwrap();
}

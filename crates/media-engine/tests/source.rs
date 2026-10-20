use std::{
    fs::{self, FileTimes, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use media_engine::{
    DecoderInfo, EngineCommand, EngineError, EngineSnapshot, PlaybackGeneration, PlayerStatus,
    SelectedFile, Track, TrackKind,
};
use media_stream::CHUNK_BYTES;
use shared_protocol::Speed;
use tokio_util::sync::CancellationToken;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let dir = std::env::temp_dir().join(format!("media-engine-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("影片 selected.bin");
        fs::write(&path, bytes).unwrap();
        Self { dir, path }
    }

    fn open(&self) -> SelectedFile {
        SelectedFile::open(&self.path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// Returning more than CHUNK_BYTES or reopening the pathname breaks this test.
#[test]
fn read_is_bounded_and_descriptor_stable() {
    let bytes: Vec<u8> = (0..100_000).map(|n| (n % 251) as u8).collect();
    let fixture = Fixture::new(&bytes);
    let source = fixture.open();
    assert_eq!(source.len(), 100_000);
    assert_eq!(source.display_name(), "影片 selected.bin");
    let cancel = CancellationToken::new();
    let mut dst = vec![255; 100_000];
    assert_eq!(source.read_at(0, &mut dst, &cancel), Ok(65_536));
    assert_eq!(&dst[..CHUNK_BYTES], &bytes[..CHUNK_BYTES]);
    assert!(dst[CHUNK_BYTES..].iter().all(|&byte| byte == 255));
    fs::rename(&fixture.path, fixture.dir.join("renamed.bin")).unwrap();
    fs::write(&fixture.path, vec![42; bytes.len()]).unwrap();
    assert_eq!(source.read_at(65_536, &mut dst, &cancel), Ok(34_464));
    assert_eq!(&dst[..34_464], &bytes[65_536..]);
}

// Omitting cancellation or clearing a detected change breaks this test.
#[test]
fn cancel_and_mutation() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        source.read_at(0, &mut [0; 8], &cancel),
        Err(EngineError::Cancelled)
    );
    let cancel = CancellationToken::new();
    let file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_len(9).unwrap();
    assert_eq!(
        source.read_at(0, &mut [0; 8], &cancel),
        Err(EngineError::SourceChanged)
    );
    file.set_len(8).unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        source.read_at(0, &mut [0; 8], &cancel),
        Err(EngineError::SourceChanged)
    );
}

// Comparing only size misses this same-length metadata change.
#[test]
fn modified_time_change_is_sticky_even_at_eof() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    let file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.set_times(FileTimes::new().set_modified(modified + Duration::from_secs(2)))
        .unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        source.read_at(8, &mut [], &cancel),
        Err(EngineError::SourceChanged)
    );
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        source.read_at(0, &mut [], &cancel),
        Err(EngineError::SourceChanged)
    );
}

// Truncating offsets to 32 bits, wrapping subtraction, or accepting directories breaks this test.
#[test]
fn invalid_sources_and_bounds() {
    let fixture = Fixture::new(b"abc");
    assert!(matches!(
        SelectedFile::open(&fixture.dir),
        Err(EngineError::UnsupportedMedia)
    ));
    assert!(matches!(
        SelectedFile::open(&fixture.dir.join("missing")),
        Err(EngineError::Unavailable)
    ));
    let source = fixture.open();
    let cancel = CancellationToken::new();
    assert_eq!(source.read_at(3, &mut [0; 1], &cancel), Ok(0));
    assert_eq!(
        source.read_at(4, &mut [], &cancel),
        Err(EngineError::InvalidCommand)
    );
    assert_eq!(
        source.read_at(u64::MAX, &mut [0; 1], &cancel),
        Err(EngineError::InvalidCommand)
    );
    assert_eq!(source.read_at(0, &mut [], &cancel), Ok(0));
    let sparse = fixture.dir.join("sparse.bin");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&sparse)
        .unwrap();
    #[cfg(windows)]
    assert!(
        std::process::Command::new("fsutil")
            .args(["sparse", "setflag"])
            .arg(&sparse)
            .status()
            .expect("Windows sparse fixture needs fsutil")
            .success(),
        "filesystem must support sparse fixtures"
    );
    let offset = 4_294_967_296;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(b"tail").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert!(
            file.metadata().unwrap().blocks() * 512 < offset,
            "filesystem must create a sparse fixture"
        );
    }
    drop(file);
    let source = SelectedFile::open(&sparse).unwrap();
    let mut dst = [0; 8];
    assert_eq!(source.len(), offset + 4);
    assert_eq!(source.read_at(offset, &mut dst, &cancel), Ok(4));
    assert_eq!(&dst[..4], b"tail");
}

#[test]
fn empty_file_has_a_valid_eof() {
    let fixture = Fixture::new(b"");
    let source = fixture.open();
    assert!(source.is_empty());
    assert_eq!(
        source.read_at(0, &mut [0; 1], &CancellationToken::new()),
        Ok(0)
    );
}

// Removing the shared seek/read lock allows readers to steal each other's cursor.
#[test]
fn concurrent_readers_observe_their_own_offsets() {
    let bytes: Vec<u8> = (0..100_000).map(|n| (n % 251) as u8).collect();
    let fixture = Fixture::new(&bytes);
    let source = Arc::new(fixture.open());
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let source = &source;
            let bytes = &bytes;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                let cancel = CancellationToken::new();
                for step in 0..64 {
                    let offset = worker * 10_000 + step;
                    let mut dst = [0; 512];
                    assert_eq!(source.read_at(offset as u64, &mut dst, &cancel), Ok(512));
                    assert_eq!(&dst, &bytes[offset..offset + 512]);
                }
            });
        }
    });
}

#[test]
fn selected_file_debug_is_redacted() {
    let fixture = Fixture::new(b"private bytes");
    let source = fixture.open();
    let debug = format!("{source:?}");
    assert!(!debug.contains(&fixture.dir.to_string_lossy().to_string()));
    assert!(!debug.contains(source.display_name()));
    assert!(!debug.contains("private bytes"));
}

#[test]
fn command_validation_enforces_intrinsic_volume_bounds() {
    for value in [0, 100] {
        assert_eq!(EngineCommand::SetVolume(value).validate(), Ok(()));
    }
    for value in [101, u8::MAX] {
        assert_eq!(
            EngineCommand::SetVolume(value).validate(),
            Err(EngineError::InvalidCommand)
        );
    }
    for command in [
        EngineCommand::SetPaused(true),
        EngineCommand::SeekToMs(u64::MAX),
        EngineCommand::SetSpeed(Speed::Double),
        EngineCommand::SetMuted(false),
        EngineCommand::SelectAudio(-1),
        EngineCommand::SelectSubtitle(Some(7)),
        EngineCommand::SelectSubtitle(None),
        EngineCommand::Stop,
    ] {
        assert_eq!(command.validate(), Ok(()));
    }
}

#[test]
fn snapshots_retain_generation_tracks_and_observed_decoder() {
    let generation = PlaybackGeneration::new();
    assert_ne!(generation, PlaybackGeneration::new());
    assert_ne!(generation, PlaybackGeneration::default());
    let uuid = uuid::Uuid::parse_str(&generation.to_string()).unwrap();
    assert_eq!(uuid.get_version_num(), 4);
    let snapshot = EngineSnapshot {
        generation,
        status: PlayerStatus::Paused,
        position_ms: 100,
        duration_ms: Some(200),
        speed: Speed::Normal,
        volume: 100,
        muted: false,
        tracks: vec![
            Track {
                id: 3,
                kind: TrackKind::Audio,
                title: Some("<title>".into()),
                language: Some("ru".into()),
                selected: true,
            },
            Track {
                id: 7,
                kind: TrackKind::Subtitle,
                title: None,
                language: None,
                selected: false,
            },
        ],
        decoder: Some(DecoderInfo {
            codec: "hevc".into(),
            hwdec: None,
            video_output: "libmpv".into(),
        }),
    };
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.tracks[0].title.as_deref(), Some("<title>"));
    assert_eq!(snapshot.decoder.unwrap().hwdec, None);
}

#[test]
fn error_diagnostics_are_stable_codes() {
    for (error, code) in [
        (EngineError::Unavailable, "Unavailable"),
        (EngineError::UnsupportedMedia, "UnsupportedMedia"),
        (EngineError::InvalidCommand, "InvalidCommand"),
        (EngineError::Busy, "Busy"),
        (EngineError::SourceChanged, "SourceChanged"),
        (EngineError::Cancelled, "Cancelled"),
        (EngineError::DecoderFailed, "DecoderFailed"),
        (EngineError::ShutdownTimeout, "ShutdownTimeout"),
    ] {
        assert_eq!(format!("{error:?}"), code);
        assert_eq!(error.to_string(), code);
        assert!(std::error::Error::source(&error).is_none());
    }
}

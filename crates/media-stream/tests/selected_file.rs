use media_stream::{
    CHUNK_BYTES, MAX_READERS_PER_ROOM, MAX_READERS_PER_SESSION, ReaderLimits, SelectedMedia,
    StreamError,
};
use room_server::{CreatedRoom, MediaPermit, RoomError, RoomHandle};
use shared_protocol::{ClockEpochId, HostTime, MediaDescriptor, MediaId, WireU64};
use std::{
    fs::{self, FileTimes, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use sync_engine::ManualClock;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let dir = std::env::temp_dir().join(format!("media-stream-{}", MediaId::new()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("selected.bin");
        fs::write(&path, bytes).unwrap();
        Self { dir, path }
    }
    fn open(&self) -> SelectedMedia {
        SelectedMedia::open(&self.path, MediaId::new()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn room(descriptor: MediaDescriptor) -> (CreatedRoom, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: ClockEpochId::new(),
        ticks_ms: WireU64::new(0),
    }));
    (
        RoomHandle::create(descriptor, "host", clock.clone()).unwrap(),
        clock,
    )
}
async fn permit(created: &CreatedRoom, source: &SelectedMedia) -> MediaPermit {
    created
        .room
        .authorize_media(&created.host_session, source.descriptor(1).media_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn descriptor_and_bounded_reads() {
    assert_eq!(CHUNK_BYTES, 65_536);
    let bytes: Vec<u8> = (0..CHUNK_BYTES * 2 + 3).map(|n| (n % 251) as u8).collect();
    let fixture = Fixture::new(&bytes);
    let source = fixture.open();
    let descriptor = source.descriptor(1234);
    assert_eq!(descriptor.size_bytes.get(), bytes.len() as u64);
    assert_eq!(descriptor.duration_ms.get(), 1234);
    let (created, _) = room(descriptor);
    let auth = permit(&created, &source).await;
    assert_eq!(
        &source.read_chunk(&auth, 0, usize::MAX).await.unwrap()[..],
        &bytes[..CHUNK_BYTES]
    );
    assert_eq!(
        &source.read_chunk(&auth, 4, 2).await.unwrap()[..],
        &bytes[4..6]
    );
    assert_eq!(
        &source
            .read_chunk(&auth, (bytes.len() - 3) as u64, usize::MAX)
            .await
            .unwrap()[..],
        &bytes[bytes.len() - 3..]
    );
    assert!(
        source
            .read_chunk(&auth, bytes.len() as u64, 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(source.read_chunk(&auth, 0, 0).await.unwrap().is_empty());
    assert!(matches!(
        source.read_chunk(&auth, u64::MAX, 1).await,
        Err(StreamError::InvalidOffset)
    ));
}

#[tokio::test]
async fn empty_source_is_valid_media() {
    let fixture = Fixture::new(b"");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    assert!(source.read_chunk(&auth, 0, 1).await.unwrap().is_empty());
}

#[test]
fn open_rejects_missing_paths_and_directories() {
    let fixture = Fixture::new(b"x");
    assert!(matches!(
        SelectedMedia::open(&fixture.path.with_extension("missing"), MediaId::new()),
        Err(StreamError::Io(_))
    ));
    let directory = SelectedMedia::open(&fixture.dir, MediaId::new());
    // Windows can reject the directory in File::open, before the opened
    // descriptor's regular-file check. Both paths must reject the selection.
    assert!(
        matches!(directory, Err(StreamError::NotRegularFile))
            || (cfg!(windows) && matches!(directory, Err(StreamError::Io(_))))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sparse_offset_over_4gib() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new(b"");
    let offset = 4_294_967_312;
    let mut file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    file.set_len(offset + 6)
        .expect("sparse fixture unsupported");
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(b"marker").unwrap();
    file.sync_all().unwrap();
    assert!(
        file.metadata().unwrap().blocks() * 512 < offset,
        "filesystem did not create a sparse fixture"
    );
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    assert_eq!(
        &source.read_chunk(&auth, offset, 6).await.unwrap()[..],
        b"marker"
    );
}
#[cfg(not(unix))]
#[test]
#[ignore = "sparse fixture allocation must be validated on this platform separately"]
fn sparse_offset_over_4gib() {}

#[tokio::test]
async fn path_replacement_does_not_reopen() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    fs::rename(&fixture.path, fixture.dir.join("old.bin"))
        .expect("rename of selected descriptor unsupported");
    fs::write(&fixture.path, b"replaced").unwrap();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    assert_eq!(
        &source.read_chunk(&auth, 0, 8).await.unwrap()[..],
        b"original"
    );
}

#[tokio::test]
async fn deleted_path_still_reads_selected_descriptor() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    fs::remove_file(&fixture.path).expect("unlink of selected descriptor unsupported");
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    assert_eq!(
        &source.read_chunk(&auth, 0, 8).await.unwrap()[..],
        b"original"
    );
}

#[tokio::test]
async fn truncated_source_reports_change() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    let file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    file.set_len(2).unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 8).await,
        Err(StreamError::MediaChanged)
    ));
    file.set_len(8).unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 8).await,
        Err(StreamError::MediaChanged)
    ));
}

#[tokio::test]
async fn same_length_mtime_change_is_rejected() {
    let fixture = Fixture::new(b"original");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    let file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap() + Duration::from_secs(2);
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 8).await,
        Err(StreamError::MediaChanged)
    ));
}

#[tokio::test]
async fn concurrent_ranges_do_not_share_cursor() {
    let fixture = Fixture::new(&vec![0; CHUNK_BYTES * 2]);
    let mut file = OpenOptions::new().write(true).open(&fixture.path).unwrap();
    file.write_all(b"first!").unwrap();
    file.seek(SeekFrom::Start(CHUNK_BYTES as u64)).unwrap();
    file.write_all(b"second").unwrap();
    file.sync_all().unwrap();
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    for _ in 0..32 {
        let (left, right) = tokio::join!(
            source.read_chunk(&auth, 0, 6),
            source.read_chunk(&auth, CHUNK_BYTES as u64, 6)
        );
        assert_eq!(&left.unwrap()[..], b"first!");
        assert_eq!(&right.unwrap()[..], b"second");
    }
}

#[tokio::test]
async fn cancelled_permit_stops_read() {
    let fixture = Fixture::new(b"secret");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let guest = created
        .room
        .join(&created.invitation, "guest")
        .await
        .unwrap();
    let auth = created
        .room
        .authorize_media(&guest.session, source.descriptor(0).media_id)
        .await
        .unwrap();
    created
        .room
        .kick(&created.host_session, guest.participant_id)
        .await
        .unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 6).await,
        Err(StreamError::Permission(RoomError::Unauthorized))
    ));
}

#[tokio::test]
async fn expired_closed_replaced_and_wrong_media_permits_fail() {
    let fixture = Fixture::new(b"secret");
    let source = fixture.open();
    let (created, clock) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    clock.advance_ms(300_001);
    assert!(matches!(
        source.read_chunk(&auth, 0, 6).await,
        Err(StreamError::Permission(RoomError::Unauthorized))
    ));
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    created.room.close(&created.host_session).await.unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 6).await,
        Err(StreamError::Permission(RoomError::RoomClosed))
    ));
    let (created, _) = room(source.descriptor(0));
    let auth = permit(&created, &source).await;
    let other = fixture.open();
    created
        .room
        .replace_media(&created.host_session, other.descriptor(0))
        .await
        .unwrap();
    assert!(matches!(
        source.read_chunk(&auth, 0, 6).await,
        Err(StreamError::Permission(RoomError::MediaChanged))
    ));
    let other_auth = permit(&created, &other).await;
    assert!(matches!(
        source.read_chunk(&other_auth, 0, 6).await,
        Err(StreamError::MediaChanged)
    ));
}

#[tokio::test]
async fn reader_leases_share_session_limits_and_release_on_drop() {
    assert_eq!(MAX_READERS_PER_SESSION, 2);
    let fixture = Fixture::new(b"x");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let first = permit(&created, &source).await;
    let second = permit(&created, &source).await;
    let limits = ReaderLimits::default();
    let independent = ReaderLimits::default();
    let a = limits.try_acquire(&first).unwrap();
    let b = independent.try_acquire(&second).unwrap();
    assert!(matches!(
        limits.try_acquire(&first),
        Err(StreamError::ReaderLimit)
    ));
    drop(a);
    let c = independent.try_acquire(&second).unwrap();
    drop((b, c));
    let a = limits.try_acquire(&first).unwrap();
    let b = limits.try_acquire(&second).unwrap();
    drop((a, b));
    created.room.close(&created.host_session).await.unwrap();
    assert!(matches!(
        limits.try_acquire(&first),
        Err(StreamError::Permission(RoomError::RoomClosed))
    ));
}

#[tokio::test]
async fn reader_leases_enforce_room_limit_across_sources_and_media_replacement() {
    assert_eq!(MAX_READERS_PER_ROOM, 8);
    let fixture = Fixture::new(b"x");
    let source = fixture.open();
    let (created, _) = room(source.descriptor(0));
    let mut sessions = vec![created.host_session.clone()];
    for _ in 0..4 {
        sessions.push(
            created
                .room
                .join(&created.invitation, "guest")
                .await
                .unwrap()
                .session,
        );
    }
    let limits = ReaderLimits::default();
    let mut leases = Vec::new();
    for session in &sessions[..4] {
        let auth = created
            .room
            .authorize_media(session, source.descriptor(0).media_id)
            .await
            .unwrap();
        leases.push(limits.try_acquire(&auth).unwrap());
        leases.push(limits.try_acquire(&auth).unwrap());
    }
    let auth = created
        .room
        .authorize_media(&sessions[4], source.descriptor(0).media_id)
        .await
        .unwrap();
    assert!(matches!(
        limits.try_acquire(&auth),
        Err(StreamError::ReaderLimit)
    ));
    let replacement = fixture.open();
    created
        .room
        .replace_media(&created.host_session, replacement.descriptor(0))
        .await
        .unwrap();
    let auth = permit(&created, &replacement).await;
    assert!(matches!(
        ReaderLimits::default().try_acquire(&auth),
        Err(StreamError::ReaderLimit)
    ));
    drop(leases);
    assert!(limits.try_acquire(&auth).is_ok());
    let (other, _) = room(replacement.descriptor(0));
    assert!(
        limits
            .try_acquire(&permit(&other, &replacement).await)
            .is_ok()
    );
}

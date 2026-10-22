use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shared_protocol::{
    ClockEpochId, CommandEnvelope, CommandId, ControlMode, ErrorCode, HostTime, MediaDescriptor,
    MediaFingerprint, MediaId, ParticipantId, Playback, PlaybackCommand, PlaybackStatus, RoomId,
    RoomSnapshot, Speed, WireU64,
};

fn roundtrip<T>(value: T)
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let encoded = serde_json::to_string(&value).unwrap();
    assert_eq!(serde_json::from_str::<T>(&encoded).unwrap(), value);
}

fn snapshot() -> RoomSnapshot {
    RoomSnapshot {
        room_id: RoomId::new(),
        media: MediaDescriptor {
            media_id: MediaId::new(),
            size_bytes: WireU64::new(u64::MAX),
            duration_ms: WireU64::new(9_007_199_254_740_993),
            fingerprint: MediaFingerprint::from_bytes([0xAB; 32]),
        },
        revision: WireU64::new(42),
        playback: Playback {
            status: PlaybackStatus::Playing,
            position_ms: WireU64::new(73_000),
            speed: Speed::try_from(1.25).unwrap(),
            updated_at: HostTime {
                epoch: ClockEpochId::new(),
                ticks_ms: WireU64::new(57),
            },
        },
        control: ControlMode::HostOnly,
    }
}

fn envelope() -> CommandEnvelope {
    let state = snapshot();
    CommandEnvelope {
        room_id: state.room_id,
        media_id: state.media.media_id,
        command_id: CommandId::new(),
        issued_at: state.playback.updated_at,
        action: PlaybackCommand::Play,
    }
}

#[test]
fn wire_u64_preserves_precision() {
    assert_eq!(
        serde_json::to_string(&WireU64::new(9_007_199_254_740_993)).unwrap(),
        "\"9007199254740993\""
    );
    for n in [0, 1, 9_007_199_254_740_993, u64::MAX] {
        assert_eq!(WireU64::new(n).get(), n);
        roundtrip(WireU64::new(n));
    }
}

#[test]
fn wire_u64_rejects_noncanonical() {
    for bad in [
        "9007199254740993",
        "-1",
        "null",
        "true",
        "[]",
        "{}",
        "1.0",
        "\"18446744073709551616\"",
        "\"01\"",
        "\"00\"",
        "\"-1\"",
        "\"+1\"",
        "\"\"",
        "\" 1\"",
        "\"1 \"",
        "\"1.0\"",
        "\"1e3\"",
        "\"１２\"",
        "\"000000000000000000000\"",
        "\"1\\n\"",
    ] {
        assert!(
            serde_json::from_str::<WireU64>(bad).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn media_fingerprint_is_canonical_and_redacted_from_debug() {
    let fingerprint = MediaFingerprint::from_bytes([0xAB; 32]);
    assert_eq!(
        serde_json::to_string(&fingerprint).unwrap(),
        format!("\"{}\"", "ab".repeat(32))
    );
    assert_eq!(
        serde_json::from_str::<MediaFingerprint>(&serde_json::to_string(&fingerprint).unwrap())
            .unwrap(),
        fingerprint
    );
    assert!(serde_json::from_str::<MediaFingerprint>(&format!("\"{}\"", "AB".repeat(32))).is_err());
    assert!(serde_json::from_str::<MediaFingerprint>("\"short\"").is_err());
    assert!(!format!("{fingerprint:?}").contains(&"ab".repeat(32)));
}

#[test]
fn speed_accepts_only_prd_values() {
    for (value, ratio) in [
        (0.5, (1, 2)),
        (0.75, (3, 4)),
        (1.0, (1, 1)),
        (1.25, (5, 4)),
        (1.5, (3, 2)),
        (2.0, (2, 1)),
    ] {
        let speed = Speed::try_from(value).unwrap();
        assert_eq!(speed.ratio(), ratio);
        assert_eq!(speed.as_f64(), value);
        assert_eq!(serde_json::to_value(speed).unwrap(), json!(value));
        roundtrip(speed);
    }
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        0.0,
        0.1,
        1.000001,
    ] {
        assert!(Speed::try_from(bad).is_err(), "accepted {bad}");
    }
    for bad in ["0.1", "-1", "null", "\"1\"", "true", "{}", "[]"] {
        assert!(
            serde_json::from_str::<Speed>(bad).is_err(),
            "accepted {bad}"
        );
    }
    assert_eq!(serde_json::from_str::<Speed>("1").unwrap().ratio(), (1, 1));
}

#[test]
fn input_cannot_claim_actor() {
    let mut input = serde_json::to_value(envelope()).unwrap();
    input["participantId"] = serde_json::to_value(ParticipantId::new()).unwrap();
    assert!(serde_json::from_value::<CommandEnvelope>(input).is_err());
}

#[test]
fn uuid_newtypes_are_v4_strings() {
    macro_rules! check_id {
        ($id:ty) => {{
            let id = <$id>::new();
            let value = serde_json::to_value(id).unwrap();
            let text = value.as_str().unwrap();
            let uuid = uuid::Uuid::parse_str(text).unwrap();
            assert_eq!(uuid.get_version_num(), 4);
            assert_eq!(uuid.get_variant(), uuid::Variant::RFC4122);
            assert_eq!(<$id>::parse(text).unwrap(), id);
            assert_eq!(id.to_string(), text);
            roundtrip(id);
            assert_ne!(id, <$id>::default());
            for bad in [
                "",
                "not-a-uuid",
                "00000000-0000-0000-0000-000000000000",
                "550e8400-e29b-11d4-a716-446655440000",
                "550e8400-e29b-41d4-0716-446655440000",
            ] {
                assert!(<$id>::parse(bad).is_err(), "accepted {bad}");
                assert!(serde_json::from_value::<$id>(json!(bad)).is_err());
            }
            for bad in [json!(null), json!(123), json!({}), json!([])] {
                assert!(serde_json::from_value::<$id>(bad).is_err());
            }
        }};
    }
    check_id!(RoomId);
    check_id!(MediaId);
    check_id!(ParticipantId);
    check_id!(CommandId);
    check_id!(ClockEpochId);
}

#[test]
fn room_snapshot_roundtrip_preserves_types_and_values() {
    let state = snapshot();
    let wire = serde_json::to_value(state).unwrap();
    assert_eq!(wire["media"]["sizeBytes"], "18446744073709551615");
    assert_eq!(wire["media"]["durationMs"], "9007199254740993");
    assert_eq!(wire["playback"]["positionMs"], "73000");
    assert_eq!(wire["playback"]["updatedAt"]["ticksMs"], "57");
    assert_eq!(wire["playback"]["status"], "playing");
    assert_eq!(wire["control"], "host-only");
    assert_eq!(wire["revision"], "42");
    roundtrip(state);
    roundtrip(PlaybackStatus::Paused);
    roundtrip(ControlMode::Everyone);
}

#[test]
fn actions_have_strict_tagged_contracts() {
    for (action, wire) in [
        (PlaybackCommand::Play, json!({"type": "play"})),
        (PlaybackCommand::Pause, json!({"type": "pause"})),
        (
            PlaybackCommand::Seek {
                position_ms: WireU64::new(u64::MAX),
            },
            json!({"type": "seek", "positionMs": "18446744073709551615"}),
        ),
        (
            PlaybackCommand::SetSpeed {
                speed: Speed::try_from(0.75).unwrap(),
            },
            json!({"type": "setSpeed", "speed": 0.75}),
        ),
    ] {
        assert_eq!(serde_json::to_value(action).unwrap(), wire);
        let mut command = envelope();
        command.action = action;
        roundtrip(command);
    }
    for bad in [
        json!({"type": "unknown"}),
        json!({"type": "play", "actor": "host"}),
        json!({"type": "pause", "positionMs": "1"}),
        json!({"type": "seek"}),
        json!({"type": "seek", "positionMs": "1", "extra": 1}),
        json!({"type": "setSpeed"}),
        json!({"type": "setSpeed", "speed": 1, "extra": 1}),
        json!({"Play": null}),
        json!(null),
    ] {
        assert!(
            serde_json::from_value::<PlaybackCommand>(bad.clone()).is_err(),
            "accepted {bad}"
        );
    }
}

#[test]
fn every_object_rejects_unknown_fields() {
    let state = serde_json::to_value(snapshot()).unwrap();
    for key in [None, Some("media"), Some("playback")] {
        let mut bad = state.clone();
        let target = match key {
            Some(k) => &mut bad[k],
            None => &mut bad,
        };
        target["unexpected"] = json!(true);
        assert!(serde_json::from_value::<RoomSnapshot>(bad).is_err());
    }
    let mut bad = state;
    bad["playback"]["updatedAt"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<RoomSnapshot>(bad).is_err());
    let mut bad = serde_json::to_value(envelope()).unwrap();
    bad["issuedAt"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<CommandEnvelope>(bad).is_err());
    for key in ["roomId", "mediaId", "commandId", "issuedAt", "action"] {
        let mut bad = serde_json::to_value(envelope()).unwrap();
        bad.as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<CommandEnvelope>(bad).is_err(),
            "missing {key}"
        );
    }
}

#[test]
fn error_codes_roundtrip_as_stable_strings() {
    for (code, wire) in [
        (ErrorCode::InvalidInput, "invalidInput"),
        (ErrorCode::Unauthorized, "unauthorized"),
        (ErrorCode::Forbidden, "forbidden"),
        (ErrorCode::RoomFull, "roomFull"),
        (ErrorCode::RoomClosed, "roomClosed"),
        (ErrorCode::MediaChanged, "mediaChanged"),
        (ErrorCode::CommandConflict, "commandConflict"),
        (ErrorCode::CommandExpired, "commandExpired"),
        (ErrorCode::RateLimited, "rateLimited"),
        (ErrorCode::ClockMismatch, "clockMismatch"),
        (ErrorCode::CounterExhausted, "counterExhausted"),
    ] {
        assert_eq!(
            serde_json::to_value(code).unwrap(),
            Value::String(wire.into())
        );
        roundtrip(code);
    }
    assert!(serde_json::from_str::<ErrorCode>("\"unknown\"").is_err());
}

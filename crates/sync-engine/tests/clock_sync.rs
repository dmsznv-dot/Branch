use shared_protocol::ClockEpochId;
use sync_engine::{Clock, ClockEstimate, ClockEstimator, ClockSample, MonotonicClock};

fn epoch() -> ClockEpochId {
    ClockEpochId::parse("11111111-1111-4111-8111-111111111111").unwrap()
}

fn sample() -> ClockSample {
    ClockSample {
        client_send_ms: 1000,
        host_recv_ms: 1510,
        host_send_ms: 1512,
        client_recv_ms: 1022,
        host_epoch: epoch(),
    }
}

#[test]
fn clock_estimator_ignores_high_rtt() {
    let mut estimator = ClockEstimator::new();
    assert_eq!(estimator.estimate(), None);
    estimator.observe(sample()).unwrap();
    let best = ClockEstimate {
        offset_ms: 500,
        rtt_ms: 20,
        host_epoch: epoch(),
    };
    assert_eq!(estimator.estimate(), Some(best));
    estimator
        .observe(ClockSample {
            client_recv_ms: 1202,
            ..sample()
        })
        .unwrap();
    assert_eq!(estimator.estimate(), Some(best));
    assert!(
        estimator
            .observe(ClockSample {
                client_recv_ms: 3003,
                ..sample()
            })
            .is_err()
    );
    assert_eq!(estimator.estimate(), Some(best));
}

#[test]
fn invalid_intervals_do_not_poison_estimator() {
    let mut estimator = ClockEstimator::default();
    estimator.observe(sample()).unwrap();
    let previous = estimator.estimate();
    for invalid in [
        ClockSample {
            client_recv_ms: 999,
            ..sample()
        },
        ClockSample {
            host_send_ms: 1509,
            ..sample()
        },
        ClockSample {
            host_send_ms: 1533,
            ..sample()
        },
    ] {
        assert!(estimator.observe(invalid).is_err());
        assert_eq!(estimator.estimate(), previous);
    }
}

#[test]
fn valid_new_epoch_resets_samples_but_invalid_epoch_does_not() {
    let mut estimator = ClockEstimator::new();
    estimator.observe(sample()).unwrap();
    let other = ClockEpochId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    assert!(
        estimator
            .observe(ClockSample {
                host_epoch: other,
                client_recv_ms: 999,
                ..sample()
            })
            .is_err()
    );
    assert_eq!(estimator.estimate().unwrap().host_epoch, epoch());
    estimator
        .observe(ClockSample {
            host_epoch: other,
            client_recv_ms: 1102,
            ..sample()
        })
        .unwrap();
    assert_eq!(
        estimator.estimate(),
        Some(ClockEstimate {
            offset_ms: 460,
            rtt_ms: 100,
            host_epoch: other
        })
    );
}

#[test]
fn only_last_eight_valid_samples_compete() {
    let mut estimator = ClockEstimator::new();
    estimator.observe(sample()).unwrap();
    for _ in 0..7 {
        estimator
            .observe(ClockSample {
                client_recv_ms: 1042,
                ..sample()
            })
            .unwrap();
    }
    assert_eq!(estimator.estimate().unwrap().rtt_ms, 20);
    for _ in 0..20 {
        assert!(
            estimator
                .observe(ClockSample {
                    client_recv_ms: 999,
                    ..sample()
                })
                .is_err()
        );
    }
    assert_eq!(estimator.estimate().unwrap().rtt_ms, 20);
    estimator
        .observe(ClockSample {
            client_recv_ms: 1042,
            ..sample()
        })
        .unwrap();
    assert_eq!(estimator.estimate().unwrap().rtt_ms, 40);
}

#[test]
fn estimator_accepts_rtt_boundary_and_signed_offsets() {
    let mut estimator = ClockEstimator::new();
    estimator
        .observe(ClockSample {
            client_recv_ms: 3002,
            ..sample()
        })
        .unwrap();
    assert_eq!(estimator.estimate().unwrap().rtt_ms, 2000);
    estimator
        .observe(ClockSample {
            client_send_ms: 2000,
            client_recv_ms: 2022,
            ..sample()
        })
        .unwrap();
    assert_eq!(estimator.estimate().unwrap().offset_ms, -500);
    estimator
        .observe(ClockSample {
            client_send_ms: u64::MAX,
            client_recv_ms: u64::MAX,
            host_recv_ms: 0,
            host_send_ms: 0,
            host_epoch: epoch(),
        })
        .unwrap();
    assert_eq!(
        estimator.estimate().unwrap().offset_ms,
        -i128::from(u64::MAX)
    );
}

#[test]
fn monotonic_clock_satisfies_injectable_clock_contract() {
    fn assert_clock<T: Clock>() {}
    assert_clock::<MonotonicClock>();
    // Construction accepts an explicit epoch; no wall-clock or sleep is needed.
    let _clock: Box<dyn Clock> = Box::new(MonotonicClock::new(epoch()));
}

#[cfg(feature = "test-support")]
#[test]
fn manual_clock_is_shared_monotonic_and_saturates() {
    use shared_protocol::{HostTime, WireU64};
    use std::sync::Arc;
    use sync_engine::ManualClock;

    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: epoch(),
        ticks_ms: WireU64::new(1000),
    }));
    let injected: Arc<dyn Clock> = clock.clone();
    clock.advance_ms(2000);
    assert_eq!(
        injected.now(),
        HostTime {
            epoch: epoch(),
            ticks_ms: WireU64::new(3000)
        }
    );
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let clock = clock.clone();
            scope.spawn(move || {
                for _ in 0..1000 {
                    clock.advance_ms(1);
                }
            });
        }
    });
    assert_eq!(injected.now().ticks_ms.get(), 7000);
    clock.advance_ms(u64::MAX);
    clock.advance_ms(1);
    assert_eq!(
        injected.now(),
        HostTime {
            epoch: epoch(),
            ticks_ms: WireU64::new(u64::MAX)
        }
    );
}

//! Pure security regression tests: no sockets or agents.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_crypto::*;
fn info() -> PairInfo {
    PairInfo {
        identity: Identity::generate().public_key(),
        static_key: StaticKey::generate().public_key(),
        display_name: "test".into(),
    }
}
fn sides() -> (Pairing, Pairing, PairInfo, PairInfo) {
    let code = PairCode::parse("1234-567890").unwrap();
    let h = info();
    let w = info();
    (
        Pairing::new(PairRole::Hub, &code, h.clone(), 0).unwrap(),
        Pairing::new(PairRole::Worker, &code, w.clone(), 0).unwrap(),
        h,
        w,
    )
}
#[test]
fn pairing_success_requires_both_confirmations() {
    let (mut h, mut w, hi, wi) = sides();
    assert!(h.peer().is_err());
    assert!(w.peer().is_err());
    let mut message = h.start(0).unwrap();
    for n in 1..=6 {
        assert_eq!(message[0], n);
        let receiver = if n % 2 == 1 { &mut w } else { &mut h };
        let out = receiver.receive(&message, 1).unwrap();
        if n < 5 {
            assert!(receiver.peer().is_err());
        }
        if n < 6 {
            message = out.unwrap();
        } else {
            assert!(out.is_none());
        }
    }
    assert_eq!(h.peer().unwrap(), &wi);
    assert_eq!(w.peer().unwrap(), &hi);
    assert!(h.receive(&message, 2).is_err());
    assert!(h.peer().is_err());
}
#[test]
fn pairing_wrong_secret_fails_without_accepting_identity() {
    let (mut h, _, _, _) = sides();
    let code = PairCode::parse("1234-000000").unwrap();
    let mut w = Pairing::new(PairRole::Worker, &code, info(), 0).unwrap();
    let m = h.start(0).unwrap();
    let m = w.receive(&m, 0).unwrap().unwrap();
    let m = h.receive(&m, 0).unwrap().unwrap();
    assert!(w.receive(&m, 0).is_err());
    assert!(w.peer().is_err());
    assert!(w.receive(&m, 0).is_err());
}
#[test]
fn pairing_rejects_replay_out_of_order_reflection_and_every_truncation() {
    for n in 1..=6 {
        let (mut h, mut w, _, _) = sides();
        let mut m = h.start(0).unwrap();
        for step in 1..n {
            m = if step % 2 == 1 {
                w.receive(&m, 0)
            } else {
                h.receive(&m, 0)
            }
            .unwrap()
            .unwrap();
        }
        for end in 0..m.len() {
            let (mut hh, mut ww, _, _) = sides();
            let mut mm = hh.start(0).unwrap();
            for step in 1..n {
                mm = if step % 2 == 1 {
                    ww.receive(&mm, 0)
                } else {
                    hh.receive(&mm, 0)
                }
                .unwrap()
                .unwrap();
            }
            let receiver = if n % 2 == 1 { &mut ww } else { &mut hh };
            assert!(receiver.receive(&mm[..end], 0).is_err());
            assert!(receiver.receive(&mm, 0).is_err());
        }
        let receiver = if n % 2 == 1 { &mut w } else { &mut h };
        let mut bad = m.clone();
        bad[0] = n + 1;
        assert!(receiver.receive(&bad, 0).is_err());
    }
    let (mut h, mut w, _, _) = sides();
    let m = h.start(0).unwrap();
    assert!(h.start(0).is_err());
    w.receive(&m, 0).unwrap();
    assert!(w.receive(&m, 0).is_err());
    let (mut h, _, _, _) = sides();
    let m = h.start(0).unwrap();
    assert!(h.receive(&m, 0).is_err());
}
#[test]
fn pairing_ciphertext_and_confirmation_tampering_is_terminal() {
    for target in 3..=6 {
        let (mut h, mut w, _, _) = sides();
        let mut m = h.start(0).unwrap();
        for step in 1..target {
            m = if step % 2 == 1 {
                w.receive(&m, 0)
            } else {
                h.receive(&m, 0)
            }
            .unwrap()
            .unwrap();
        }
        m[2] ^= 1;
        let r = if target % 2 == 1 { &mut w } else { &mut h };
        assert!(r.receive(&m, 0).is_err());
        assert!(r.peer().is_err());
    }
}
#[test]
fn strict_code_parsing_and_expiry() {
    for text in [
        "123-123456",
        "1234-12345",
        "1234-1234567",
        "1234_123456",
        "１２34-123456",
        "1234-12345x",
        " 1234-123456",
    ] {
        assert!(PairCode::parse(text).is_err());
    }
    let code = PairCode::generate();
    assert!(PairCode::parse(&code.display()).is_ok());
    let (mut h, mut w, _, _) = sides();
    let m = h.start(599).unwrap();
    assert!(w.receive(&m, 600).is_err());
    let (mut h, _, _, _) = sides();
    assert!(h.start(600).is_err());
    let code = PairCode::parse("0000-000000").unwrap();
    let mut h = Pairing::new(PairRole::Hub, &code, info(), 10).unwrap();
    assert!(h.start(9).is_err());
}
fn channels() -> (NoiseChannel, NoiseChannel) {
    let a = StaticKey::generate();
    let b = StaticKey::generate();
    let mut h = NoiseHandshake::new(true, &a, &b.public_key()).unwrap();
    let mut w = NoiseHandshake::new(false, &b, &a.public_key()).unwrap();
    let m = h.write().unwrap();
    assert_eq!(m.len(), 48);
    w.read(&m).unwrap();
    h.read(&w.write().unwrap()).unwrap();
    (h.finish().unwrap(), w.finish().unwrap())
}
#[test]
fn noise_both_directions_and_maximum_record() {
    assert_eq!(NOISE_PATTERN, "Noise_KK_25519_ChaChaPoly_BLAKE2s");
    let (mut a, mut b) = channels();
    for bytes in [vec![], vec![9; 65519]] {
        let cipher = a.seal(&bytes).unwrap();
        assert_eq!(b.open(&cipher).unwrap(), bytes);
        let cipher = b.seal(&bytes).unwrap();
        assert_eq!(a.open(&cipher).unwrap(), bytes);
    }
    assert!(a.seal(&vec![0; 65520]).is_err());
    assert!(a.seal(b"after error").is_err());
}
#[test]
fn noise_tamper_replay_and_out_of_order_fail_closed() {
    for kind in 0..4 {
        let (mut a, mut b) = channels();
        let first = a.seal(b"one").unwrap();
        let second = a.seal(b"two").unwrap();
        let bad = match kind {
            0 => {
                let mut bad = first.clone();
                bad[0] ^= 1;
                bad
            }
            1 => {
                b.open(&first).unwrap();
                first.clone()
            }
            2 => second,
            3 => first[..first.len() - 1].to_vec(),
            _ => unreachable!(),
        };
        assert!(b.open(&bad).is_err());
        assert!(b.open(&first).is_err());
    }
}
#[test]
fn noise_handshake_rejects_wrong_static_replay_order_and_truncation() {
    let a = StaticKey::generate();
    let b = StaticKey::generate();
    let c = StaticKey::generate();
    let mut h = NoiseHandshake::new(true, &a, &b.public_key()).unwrap();
    let m = h.write().unwrap();
    let mut w = NoiseHandshake::new(false, &b, &c.public_key()).unwrap();
    assert!(w.read(&m).is_err());
    for n in 0..m.len() {
        let mut w = NoiseHandshake::new(false, &b, &a.public_key()).unwrap();
        assert!(w.read(&m[..n]).is_err());
        assert!(w.read(&m).is_err());
    }
    let mut w = NoiseHandshake::new(false, &b, &a.public_key()).unwrap();
    w.read(&m).unwrap();
    assert!(w.read(&m).is_err());
    assert!(w.write().is_err());
    let mut w = NoiseHandshake::new(false, &b, &a.public_key()).unwrap();
    assert!(w.write().is_err());
    assert!(w.finish().is_err());
}
#[test]
fn relay_signatures_bind_challenge_identity_and_exact_length() {
    let a = Identity::generate();
    let b = Identity::generate();
    let challenge = [1; 32];
    let sig = a.sign_challenge(&challenge);
    verify_challenge(&a.public_key(), &challenge, &sig).unwrap();
    assert!(verify_challenge(&b.public_key(), &challenge, &sig).is_err());
    assert!(verify_challenge(&a.public_key(), &[2; 32], &sig).is_err());
    for n in 0..64 {
        assert!(verify_challenge(&a.public_key(), &challenge, &sig[..n]).is_err());
    }
}
#[test]
fn static_keys_private_atomic_concurrent_and_persistent() {
    let dir = tempfile::tempdir().unwrap();
    let keys = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| StaticKey::load_or_create(dir.path()).unwrap().public_key()))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(keys.iter().all(|k| *k == keys[0]));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("noise.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::set_permissions(
        dir.path().join("noise.key"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(StaticKey::load_or_create(dir.path()).is_err());
    assert_eq!(
        std::fs::metadata(dir.path().join("noise.key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[test]
fn malformed_public_keys_and_noncontributory_static_keys_are_rejected() {
    let code = PairCode::parse("1234-123456").unwrap();
    for key in [[0; 32], {
        let mut k = [0; 32];
        k[0] = 1;
        k
    }] {
        let mut bad = info();
        bad.static_key = key;
        assert!(Pairing::new(PairRole::Hub, &code, bad, 0).is_err());
        assert!(NoiseHandshake::new(true, &StaticKey::generate(), &key).is_err());
    }
    let mut bad = info();
    bad.identity = [0; 32];
    assert!(Pairing::new(PairRole::Hub, &code, bad, 0).is_err());
    let mut bad = info();
    bad.display_name = "x".repeat(201);
    assert!(Pairing::new(PairRole::Hub, &code, bad, 0).is_err());
    let mut bad = info();
    bad.display_name = "line\nbreak".into();
    assert!(Pairing::new(PairRole::Hub, &code, bad, 0).is_err());
}
#[test]
fn secret_file_symlinks_corruption_and_initial_modes_are_rejected_or_private() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, [1; 32]).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, dir.path().join("noise.key")).unwrap();
    assert!(StaticKey::load_or_create(dir.path()).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), [1; 32]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("noise.key"), [1; 31]).unwrap();
    std::fs::set_permissions(
        dir.path().join("noise.key"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(StaticKey::load_or_create(dir.path()).is_err());
    let dir = tempfile::tempdir().unwrap();
    let finished = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let done = &finished;
        let path = dir.path();
        let observer = scope.spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                for entry in std::fs::read_dir(path).unwrap() {
                    let entry = entry.unwrap();
                    if let Ok(meta) = entry.metadata() {
                        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
                    }
                }
                std::thread::yield_now();
            }
        });
        Identity::load_or_create(path).unwrap();
        StaticKey::load_or_create(path).unwrap();
        finished.store(true, std::sync::atomic::Ordering::Release);
        observer.join().unwrap();
    });
}

#[test]
fn noise_fresh_prologue_rejects_recorded_first_message_and_context_changes() {
    let a = StaticKey::generate();
    let b = StaticKey::generate();
    let context = b"navarch-channel-v1\0worker\0hub\0worker-id:hub-id:nonce-a:nonce-b";
    let mut initiator = NoiseHandshake::with_prologue(true, &a, &b.public_key(), context).unwrap();
    let first = initiator.write().unwrap();
    for i in 0..context.len() {
        let mut changed = context.to_vec();
        changed[i] ^= 1;
        let mut responder =
            NoiseHandshake::with_prologue(false, &b, &a.public_key(), &changed).unwrap();
        assert!(responder.read(&first).is_err());
        assert!(responder.write().is_err());
    }
    let mut responder = NoiseHandshake::with_prologue(false, &b, &a.public_key(), context).unwrap();
    responder.read(&first).unwrap();
    initiator.read(&responder.write().unwrap()).unwrap();
    initiator.finish().unwrap();
    responder.finish().unwrap();
}

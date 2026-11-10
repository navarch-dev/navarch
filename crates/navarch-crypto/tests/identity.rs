//! Identity helpers use only temporary state and operating system entropy.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use navarch_crypto::*;
#[test]
fn identity_round_trip_mode_and_concurrent_creators() {
    let temp = tempfile::tempdir().unwrap();
    let keys = std::thread::scope(|s| {
        let handles: Vec<_> = (0..12)
            .map(|_| s.spawn(|| Identity::load_or_create(temp.path()).unwrap().public_key()))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(keys.iter().all(|k| k == &keys[0]));
    let id = Identity::load_or_create(temp.path()).unwrap();
    assert_eq!(id.machine_id(), machine_id(&keys[0]));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(temp.path().join("identity.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
#[test]
fn derivation_matches_blake2s_vector_and_different_keys() {
    // BLAKE2s-256([0;32]) = 320b5ea99e653bc2b593db4130d10a4e...
    use data_encoding::BASE32_NOPAD;
    assert_eq!(
        machine_id(&[0; 32]),
        BASE32_NOPAD
            .encode(&[
                0x32, 0x0b, 0x5e, 0xa9, 0x9e, 0x65, 0x3b, 0xc2, 0xb5, 0x93, 0xdb, 0x41, 0x30, 0xd1,
                0x0a, 0x4e
            ])
            .to_ascii_lowercase()
    );
    assert_ne!(
        Identity::generate().machine_id(),
        Identity::generate().machine_id()
    );
}
#[test]
fn corrupt_key_is_reported_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("identity.key"), b"broken").unwrap();
    assert!(matches!(
        Identity::load_or_create(temp.path()),
        Err(IdentityError::InvalidKey)
    ));
    assert_eq!(
        std::fs::read(temp.path().join("identity.key")).unwrap(),
        b"broken"
    );
}

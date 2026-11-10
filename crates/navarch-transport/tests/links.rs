//! Only in-memory and temporary Unix sockets; no network or agents.
#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used)]
use bytes::Bytes;
use navarch_crypto::Identity;
use navarch_transport::*;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
fn hello() -> LocalHello {
    LocalHello {
        public_key: Identity::generate().public_key(),
        display_name: "test".into(),
    }
}
#[tokio::test]
async fn local_round_trip_private_mode_live_socket_and_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("w.sock");
    let a = hello();
    let b = hello();
    let listener = LocalListener::bind(&path, a.clone()).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let connect = LocalLink::connect(&path, &b);
    let (client, server) = tokio::join!(connect, listener.accept());
    let mut client = client.unwrap();
    let (mut server, remote) = server.unwrap();
    assert_eq!(remote.public_key, b.public_key);
    assert_eq!(client.peer(), &a.machine_id().unwrap());
    assert_eq!(server.peer(), &b.machine_id().unwrap());
    for bytes in [
        Bytes::new(),
        Bytes::from_static(b"hello"),
        Bytes::from(vec![3; MAX_FRAME]),
    ] {
        let (sent, received) = tokio::join!(client.send(bytes.clone()), server.recv());
        sent.unwrap();
        assert_eq!(received.unwrap(), bytes);
    }
    server.send(Bytes::from_static(b"back")).await.unwrap();
    assert_eq!(client.recv().await.unwrap(), "back");
    assert!(
        client
            .send(Bytes::from(vec![0; MAX_FRAME + 1]))
            .await
            .is_err()
    );
    assert!(PrivateListener::bind(&path).is_err());
    drop(listener);
    assert!(!path.exists());
}
#[tokio::test]
async fn stale_socket_recovers_but_files_and_symlinks_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sock");
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let listener = PrivateListener::bind(&path).unwrap();
    drop(listener);
    assert!(!path.exists());
    std::fs::write(&path, "mine").unwrap();
    assert!(PrivateListener::bind(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"mine");
    let symlink = temp.path().join("link");
    std::os::unix::fs::symlink(&path, &symlink).unwrap();
    assert!(PrivateListener::bind(&symlink).is_err());
}
#[tokio::test]
async fn framing_handles_partial_headers_cancellation_and_oversize() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sock");
    let h = hello();
    let listener = LocalListener::bind(&path, h.clone()).unwrap();
    let raw = tokio::spawn({
        let path = path.clone();
        async move {
            let mut raw = tokio::net::UnixStream::connect(path).await.unwrap();
            let bytes = serde_json::to_vec(&h).unwrap();
            raw.write_all(&(bytes.len() as u32).to_be_bytes())
                .await
                .unwrap();
            raw.write_all(&bytes).await.unwrap();
            raw
        }
    });
    let (mut link, _) = listener.accept().await.unwrap();
    let mut raw = raw.await.unwrap();
    // The peer's introduction response is intentionally left unread: Unix
    // full-duplex framing still permits exercising fragmented inbound frames.
    raw.write_all(&[0, 0]).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), link.recv())
            .await
            .is_err()
    );
    raw.write_all(&[0, 3, b'a']).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), link.recv())
            .await
            .is_err()
    );
    raw.write_all(b"bc").await.unwrap();
    assert_eq!(link.recv().await.unwrap(), "abc");
    raw.write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
        .await
        .unwrap();
    assert!(link.recv().await.is_err());
}
#[tokio::test]
async fn memory_link_limits_and_disconnect() {
    let (mut a, mut b) = memory_pair(hello().machine_id().unwrap(), hello().machine_id().unwrap());
    a.send(Bytes::from(vec![0; MAX_FRAME])).await.unwrap();
    assert_eq!(b.recv().await.unwrap().len(), MAX_FRAME);
    assert!(a.send(Bytes::from(vec![0; MAX_FRAME + 1])).await.is_err());
    drop(a);
    assert!(b.recv().await.is_err());
}

#[tokio::test]
async fn both_peers_can_send_maximum_frames_before_reading() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("full-duplex.sock");
    let a = hello();
    let b = hello();
    let listener = LocalListener::bind(&path, a).unwrap();
    let (a, b) = tokio::join!(LocalLink::connect(&path, &b), listener.accept());
    let mut a = a.unwrap();
    let (mut b, _) = b.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        let (sa, sb) = tokio::join!(
            a.send(Bytes::from(vec![1; MAX_FRAME])),
            b.send(Bytes::from(vec![2; MAX_FRAME]))
        );
        sa.unwrap();
        sb.unwrap();
        let (ra, rb) = tokio::join!(a.recv(), b.recv());
        assert_eq!(ra.unwrap(), Bytes::from(vec![2; MAX_FRAME]));
        assert_eq!(rb.unwrap(), Bytes::from(vec![1; MAX_FRAME]));
    })
    .await
    .unwrap();
}

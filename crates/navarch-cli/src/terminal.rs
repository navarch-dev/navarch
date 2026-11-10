//! Cancellable terminal reads. A permission timeout must not leave a blocking
//! stdin task alive or steal input from a subsequent request.
#[cfg(unix)]
pub(crate) async fn read_line() -> std::io::Result<String> {
    use rustix::fs::{Mode, OFlags, open};
    let fd = open(
        "/dev/tty",
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut result = Vec::new();
    loop {
        let mut bytes = [0; 1024];
        match rustix::io::read(&fd, &mut bytes) {
            Ok(0) => return Ok(String::from_utf8_lossy(&result).into_owned()),
            Ok(n) => {
                result.extend_from_slice(&bytes[..n]);
                if result.contains(&b'\n') {
                    return Ok(String::from_utf8_lossy(&result).into_owned());
                }
            }
            Err(rustix::io::Errno::AGAIN) => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await
            }
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
#[cfg(not(unix))]
pub(crate) async fn read_line() -> std::io::Result<String> {
    // Unix is the deployment target (local links use Unix sockets). On other
    // platforms fail closed rather than leave an uncancellable stdin read.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "interactive permissions require a Unix terminal",
    ))
}

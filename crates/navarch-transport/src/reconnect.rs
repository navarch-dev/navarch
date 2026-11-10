use crate::*;
use rand::Rng;
use std::{path::PathBuf, time::Duration};
/// Source of replacement links. Tests can provide channel-fed in-memory links.
#[async_trait]
pub trait Connector: Send {
    /// Attempt one connection.
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError>;
}
/// Reconnect to the local hub after it restarts.
pub struct LocalConnector {
    /// Hub worker socket path.
    pub path: PathBuf,
    /// Local public identity and display name.
    pub hello: LocalHello,
}
#[async_trait]
impl Connector for LocalConnector {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        Ok(Box::new(LocalLink::connect(&self.path, &self.hello).await?))
    }
}
/// Reconnection wrapper with exponential, jittered backoff (1 to 60 seconds).
/// Reliable delivery explicitly replaces links through this wrapper so it can
/// resend outstanding envelopes before receiving fresh traffic.
pub struct ReconnectingLink {
    connector: Box<dyn Connector>,
    delay: u64,
}
impl ReconnectingLink {
    /// Wrap a connector, starting with a one-second retry interval.
    pub fn new(connector: Box<dyn Connector>) -> Self {
        Self {
            connector,
            delay: 1,
        }
    }
    /// Keep attempting until a connection succeeds. Cancellation is safe.
    pub async fn connect(&mut self) -> Box<dyn Link> {
        loop {
            match self.connector.connect().await {
                Ok(link) => {
                    self.delay = 1;
                    return link;
                }
                Err(error) => {
                    tracing::warn!(%error, "link connection failed; retrying");
                    let millis = rand::thread_rng()
                        .gen_range(self.delay * 1000..=(self.delay * 1500).min(60_000));
                    tokio::time::sleep(Duration::from_millis(millis)).await;
                    self.delay = (self.delay * 2).min(60);
                }
            }
        }
    }
}

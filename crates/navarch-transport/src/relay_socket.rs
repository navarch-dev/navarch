use crate::{LinkError, MAX_FRAME};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt};
use navarch_crypto::Identity;
use navarch_proto::relay::{RelayRequest, RelayResponse};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// An authenticated relay connection. Errors never contain payload bytes or URLs.
pub struct RelaySocket {
    socket: Socket,
}
pub(crate) fn protocol() -> LinkError {
    LinkError::Protocol("relay protocol rejected message".into())
}
pub(crate) fn encode(bytes: &[u8]) -> String {
    B64.encode(bytes)
}
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, LinkError> {
    B64.decode(text).map_err(|_| protocol())
}
impl RelaySocket {
    /// Authenticate a fresh WebSocket with an OS-random challenge signature.
    pub async fn connect(url: &str, identity: &Identity) -> Result<Self, LinkError> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME))
                .max_frame_size(Some(MAX_FRAME));
            let (socket, _) =
                tokio_tungstenite::connect_async_with_config(url, Some(config), false)
                    .await
                    .map_err(|_| protocol())?;
            let mut this = Self { socket };
            let RelayResponse::Challenge { challenge } = this.recv().await? else {
                return Err(protocol());
            };
            let challenge: [u8; 32] = decode(&challenge)?.try_into().map_err(|_| protocol())?;
            this.send(RelayRequest::Authenticate {
                public_key: identity.public_key(),
                signature: encode(&identity.sign_challenge(&challenge)),
            })
            .await?;
            if !matches!(this.recv().await?, RelayResponse::Authenticated) {
                return Err(protocol());
            }
            Ok(this)
        })
        .await
        .map_err(|_| protocol())?
    }
    /// Send one bounded relay command.
    pub async fn send(&mut self, request: RelayRequest) -> Result<(), LinkError> {
        self.socket
            .send(wire(request)?)
            .await
            .map_err(|_| protocol())
    }
    /// Receive one complete bounded response; cancellation is safe.
    pub async fn recv(&mut self) -> Result<RelayResponse, LinkError> {
        parse(self.socket.next().await)
    }
    pub(crate) fn spawn(
        self,
    ) -> (
        mpsc::UnboundedSender<RelayRequest>,
        mpsc::Receiver<Result<RelayResponse, LinkError>>,
        Arc<Guard>,
    ) {
        let (mut write, mut read) = self.socket.split();
        let (tx, mut output) = mpsc::unbounded_channel();
        let (input, rx) = mpsc::channel(64);
        let writer = tokio::spawn(async move {
            while let Some(request) = output.recv().await {
                let Ok(message) = wire(request) else {
                    break;
                };
                if write.send(message).await.is_err() {
                    break;
                }
            }
        });
        let reader = tokio::spawn(async move {
            loop {
                let message = parse(read.next().await);
                let failed = message.is_err();
                if input.send(message).await.is_err() || failed {
                    break;
                }
            }
        });
        (tx, rx, Arc::new(Guard(vec![writer, reader])))
    }
}
pub(crate) struct Guard(pub Vec<tokio::task::JoinHandle<()>>);
impl Drop for Guard {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}
fn wire(request: RelayRequest) -> Result<Message, LinkError> {
    let text = serde_json::to_string(&request).map_err(|_| protocol())?;
    if text.len() > MAX_FRAME {
        return Err(protocol());
    }
    Ok(Message::Text(text.into()))
}
fn parse(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<RelayResponse, LinkError> {
    match message {
        Some(Ok(Message::Text(text))) if text.len() <= MAX_FRAME => {
            serde_json::from_str(&text).map_err(|_| protocol())
        }
        _ => Err(LinkError::Closed),
    }
}

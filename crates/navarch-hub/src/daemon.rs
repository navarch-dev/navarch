use crate::{
    Hub, HubError,
    control::{Request, Response, read_line},
};
use navarch_proto::MachineId;
use navarch_transport::{
    Connector, Link, LinkError, LocalHello, LocalListener, PrivateListener, ReliableEvent, reliable,
};
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
    task::JoinSet,
};

pub(crate) struct Incoming(pub(crate) mpsc::UnboundedReceiver<Box<dyn Link>>);
#[async_trait::async_trait]
impl Connector for Incoming {
    async fn connect(&mut self) -> Result<Box<dyn Link>, LinkError> {
        self.0.recv().await.ok_or(LinkError::Closed)
    }
}
impl Hub {
    /// Serve a local worker using the shared reliable path. Tests inject only
    /// in-memory links; production feeds newly accepted Unix links to a connector.
    pub async fn run_local_peer(
        &self,
        local: MachineId,
        hello: LocalHello,
        connector: Box<dyn Connector>,
    ) -> Result<(), HubError> {
        let machine = self.register_local(&hello)?;
        self.run_peer(local, machine, connector).await
    }
    /// Serve a previously paired active remote using the existing delivery path.
    pub async fn run_peer(
        &self,
        local: MachineId,
        machine: MachineId,
        connector: Box<dyn Connector>,
    ) -> Result<(), HubError> {
        let connector = Box::new(crate::remote::ActiveConnector {
            hub: self.clone(),
            inner: connector,
        });
        let (sender, mut events, _task) = reliable(local, connector, 4096);
        let mut token = None;
        struct Detach {
            hub: Hub,
            machine: MachineId,
            token: Option<uuid::Uuid>,
        }
        impl Drop for Detach {
            fn drop(&mut self) {
                if let Some(token) = self.token {
                    let _ = self.hub.detach(&self.machine, token);
                }
            }
        }
        let mut cleanup = Detach {
            hub: self.clone(),
            machine: machine.clone(),
            token: None,
        };
        while let Some(event) = events.recv().await {
            match event {
                ReliableEvent::Connected(peer) => {
                    if peer != machine {
                        return Err(HubError::Invalid(
                            "local introduction and link address differ".into(),
                        ));
                    }
                    self.require_active(&machine)?;
                    token = Some(self.attach(machine.clone(), sender.clone())?);
                    cleanup.token = token;
                }
                ReliableEvent::Disconnected(_) => {
                    if let Some(token) = token.take() {
                        self.detach(&machine, token)?;
                    }
                    cleanup.token = None;
                }
                ReliableEvent::Message(delivery) => {
                    let result = self.apply(&delivery.envelope);
                    delivery.complete(result.is_ok());
                    result?;
                }
            }
        }
        Ok(())
    }
}
/// Run the local hub daemon until cancelled. Both Unix sockets have mode 0600;
/// cancelling the future stops all peer/control tasks and unlinks its sockets.
pub async fn serve(
    home: &Path,
    hello: LocalHello,
    store: Arc<dyn navarch_store::Store>,
) -> Result<(), HubError> {
    serve_with_relay(home, hello, store, None).await
}
/// Serve local sockets and optionally maintain a remote relay connection.
pub async fn serve_with_relay(
    home: &Path,
    hello: LocalHello,
    store: Arc<dyn navarch_store::Store>,
    remote: Option<crate::RemoteRelay>,
) -> Result<(), HubError> {
    let local = hello.machine_id()?;
    let workers = Arc::new(LocalListener::bind(&home.join("hub-worker.sock"), hello)?);
    let control = PrivateListener::bind(&home.join("hub.sock"))?;
    let hub = Hub::new(store);
    let mut peers: HashMap<MachineId, mpsc::UnboundedSender<Box<dyn Link>>> = HashMap::new();
    let mut tasks = JoinSet::new();
    if let Some(remote) = remote {
        let hub = hub.clone();
        let local = local.clone();
        tasks.spawn(async move { hub.serve_relay(local, remote).await });
    }
    // Handshakes run separately so a stalled client cannot block control requests.
    let mut accepts = JoinSet::new();
    let listener = workers.clone();
    accepts.spawn(async move { listener.accept().await });
    loop {
        tokio::select! {
            stream = control.accept() => {
                let hub = hub.clone(); let stream = stream?;
                tasks.spawn(async move { control_connection(hub,stream).await });
            }
            Some(result) = accepts.join_next() => {
                let listener = workers.clone(); accepts.spawn(async move {listener.accept().await});
                if let Ok(Ok((link,hello))) = result {
                    let id = hello.machine_id()?;
                    if peers.get(&id).is_none_or(|s| s.is_closed()) {
                        let (tx,rx) = mpsc::unbounded_channel(); peers.insert(id.clone(),tx);
                        let hub = hub.clone(); let local = local.clone();
                        tasks.spawn(async move {hub.run_local_peer(local,hello,Box::new(Incoming(rx))).await});
                    }
                    if let Some(sender) = peers.get(&id) { let _ = sender.send(Box::new(link)); }
                }
            }
            Some(_) = tasks.join_next() => {},
        }
    }
}
async fn control_connection(hub: Hub, stream: UnixStream) -> Result<(), HubError> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let line = match read_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(error) => {
                let mut bytes = serde_json::to_vec(&Response::Error {
                    message: error.to_string(),
                })?;
                bytes.push(b'\n');
                write.write_all(&bytes).await?;
                return Ok(());
            }
        };
        let result = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => hub.request_async(request).await,
            Err(error) => Err(error.into()),
        };
        let response = match result {
            Ok(value) => Response::Ok { value },
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        };
        let mut bytes = serde_json::to_vec(&response)?;
        bytes.push(b'\n');
        write.write_all(&bytes).await?;
    }
}

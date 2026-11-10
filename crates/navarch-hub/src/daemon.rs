use crate::{
    Hub, HubError,
    control::{Request, Response, read_line},
};
use navarch_proto::MachineId;
use navarch_transport::{
    Connector, Link, LocalHello, LocalListener, PrivateListener, ReliableEvent, reliable,
};
use std::{path::Path, sync::Arc};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    net::UnixStream,
    task::JoinSet,
};

impl Hub {
    pub(crate) fn admit_link(
        &self,
        local: MachineId,
        machine: MachineId,
        link: Box<dyn Link>,
        tasks: &mut JoinSet<Result<(), HubError>>,
    ) -> Result<(), HubError> {
        self.require_active(&machine)?;
        let mut state = self.state()?;
        if state
            .links
            .get(&machine)
            .is_none_or(|sender| sender.is_closed())
        {
            let (sender, connector) = navarch_transport::replacement_channel();
            state.links.insert(machine.clone(), sender);
            let hub = self.clone();
            let peer = machine.clone();
            tasks.spawn(async move { hub.run_peer(local, peer, Box::new(connector)).await });
        }
        if let Some(sender) = state.links.get(&machine) {
            sender.replace(link)?;
        }
        Ok(())
    }

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
                    if sender.connection_kind() != "local" {
                        let direct = self
                            .endpoints
                            .lock()
                            .map_err(|e| HubError::Invalid(e.to_string()))?
                            .clone();
                        sender.send(navarch_proto::Body::HubEndpoints { direct })?;
                    }
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
    serve_with_links(home, hello, store, remote, None).await
}
/// Serve local, relay and explicitly configured direct listeners with one outbox per machine.
pub async fn serve_with_links(
    home: &Path,
    hello: LocalHello,
    store: Arc<dyn navarch_store::Store>,
    remote: Option<crate::RemoteRelay>,
    direct: Option<crate::RemoteDirect>,
) -> Result<(), HubError> {
    let local = hello.machine_id()?;
    let workers = Arc::new(LocalListener::bind(&home.join("hub-worker.sock"), hello)?);
    let control = PrivateListener::bind(&home.join("hub.sock"))?;
    let hub = Hub::new(store);
    let mut tasks = JoinSet::new();
    let direct_listener = if let Some(direct) = direct {
        Some(hub.direct_listener(&local, &direct).await?)
    } else {
        None
    };
    if let Some(remote) = remote {
        let hub = hub.clone();
        let local = local.clone();
        tasks.spawn(async move { hub.serve_relay(local, remote).await });
    }
    if let Some(listener) = direct_listener {
        let hub = hub.clone();
        let local = local.clone();
        tasks.spawn(async move { hub.serve_direct_listener(local, listener).await });
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
                    hub.register_local(&hello)?;
                    hub.admit_link(local.clone(), id, Box::new(link), &mut tasks)?;
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
                let bytes = response_line(&Response::Error {
                    message: error.to_string(),
                })?;
                write.write_all(&bytes).await?;
                return Ok(());
            }
        };
        let result = match serde_json::from_slice::<Request>(&line) {
            Ok(Request::Subscribe { after_seq }) => {
                return stream_events(&hub, &mut write, after_seq, false).await;
            }
            Ok(Request::SubscribeLive) => {
                return stream_events(&hub, &mut write, None, true).await;
            }
            Ok(request) => hub.request_async(request).await,
            Err(error) => Err(error.into()),
        };
        let response = match result {
            Ok(value) => Response::Ok { value },
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        };
        let bytes = response_line(&response)?;
        write.write_all(&bytes).await?;
    }
}

fn response_line(response: &Response) -> Result<Vec<u8>, HubError> {
    let mut bytes = serde_json::to_vec(response)?;
    if bytes.len() >= navarch_transport::MAX_FRAME {
        bytes = serde_json::to_vec(&Response::Error {
            message: "control response exceeds 1 MiB; request a smaller result or a narrower query"
                .into(),
        })?;
    }
    bytes.push(b'\n');
    Ok(bytes)
}

async fn stream_events<W: tokio::io::AsyncWrite + Unpin>(
    hub: &Hub,
    write: &mut W,
    after_seq: Option<u64>,
    live_only: bool,
) -> Result<(), HubError> {
    let (mut cursor, replay, mut receiver) = if live_only {
        let (cursor, receiver) = hub.subscribe_live_events()?;
        event_line(write, &serde_json::json!({"kind":"ready", "seq":cursor})).await?;
        (cursor, vec![], receiver)
    } else {
        let (replay, receiver) = hub.subscribe_events(after_seq)?;
        (after_seq.unwrap_or(0), replay, receiver)
    };
    for event in replay {
        cursor = cursor.max(event.seq);
        event_line(write, &event).await?;
    }
    loop {
        match receiver.recv().await {
            Ok(event) if event.seq > cursor => {
                cursor = event.seq;
                event_line(write, &event).await?;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                event_line(write, &serde_json::json!({"kind":"lagged"})).await?;
                return Ok(());
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}
async fn event_line<W: tokio::io::AsyncWrite + Unpin>(
    write: &mut W,
    value: &impl serde::Serialize,
) -> Result<(), HubError> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    tokio::time::timeout(std::time::Duration::from_secs(10), write.write_all(&line))
        .await
        .map_err(|_| HubError::Invalid("slow event subscriber timed out".into()))??;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod event_tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[tokio::test]
    async fn oversized_card_response_is_a_bounded_error_and_the_connection_stays_usable() {
        use navarch_store::Store;
        let store = Arc::new(navarch_store::MemoryStore::default());
        let card_id = navarch_proto::CardId::new();
        store
            .put_task(navarch_store::TaskCard {
                id: card_id,
                title: "Legacy card".into(),
                project: None,
                state: "finished".into(),
                steps: vec![],
                log: vec![navarch_store::CardLog {
                    time: 1,
                    kind: "progress".into(),
                    step: navarch_proto::TaskId::new(),
                    summary: "x".repeat(navarch_transport::MAX_FRAME),
                }],
                applied_events: vec![],
                permissions: vec![],
            })
            .unwrap();
        let hub = Hub::new(store.clone());
        let (client, server) = UnixStream::pair().unwrap();
        let connection = tokio::spawn(async move { control_connection(hub, server).await });
        let (read, mut write) = client.into_split();
        let mut reader = BufReader::new(read);
        for (request, oversized) in [
            (
                Request::GetTask {
                    task_id: card_id,
                    include_log: true,
                },
                true,
            ),
            (
                Request::WaitForEvents {
                    timeout_secs: Some(0),
                    after_seq: None,
                },
                false,
            ),
        ] {
            let mut bytes = serde_json::to_vec(&request).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).await.unwrap();
            let line = read_line(&mut reader).await.unwrap().unwrap();
            assert!(line.len() <= navarch_transport::MAX_FRAME);
            match serde_json::from_slice::<Response>(&line).unwrap() {
                Response::Error { message } if oversized => {
                    assert!(message.contains("response exceeds 1 MiB"));
                }
                Response::Ok { value } if !oversized => {
                    assert_eq!(value, serde_json::json!([]));
                }
                response => panic!("unexpected response: {response:?}"),
            }
        }
        assert!(store.inbox(false).unwrap().is_empty());
        drop(write);
        connection.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn live_subscription_skips_approved_machine_backlog_and_keeps_events_during_ready() {
        let hub = Hub::new(Arc::new(navarch_store::MemoryStore::default()));
        let peer = || navarch_crypto::PairInfo {
            identity: navarch_crypto::Identity::generate().public_key(),
            static_key: navarch_crypto::StaticKey::generate().public_key(),
            display_name: "worker".into(),
        };
        let old = hub.register_paired(peer()).unwrap();
        hub.request(Request::ApproveMachine { machine: old })
            .unwrap();
        let (writer, reader) = tokio::io::duplex(1);
        let h = hub.clone();
        let stream = tokio::spawn(async move {
            stream_events(&h, &mut { writer }, None, true)
                .await
                .unwrap()
        });
        // Ready cannot finish writing before the client reads. Publish in that gap.
        let mut reader = BufReader::new(reader);
        assert_eq!(reader.fill_buf().await.unwrap(), b"{");
        let new = hub.register_paired(peer()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            serde_json::json!({"kind":"ready","seq":1})
        );
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let event: crate::HubEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(event.seq, 2);
        assert_eq!(event.machine, new);
        assert_eq!(hub.store.inbox(false).unwrap().len(), 2);
        stream.abort();
    }

    #[tokio::test]
    async fn slow_subscriber_gets_one_lagged_line_without_blocking_publishers() {
        let hub = Hub::new(Arc::new(navarch_store::MemoryStore::default()));
        let machine = "aaaaaaaaaaaaaaaaaaaaaaaaaa".parse().unwrap();
        let (writer, reader) = tokio::io::duplex(1);
        let h = hub.clone();
        let stream = tokio::spawn(async move {
            stream_events(&h, &mut { writer }, None, false)
                .await
                .unwrap()
        });
        tokio::task::yield_now().await;
        for _ in 0..1024 {
            hub.lifecycle(&machine, "worker_online").unwrap();
        }
        // The publisher completed without the subscriber reading even one byte.
        let mut reader = BufReader::new(reader);
        let mut lagged = 0;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            if serde_json::from_str::<serde_json::Value>(&line).unwrap()["kind"] == "lagged" {
                lagged += 1;
            }
        }
        stream.await.unwrap();
        assert_eq!(lagged, 1);
    }
}

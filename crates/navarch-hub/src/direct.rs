//! Explicit direct listener configuration and authenticated link admission.
use crate::{Hub, HubError};
use navarch_crypto::StaticKey;
use navarch_proto::MachineId;
use navarch_store::MachineState;
use navarch_transport::DirectListener;
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
/// Optional hub.toml direct settings. Absent configuration never opens a TCP port.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DirectConfig {
    /// TCP socket address on which to listen.
    pub listen: String,
    /// Explicit advertised socket addresses; otherwise derive safe interface addresses.
    pub advertise: Option<Vec<String>>,
}
/// Direct listener configuration and the hub's pinned static identity.
pub struct RemoteDirect {
    /// Explicit listener settings.
    pub config: DirectConfig,
    /// Persistent hub Noise key, shared with relay channels.
    pub static_key: Arc<StaticKey>,
}
impl Hub {
    /// Bind the configured listener and record authenticated endpoint advertisements.
    pub async fn direct_listener(
        &self,
        local: &MachineId,
        remote: &RemoteDirect,
    ) -> Result<DirectListener, HubError> {
        let hub = self.clone();
        let auth = Arc::new(move |id: &MachineId| {
            hub.machine(id)
                .ok()
                .filter(|m| m.state == MachineState::Active)
                .and_then(|m| m.static_public_key)
        });
        let listener = DirectListener::bind(
            &remote.config.listen,
            local.clone(),
            remote.static_key.clone(),
            auth,
        )
        .await?;
        let endpoints = if let Some(addresses) = &remote.config.advertise {
            addresses.clone()
        } else {
            let bound = listener.local_addr();
            let mut addresses = if bound.ip().is_unspecified() {
                if_addrs::get_if_addrs()?
                    .into_iter()
                    .map(|i| i.ip())
                    .filter(|ip| usable(*ip))
                    .filter(|ip| ip.is_ipv4() == bound.is_ipv4())
                    .collect::<Vec<_>>()
            } else if usable(bound.ip()) {
                vec![bound.ip()]
            } else {
                vec![]
            };
            addresses.sort();
            addresses.dedup();
            addresses.truncate(8);
            addresses
                .into_iter()
                .map(|ip| SocketAddr::new(ip, bound.port()).to_string())
                .collect()
        };
        navarch_proto::validate_direct_endpoints(&endpoints)
            .map_err(|e| HubError::Invalid(e.into()))?;
        *self
            .endpoints
            .lock()
            .map_err(|e| HubError::Invalid(e.to_string()))? = endpoints;
        Ok(listener)
    }
    /// Admit direct connections into the same replacement slots used by the relay.
    pub async fn serve_direct_listener(
        &self,
        local: MachineId,
        mut listener: DirectListener,
    ) -> Result<(), HubError> {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            let (machine, link) = listener.accept().await?;
            if self.require_active(&machine).is_err() {
                continue;
            }
            self.admit_link(local.clone(), machine, Box::new(link), &mut tasks)?;
            while tasks.try_join_next().is_some() {}
        }
    }
}
fn usable(ip: IpAddr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_broadcast(),
            IpAddr::V6(ip) => !ip.is_unicast_link_local(),
        }
}

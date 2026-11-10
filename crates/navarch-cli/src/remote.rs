//! Pairing and relay startup wiring. Secrets are never formatted as diagnostics.
use anyhow::{Context, Result, bail};
use navarch_crypto::{Identity, PairCode, PairInfo, PairRole, Pairing, StaticKey};
use navarch_transport::{RelayConnector, RelaySocket, pair_room, reserve_pair_room};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path, sync::Arc};
/// Hub's relay configuration, in hub.toml.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    /// Full WebSocket URL ending in /v1/ws.
    pub relay: Option<String>,
}
impl HubConfig {
    /// Load inert defaults if absent.
    pub fn load(home: &Path) -> Result<Self> {
        match std::fs::read_to_string(home.join("hub.toml")) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct PairedHub {
    relay: String,
    hub: PairInfo,
}
fn write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}
fn info(home: &Path, name: String) -> Result<PairInfo> {
    Ok(PairInfo {
        identity: Identity::load_or_create(home)?.public_key(),
        static_key: StaticKey::load_or_create(home)?.public_key(),
        display_name: name,
    })
}
/// Create one pairing attempt, show its code after reservation, and save pending.
pub async fn pair(relay: Option<String>) -> Result<u8> {
    let home = crate::home::home()?;
    let mut config = HubConfig::load(&home)?;
    // The daemon serializes this mutation with approval/revocation and reports.
    navarch_hub::control::call(
        &home.join("hub.sock"),
        &navarch_hub::control::Request::ListWorkers,
    )
    .await?;
    let url = relay
        .or(config.relay.clone())
        .context("set relay in hub.toml or pass --relay")?;
    // A transient relay identity avoids replacing the running hub connection.
    // The persistent identity and static key are bound by the encrypted transcript.
    let mut socket = RelaySocket::connect(&url, &Identity::generate()).await?;
    let code = reserve_pair_room(&mut socket).await?;
    let pairing = Pairing::new(PairRole::Hub, &code, info(&home, "hub".into())?, 0)?;
    println!("{}", code.display().as_str());
    let peer = pair_room(&mut socket, code.room(), PairRole::Hub, pairing).await?;
    let result = navarch_hub::control::call(
        &home.join("hub.sock"),
        &navarch_hub::control::Request::RegisterPaired { peer },
    )
    .await?;
    let id = result["machine"]
        .as_str()
        .context("hub did not return paired machine ID")?;
    config.relay = Some(url);
    write(
        &home.join("hub.toml"),
        toml::to_string(&config)?.as_bytes(),
        true,
    )?;
    eprintln!(
        "Paired {id} (pending approval). Start or restart hub serve after changing its relay URL."
    );
    Ok(0)
}
/// Join once and persist only confirmed hub public keys and the relay URL.
pub async fn join(code: String, relay: String) -> Result<u8> {
    let code_text = zeroize::Zeroizing::new(code);
    let code = PairCode::parse(&code_text)?;
    let home = crate::home::home()?;
    if home.join("worker/hub.json").exists() {
        bail!("worker already paired; refusing to replace pinned hub keys");
    }
    let name = join_display_name(&home)?;
    let pairing = Pairing::new(PairRole::Worker, &code, info(&home, name)?, 0)?;
    let mut socket = RelaySocket::connect(&relay, &Identity::generate()).await?;
    let hub = pair_room(&mut socket, code.room(), PairRole::Worker, pairing).await?;
    write(
        &home.join("worker/hub.json"),
        &serde_json::to_vec_pretty(&PairedHub { relay, hub })?,
        false,
    )?;
    eprintln!("Joined hub; approval is required before remote work.");
    eprintln!(
        "Next: run `navarch worker init`, edit worker.toml and worker-policy.toml, then run `navarch worker serve`."
    );
    Ok(0)
}
/// Select a pinned remote connection when no explicit local socket is configured.
pub fn worker_connector(
    home: &Path,
    identity: Arc<Identity>,
    local_socket: Option<std::path::PathBuf>,
    display_name: String,
) -> Result<Box<dyn navarch_transport::Connector>> {
    if local_socket.is_none() && home.join("worker/hub.json").exists() {
        let paired: PairedHub =
            serde_json::from_slice(&std::fs::read(home.join("worker/hub.json"))?)?;
        return Ok(Box::new(RelayConnector {
            url: paired.relay,
            identity,
            static_key: Arc::new(StaticKey::load_or_create(home)?),
            peer: navarch_crypto::machine_id(&paired.hub.identity).parse()?,
            peer_static: paired.hub.static_key,
        }));
    }
    Ok(Box::new(navarch_transport::LocalConnector {
        path: local_socket.unwrap_or_else(|| home.join("hub-worker.sock")),
        hello: navarch_transport::LocalHello {
            public_key: identity.public_key(),
            display_name,
        },
    }))
}

fn host_display_name(host: &str) -> String {
    let mut name = String::new();
    for ch in host.chars().filter(|ch| !ch.is_control()) {
        if name.len() + ch.len_utf8() > 200 {
            break;
        }
        name.push(ch);
    }
    name
}
fn join_display_name(home: &Path) -> Result<String> {
    // Only an absent file selects the OS hostname. Invalid configured names
    // remain errors in pairing validation, rather than being repaired.
    match std::fs::read_to_string(home.join("worker.toml")) {
        Ok(text) => Ok(
            navarch_worker::config::Config::parse(&text, &home.join("worker.toml"))?.display_name,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let host = system_hostname()?;
            let name = host_display_name(&host);
            if name.is_empty() {
                bail!(
                    "system hostname has no displayable characters; configure display_name in worker.toml"
                );
            }
            Ok(name)
        }
        Err(error) => Err(error.into()),
    }
}
#[cfg(unix)]
fn system_hostname() -> Result<String> {
    Ok(rustix::system::uname()
        .nodename()
        .to_str()
        .context("system hostname is not UTF-8")?
        .to_owned())
}
#[cfg(not(unix))]
fn system_hostname() -> Result<String> {
    bail!("hostname lookup is unsupported on this platform; configure display_name in worker.toml")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn hostname_is_control_free_and_bounded_on_utf8_boundary() {
        assert_eq!(host_display_name("host\n\t\u{7f}\u{85}name"), "hostname");
        assert_eq!(host_display_name(&"界".repeat(100)), "界".repeat(66));
        assert_eq!(host_display_name(&"x".repeat(201)).len(), 200);
        assert!(host_display_name("\n\t").is_empty());
    }
    #[test]
    fn join_uses_hostname_only_for_missing_configuration() {
        let home = tempfile::tempdir().unwrap();
        let host = system_hostname().unwrap();
        assert_eq!(
            join_display_name(home.path()).unwrap(),
            host_display_name(&host)
        );
        std::fs::write(
            home.path().join("worker.toml"),
            "display_name = 'configured'",
        )
        .unwrap();
        assert_eq!(join_display_name(home.path()).unwrap(), "configured");
        std::fs::write(home.path().join("worker.toml"), "display_name = ''").unwrap();
        assert_eq!(join_display_name(home.path()).unwrap(), "");
        std::fs::write(home.path().join("worker.toml"), "invalid toml").unwrap();
        assert!(join_display_name(home.path()).is_err());
    }
}

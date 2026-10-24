//! Persistent Docker-style network definitions and container address allocation.
//! The store catalog lock serializes definitions and reservations, never packets.
#![deny(missing_docs)]

use super::*;
use std::net::Ipv4Addr;

/// A persistent, store-scoped logical bridge or the built-in offline network.
/// The bridge driver uses direct Unix sockets, not a host Linux bridge.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Full 64-hex network identity, retained across CLI and owner restarts.
    pub id: String,
    /// Unique Docker-style name, 1–128 ASCII alphanumeric/underscore/dot/hyphen.
    pub name: String,
    /// IPv4 /24 CIDR for bridges; None for the offline network.
    pub subnet: Option<String>,
    /// A usable host in subnet, excluded from address allocation; None offline.
    pub gateway: Option<Ipv4Addr>,
    /// Built-in bridge and none cannot be removed.
    pub builtin: bool,
    /// Creation time in Unix milliseconds; not a monotonic clock.
    pub created_ms: u64,
}

/// Request to create one private IPv4 /24 network.
#[derive(Clone, Debug)]
pub struct NetworkCreate {
    /// Unique Docker-style network name. bridge, host and none are reserved.
    pub name: String,
    /// Optional RFC1918 or 192.0.2.0/24 subnet. Omission chooses a free /24
    /// from 172.18.0.0–172.31.255.0, avoiding existing networks in this store.
    pub subnet: Option<String>,
    /// Optional usable host in subnet; omission uses its first host.
    pub gateway: Option<Ipv4Addr>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    networks: Vec<Network>,
}

fn subnet(value: &str) -> anyhow::Result<Ipv4Addr> {
    let (address, prefix) = value
        .split_once('/')
        .context("network subnet must be IPv4 CIDR")?;
    let address: Ipv4Addr = address
        .parse()
        .context("network subnet must be IPv4 CIDR")?;
    let [a, b, c, d] = address.octets();
    ensure!(
        prefix == "24" && d == 0,
        "only canonical IPv4 /24 subnets are supported"
    );
    ensure!(
        a == 10
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || [a, b, c] == [192, 0, 2],
        "network subnet must be RFC1918 or 192.0.2.0/24"
    );
    Ok(address)
}

fn usable(address: Ipv4Addr, network: &Network) -> bool {
    let Some(cidr) = &network.subnet else {
        return false;
    };
    let Ok(base) = subnet(cidr) else {
        return false;
    };
    let bytes = address.octets();
    bytes[..3] == base.octets()[..3]
        && (1..=254).contains(&bytes[3])
        && Some(address) != network.gateway
}

pub(super) fn key(network: &Network) -> &str {
    &network.id[..24]
}

impl Store {
    fn network_catalog(&self) -> anyhow::Result<Vec<Network>> {
        let path = self.root.join("networks.json");
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
                && metadata.nlink() == 1
                && metadata.len() <= 1024 * 1024,
            "unsafe network catalog"
        );
        let catalog: Catalog = serde_json::from_reader(file.take(1024 * 1024 + 1))?;
        ensure!(catalog.version == 1, "unknown network catalog version");
        let mut names = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        let mut subnets = std::collections::BTreeSet::new();
        for network in &catalog.networks {
            ensure!(
                valid_name(&network.name)
                    && network.id.len() == 64
                    && network.id.bytes().all(|b| b.is_ascii_hexdigit())
                    && names.insert(&network.name)
                    && ids.insert(&network.id),
                "invalid network identity"
            );
            if let Some(cidr) = &network.subnet {
                let base = subnet(cidr)?;
                let Some(gateway) = network.gateway else {
                    bail!("network missing gateway");
                };
                ensure!(
                    gateway.octets()[..3] == base.octets()[..3]
                        && (1..=254).contains(&gateway.octets()[3])
                        && subnets.insert(cidr),
                    "invalid network IPAM"
                );
            } else {
                ensure!(
                    network.name == "none" && network.builtin && network.gateway.is_none(),
                    "invalid offline network"
                );
            }
        }
        ensure!(
            catalog
                .networks
                .iter()
                .any(|n| n.name == "bridge" && n.builtin && n.subnet.is_some())
                && catalog
                    .networks
                    .iter()
                    .any(|n| n.name == "none" && n.builtin),
            "missing built-in networks"
        );
        Ok(catalog.networks)
    }

    fn save_networks(&self, networks: Vec<Network>) -> anyhow::Result<()> {
        crate::util::write_private_json(
            &self.root.join("networks.json"),
            &Catalog {
                version: 1,
                networks,
            },
        )
    }

    pub(super) fn initialize_networks(&self) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        match fs::symlink_metadata(self.root.join("networks.json")) {
            Ok(_) => {
                self.network_catalog()?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.save_networks(vec![
                    Network {
                        id: format!("{}{}", uuid(), uuid()),
                        name: "bridge".into(),
                        subnet: Some("172.17.0.0/24".into()),
                        gateway: Some(Ipv4Addr::new(172, 17, 0, 1)),
                        builtin: true,
                        created_ms: crate::unix_now_ms(),
                    },
                    Network {
                        id: format!("{}{}", uuid(), uuid()),
                        name: "none".into(),
                        subnet: None,
                        gateway: None,
                        builtin: true,
                        created_ms: crate::unix_now_ms(),
                    },
                ])?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn resolve_network(&self, selector: &str) -> anyhow::Result<Network> {
        let networks = self.network_catalog()?;
        if let Some(network) = networks.iter().find(|n| n.name == selector) {
            return Ok(network.clone());
        }
        let mut matches = networks
            .into_iter()
            .filter(|n| !selector.is_empty() && n.id.starts_with(selector));
        let network = matches.next().context("network not found")?;
        ensure!(matches.next().is_none(), "ambiguous network ID");
        Ok(network)
    }

    /// List persistent networks, including built-in bridge and none. Read-only
    /// callers serialize with catalog mutation; no packet endpoints are created.
    pub fn networks(&self) -> anyhow::Result<Vec<Network>> {
        let _guard = self.lock()?;
        self.network_catalog()
    }

    /// Resolve a network by exact name or unique nonempty ID prefix.
    /// Unknown or ambiguous selectors fail without changing network state.
    pub fn inspect_network(&self, selector: &str) -> anyhow::Result<Network> {
        let _guard = self.lock()?;
        self.resolve_network(selector)
    }

    /// Create one network atomically. Duplicate names, overlapping subnets,
    /// reserved names and unsupported IPAM fail without publishing a definition.
    /// Socket transport starts only when an attached container starts.
    pub fn create_network(&self, request: NetworkCreate) -> anyhow::Result<Network> {
        ensure!(
            valid_name(&request.name)
                && !["bridge", "none", "host"].contains(&request.name.as_str()),
            "invalid or reserved network name"
        );
        let _guard = self.lock()?;
        let mut networks = self.network_catalog()?;
        ensure!(networks.len() < 1024, "network catalog capacity exhausted");
        ensure!(
            !networks.iter().any(|n| n.name == request.name),
            "network name already exists"
        );
        let cidr = if let Some(cidr) = request.subnet {
            subnet(&cidr)?;
            cidr
        } else {
            (18..=31)
                .flat_map(|b| (0..=255).map(move |c| format!("172.{b}.{c}.0/24")))
                .find(|cidr| !networks.iter().any(|n| n.subnet.as_ref() == Some(cidr)))
                .context("no free IPv4 subnet")?
        };
        ensure!(
            !networks.iter().any(|n| n.subnet.as_ref() == Some(&cidr)),
            "network subnet overlaps an existing network"
        );
        let base = subnet(&cidr)?;
        let gateway = request
            .gateway
            .unwrap_or_else(|| Ipv4Addr::from(u32::from(base) + 1));
        ensure!(
            gateway.octets()[..3] == base.octets()[..3] && (1..=254).contains(&gateway.octets()[3]),
            "gateway must be a usable host in subnet"
        );
        let network = Network {
            id: format!("{}{}", uuid(), uuid()),
            name: request.name,
            subnet: Some(cidr),
            gateway: Some(gateway),
            builtin: false,
            created_ms: crate::unix_now_ms(),
        };
        networks.push(network.clone());
        self.save_networks(networks)?;
        Ok(network)
    }

    /// List attached containers, including stopped address reservations.
    /// Returns existing Instance contracts; observations are not VM liveness proof.
    pub fn network_instances(&self, selector: &str) -> anyhow::Result<Vec<Instance>> {
        let _guard = self.lock()?;
        let network = self.resolve_network(selector)?;
        Ok(self
            .all()?
            .into_iter()
            .filter(|instance| attached(instance, &network))
            .collect())
    }

    /// Remove an unused user-defined network. Built-ins and networks with any
    /// container attachment (including stopped containers) are refused. Force
    /// ignores a missing selector only; it never disconnects or kills containers.
    /// Catalog serialization fences concurrent create/attach/remove operations.
    pub fn remove_network(&self, selector: &str, force: bool) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        let networks = self.network_catalog()?;
        let network = match self.resolve_network(selector) {
            Ok(n) => n,
            Err(e) if force && e.to_string() == "network not found" => return Ok(()),
            Err(e) => return Err(e),
        };
        ensure!(!network.builtin, "built-in networks cannot be removed");
        ensure!(
            !self.all()?.iter().any(|i| attached(i, &network)),
            "network has attached containers"
        );
        self.save_networks(
            networks
                .into_iter()
                .filter(|n| n.id != network.id)
                .collect(),
        )
    }

    // Called while the container catalog lock is held. Existing containers keep
    // their address until removal; failed creation publishes no reservation.
    pub(super) fn assign_network(&self, config: &mut Create, name: &str) -> anyhow::Result<()> {
        let Some(selector) = config.network_name.as_deref() else {
            return Ok(());
        };
        let network = self.resolve_network(selector)?;
        config.network_name = Some(network.name.clone());
        if network.name == "none" {
            ensure!(
                !config.network && config.ipv4_address.is_none() && config.peers.is_none(),
                "--network none conflicts with --ip or peer membership"
            );
            return Ok(());
        }
        ensure!(
            config.network && config.peers.is_none(),
            "conflicting network settings"
        );
        ensure!(
            !network.builtin || config.ipv4_address.is_none(),
            "--ip requires a user-defined network"
        );
        let existing = self.all()?;
        ensure!(
            network.builtin
                || !existing
                    .iter()
                    .filter_map(|instance| instance.config.peers.as_ref())
                    .any(|peer| peer.network == key(&network)
                        && peer
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(name))),
            "container DNS name already allocated on network"
        );
        let taken: std::collections::BTreeSet<_> = existing
            .iter()
            .filter_map(|i| i.config.peers.as_ref())
            .filter(|p| p.network == key(&network))
            .map(|p| p.address)
            .collect();
        let address = if let Some(address) = config.ipv4_address {
            ensure!(
                usable(address, &network),
                "--ip must be a usable non-gateway address in network subnet"
            );
            ensure!(!taken.contains(&address), "IPv4 address already allocated");
            address
        } else {
            let base = subnet(network.subnet.as_deref().context("missing subnet")?)?;
            (1..=254)
                .map(|last| Ipv4Addr::from(u32::from(base) + last))
                .find(|ip| usable(*ip, &network) && !taken.contains(ip))
                .context("network has no free IPv4 addresses")?
        };
        let peers = crate::PeerNetworkSettings {
            network: key(&network).into(),
            address,
            gateway: network.gateway.context("missing gateway")?,
            aliases: if network.builtin {
                vec![]
            } else {
                vec![name.into()]
            },
        };
        peers.validate()?;
        config.peers = Some(peers);
        Ok(())
    }

    pub(super) fn validate_network_attachment(&self, instance: &Instance) -> anyhow::Result<()> {
        if let Some(selector) = &instance.config.network_name {
            let network = self.resolve_network(selector)?;
            if instance.config.network {
                let peers = instance
                    .config
                    .peers
                    .as_ref()
                    .context("missing resolved network attachment")?;
                ensure!(
                    peers.network == key(&network)
                        && usable(peers.address, &network)
                        && Some(peers.gateway) == network.gateway,
                    "container network attachment no longer matches its network"
                );
            }
        }
        Ok(())
    }
}

fn attached(instance: &Instance, network: &Network) -> bool {
    instance
        .config
        .peers
        .as_ref()
        .is_some_and(|p| p.network == key(network))
        || (network.name == "none" && instance.config.network_name.as_deref() == Some("none"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("store")).unwrap();
        (directory, store)
    }
    fn create(name: &str, cidr: Option<&str>) -> NetworkCreate {
        NetworkCreate {
            name: name.into(),
            subnet: cidr.map(str::to_owned),
            gateway: None,
        }
    }

    #[test]
    fn docker_network_catalog_is_persistent_and_validates_ipam() {
        let (_directory, store) = store();
        assert_eq!(store.networks().unwrap().len(), 2);
        let one = store
            .create_network(create("app.net", Some("172.20.0.0/24")))
            .unwrap();
        assert_eq!(one.gateway, Some("172.20.0.1".parse().unwrap()));
        assert_eq!(
            store.inspect_network(&one.id[..12]).unwrap().name,
            "app.net"
        );
        assert_eq!(
            Store::open(store.root.clone())
                .unwrap()
                .inspect_network("app.net")
                .unwrap()
                .id,
            one.id
        );
        assert!(store.create_network(create("app.net", None)).is_err());
        assert!(
            store
                .create_network(create("overlap", Some("172.20.0.0/24")))
                .is_err()
        );
        for cidr in [
            "172.21.0.1/24",
            "172.21.0.0/16",
            "8.8.8.0/24",
            "198.18.0.0/24",
            "::/64",
        ] {
            assert!(store.create_network(create("invalid", Some(cidr))).is_err());
        }
        for name in ["host", "bridge", "none", "../escape"] {
            assert!(store.create_network(create(name, None)).is_err());
        }
        let two = store.create_network(create("auto", None)).unwrap();
        assert_ne!(one.subnet, two.subnet);
        assert!(store.remove_network("bridge", true).is_err());
        assert!(store.remove_network("none", true).is_err());
        assert!(store.remove_network("absent", false).is_err());
        store.remove_network("absent", true).unwrap();
        store.remove_network("app.net", false).unwrap();
        assert!(store.inspect_network("app.net").is_err());
    }

    #[test]
    fn docker_network_default_bridge_ipam_and_named_dns_differ() {
        let (_directory, store) = store();
        let mut cfg: Create = serde_json::from_value(serde_json::json!({
            "name":null,"image":"fixture","platform":null,"command":[],"entrypoint":null,
            "env":{},"cwd":null,"user":null,"mounts":[],"ports":[],"cpus":1,
            "memory_mib":128,"auto_remove":false,"interactive":false,"tty":null,
            "network":true,"network_name":"bridge"
        }))
        .unwrap();
        let _guard = store.lock().unwrap();
        store.assign_network(&mut cfg, "one").unwrap();
        let peer = cfg.peers.unwrap();
        assert_eq!(peer.address, "172.17.0.2".parse::<Ipv4Addr>().unwrap());
        assert!(peer.aliases.is_empty());
        let mut cfg = Create {
            peers: None,
            ipv4_address: Some("172.17.0.11".parse().unwrap()),
            ..cfg
        };
        assert!(store.assign_network(&mut cfg, "two").is_err());
        drop(_guard);
        store
            .create_network(create("dev", Some("192.168.50.0/24")))
            .unwrap();
        cfg.network_name = Some("dev".into());
        cfg.ipv4_address = Some("192.168.50.11".parse().unwrap());
        let _guard = store.lock().unwrap();
        store.assign_network(&mut cfg, "web").unwrap();
        let peer = cfg.peers.as_ref().unwrap();
        assert_eq!(peer.aliases, vec!["web"]);
        assert_eq!(peer.gateway, "192.168.50.1".parse::<Ipv4Addr>().unwrap());
        peer.validate().unwrap();
    }
}

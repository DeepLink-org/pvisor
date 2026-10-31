//! Docker network command syntax and rendering.
use super::*;
use clap::{Args, Subcommand};
use std::io::Write;

#[derive(Debug, Args)]
pub(in crate::cli) struct NetworkArgs {
    #[command(subcommand)]
    command: NetworkCommand,
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Create a user-defined IPv4 /24 network.
    Create {
        #[arg(short, long, default_value = "bridge", value_parser = ["bridge"])]
        driver: String,
        #[arg(long)]
        subnet: Option<String>,
        #[arg(long)]
        gateway: Option<std::net::Ipv4Addr>,
        name: String,
    },
    /// List networks.
    #[command(alias = "list")]
    Ls {
        #[arg(short, long)]
        quiet: bool,
        #[arg(long)]
        no_trunc: bool,
    },
    /// Display network definitions and attached container addresses as JSON.
    Inspect {
        #[arg(required = true, num_args = 1..)]
        networks: Vec<String>,
    },
    /// Remove unused user-defined networks.
    #[command(alias = "remove")]
    Rm {
        /// Ignore missing networks; attached/built-in networks are still refused.
        #[arg(short, long)]
        force: bool,
        #[arg(required = true, num_args = 1..)]
        networks: Vec<String>,
    },
}

pub(super) fn run(args: NetworkArgs) -> anyhow::Result<i32> {
    let store = Store::discover()?;
    let mut output = std::io::stdout().lock();
    match args.command {
        NetworkCommand::Create {
            driver: _,
            subnet,
            gateway,
            name,
        } => {
            writeln!(
                output,
                "{}",
                store
                    .create_network(docker::NetworkCreate {
                        name,
                        subnet,
                        gateway
                    })?
                    .id
            )?;
        }
        NetworkCommand::Ls { quiet, no_trunc } => {
            if !quiet {
                writeln!(output, "NETWORK ID\tNAME\tDRIVER\tSCOPE")?;
            }
            for network in store.networks()? {
                let id = if no_trunc {
                    &network.id
                } else {
                    &network.id[..12]
                };
                if quiet {
                    writeln!(output, "{id}")?;
                } else {
                    writeln!(
                        output,
                        "{id}\t{}\t{}\tlocal",
                        network.name,
                        if network.subnet.is_some() {
                            "bridge"
                        } else {
                            "null"
                        }
                    )?;
                }
            }
        }
        NetworkCommand::Inspect { networks } => {
            let mut inspected = vec![];
            for selector in networks {
                let network = store.inspect_network(&selector)?;
                let containers: serde_json::Map<String, serde_json::Value> = store
                    .network_instances(&network.id)?
                    .into_iter()
                    .map(|instance| {
                        let address = instance
                            .config
                            .peers
                            .as_ref()
                            .map(|peer| format!("{}/24", peer.address))
                            .unwrap_or_default();
                        let mac = instance
                            .config
                            .peers
                            .as_ref()
                            .map(|peer| {
                                pvisor_overlaynet::vm::guest_mac(peer.address)
                                    .map(|b| format!("{b:02x}"))
                                    .join(":")
                            })
                            .unwrap_or_default();
                        (
                            instance.id.clone(),
                            serde_json::json!({"Name":instance.name,"EndpointID":instance.id,
                            "MacAddress":mac,"IPv4Address":address,"IPv6Address":""}),
                        )
                    })
                    .collect();
                let ipam = if let Some(subnet) = &network.subnet {
                    vec![serde_json::json!({"Subnet":subnet,"Gateway":network.gateway})]
                } else {
                    vec![]
                };
                inspected.push(serde_json::json!({"Name":network.name,"Id":network.id,"Created":network.created_ms,
                    "Scope":"local","Driver":if network.subnet.is_some() {"bridge"} else {"null"},
                    "EnableIPv4":network.subnet.is_some(),"EnableIPv6":false,"Internal":false,
                    "IPAM":{"Driver":"default","Config":ipam},"Containers":containers,"Options":{},"Labels":{}}));
            }
            writeln!(output, "{}", serde_json::to_string_pretty(&inspected)?)?;
        }
        NetworkCommand::Rm { force, networks } => {
            return batch(&networks, |selector| {
                store.remove_network(selector, force)?;
                writeln!(output, "{selector}")?;
                Ok(())
            });
        }
    }
    Ok(0)
}

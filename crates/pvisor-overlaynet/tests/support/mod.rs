use pvisor_overlaynet::api::{VmNetwork, VmNetworkConfig, VmNetworkRuntime};

pub fn start(
    config: VmNetworkConfig,
) -> anyhow::Result<(VmNetwork, std::os::unix::net::UnixStream)> {
    let (transport, guest) = std::os::unix::net::UnixStream::pair()?;
    transport.set_nonblocking(true)?;
    let owner =
        VmNetwork::start_with_io(config, move || tokio::net::UnixStream::from_std(transport))?;
    Ok((owner, guest))
}

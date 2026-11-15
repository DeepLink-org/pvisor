//! Windows has no Unix datagram sockets. The private loopback packet adapter
//! retains the same guest Ethernet policy and generation fencing.
pub use crate::peer_protocol::PeerNetworkConfig;
pub(crate) use crate::peer_protocol::reserved_frame;
pub(crate) use crate::peer_udp::{BoundPeer, PeerNetwork, lookup_peer_name};

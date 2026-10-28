//! One canonical identity for anonymous accounting and future visit capture.
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::extract::ConnectInfo;
use http::{HeaderMap, request::Parts};
use ipnet::IpNet;

/// A bare IP with IPv4-mapped IPv6 normalized to IPv4. Debug is redacted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalIp(IpAddr);

impl CanonicalIp {
    pub fn new(ip: IpAddr) -> Self {
        Self(match ip {
            IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
            ip => ip,
        })
    }

    /// For persistence/authorized visit representations, never telemetry.
    pub const fn address(self) -> IpAddr {
        self.0
    }

    pub fn host_network(self) -> IpNet {
        self.0.into()
    }
}

impl fmt::Debug for CanonicalIp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CanonicalIp([REDACTED])")
    }
}

#[derive(Clone)]
pub struct ClientIpResolver {
    trusted: Arc<[IpNet]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingTransportPeer;

impl ClientIpResolver {
    pub fn new(trusted: &[IpNet]) -> Self {
        Self {
            trusted: trusted.into(),
        }
    }

    /// Only transport-owned ConnectInfo has authority. Missing context is an
    /// integration failure, never an invented loopback/unspecified identity.
    pub fn resolve_request(&self, parts: &Parts) -> Result<CanonicalIp, MissingTransportPeer> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .ok_or(MissingTransportPeer)?;
        Ok(self.resolve(peer.0.ip(), &parts.headers))
    }

    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> CanonicalIp {
        let peer = CanonicalIp::new(peer);
        if !self.is_trusted(peer) {
            return resolved(peer, "peer_only");
        }
        match self.forwarded(headers) {
            Some(chain) => {
                // The peer is already known trusted. Validate ALL supplied hops
                // before this walk; a malformed left prefix cannot be ignored.
                let selected = chain
                    .iter()
                    .rev()
                    .copied()
                    .find(|ip| !self.is_trusted(*ip))
                    .unwrap_or(chain[0]);
                resolved(selected, "forwarded")
            }
            None => resolved(peer, "fallback"),
        }
    }

    fn is_trusted(&self, ip: CanonicalIp) -> bool {
        self.trusted.iter().any(|net| match (net, ip.0) {
            (IpNet::V4(net), IpAddr::V4(ip)) => net.contains(&ip),
            // Compare the mapped representation against the ORIGINAL IPv6
            // prefix. /120 is exactly the corresponding IPv4 /24; even prefixes
            // shorter than /96 keep their original set, without prefix rewriting.
            (IpNet::V6(net), IpAddr::V4(ip)) => net.contains(&ip.to_ipv6_mapped()),
            (IpNet::V6(net), IpAddr::V6(ip)) => net.contains(&ip),
            _ => false,
        })
    }

    fn forwarded(&self, headers: &HeaderMap) -> Option<Vec<CanonicalIp>> {
        let mut fields = headers.get_all("x-forwarded-for").iter();
        let field = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        let text = std::str::from_utf8(field.as_bytes()).ok()?;
        // Transport bounds header bytes. Allocation never exceeds 32 elements;
        // no additional byte limit rejects otherwise valid whitespace padding.
        let mut chain = Vec::with_capacity(32);
        for element in text.split(',') {
            if chain.len() == 32 {
                return None;
            }
            chain.push(CanonicalIp::new(element.trim().parse().ok()?));
        }
        Some(chain)
    }
}

fn resolved(ip: CanonicalIp, outcome: &'static str) -> CanonicalIp {
    tracing::info!(
        operation = "client_ip.resolve",
        outcome,
        "client IP resolved"
    );
    ip
}

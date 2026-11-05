#![allow(unused_imports)]
pub mod packet;
pub mod provider;

#[cfg(unix)]
pub mod unix;

#[cfg(unix)]
pub mod unix_raw;

#[cfg(target_os = "windows")]
pub mod windows;

use std::sync::atomic::{AtomicU16, Ordering};

pub use packet::{IcmpEchoReply, IcmpEchoRequest};
pub use provider::IcmpProvider;

#[cfg(unix)]
pub type DefaultIcmpProvider = unix::UnixDgramIcmp;

#[cfg(unix)]
pub type TracerouteIcmpProvider = unix_raw::RawIcmpProvider;

#[cfg(target_os = "windows")]
pub type DefaultIcmpProvider = windows::IcmpApiProvider;

#[cfg(target_os = "windows")]
pub type TracerouteIcmpProvider = windows::IcmpApiProvider;

/// Whether this platform's ICMP providers handle IPv6 targets. Windows uses the ICMPv6 Helper
/// API; Unix ICMPv6 sockets are not written yet, so there a v6 target is `unsupported`.
pub const ICMPV6_SUPPORTED: bool = cfg!(target_os = "windows");

/// The error a provider without ICMPv6 returns for an IPv6 target, instead of a socket error.
#[cfg(unix)]
pub(crate) fn require_ipv4(target: &std::net::SocketAddr) -> Result<(), crate::models::ProbeError> {
    if target.is_ipv4() {
        return Ok(());
    }
    tracing::error!(
        "ICMP to IPv6 target {} is not supported on this platform",
        target.ip()
    );
    Err(crate::models::ProbeError::Unsupported(
        "IPv6 ICMP is not yet supported on this platform".to_string(),
    ))
}

static NEXT_IDENTIFIER: AtomicU16 = AtomicU16::new(0);

/// A fresh ICMP echo identifier for one probe. Raw sockets on Unix see every ICMP reply on
/// the host and filter by identifier, so two probes sharing one would read each other's
/// replies. A process-wide counter never repeats within 65 536 calls; starting from the
/// process id keeps two intqual processes (TUI and headless) apart as well.
pub fn next_identifier() -> u16 {
    (std::process::id() as u16).wrapping_add(NEXT_IDENTIFIER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn next_identifier_never_repeats_within_a_process() {
        let ids: HashSet<u16> = (0..10_000).map(|_| next_identifier()).collect();
        assert_eq!(ids.len(), 10_000);
    }

    #[test]
    fn next_identifier_is_distinct_across_threads() {
        let handles: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..500).map(|_| next_identifier()).collect::<Vec<_>>()))
            .collect();
        let mut ids = HashSet::new();
        for h in handles {
            for id in h.join().unwrap() {
                assert!(ids.insert(id), "identifier {id} handed out twice");
            }
        }
    }
}

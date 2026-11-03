pub mod bandwidth;
pub mod icmp;

use std::net::SocketAddr;

/// Picks the address to probe from a DNS answer. Every ICMP provider is IPv4-only for now,
/// and dual-stack hosts (e.g. google.com) often list an IPv6 address first, so IPv4 wins;
/// an IPv6-only host still resolves and fails later with a clear error.
pub fn pick_address(addrs: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let mut first = None;
    for addr in addrs {
        if addr.is_ipv4() {
            return Some(addr);
        }
        first.get_or_insert(addr);
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_address_prefers_ipv4() {
        let v6: SocketAddr = "[2001:db8::1]:443".parse().unwrap();
        let v4: SocketAddr = "192.0.2.1:443".parse().unwrap();
        assert_eq!(pick_address([v6, v4]), Some(v4));
        assert_eq!(pick_address([v6]), Some(v6));
        assert_eq!(pick_address([]), None);
    }
}

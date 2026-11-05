//! Target validation and address selection, shared by the TUI, headless mode and the MCP
//! server so every entry point accepts and rejects the same targets with the same message.
//!
//! `parse_target` runs before any DNS lookup: a URL, a `host:port` pair or a string with shell
//! characters would otherwise reach `getaddrinfo` and come back as a misleading
//! `dns_resolution` error. After DNS, `select_address` picks one address for the whole run, so
//! ICMP, TCP and traceroute always measure the same host.

use crate::models::ProbeError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

/// RFC 1035 §2.3.4: 255 octets on the wire, which is 253 characters in text form.
const MAX_HOSTNAME_LEN: usize = 253;
/// RFC 1035 §2.3.4.
const MAX_LABEL_LEN: usize = 63;

/// A validated target. An IP literal skips DNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `scope_id` is the IPv6 zone (`fe80::1%12` → 12), 0 when there is none or for IPv4.
    Ip { addr: IpAddr, scope_id: u32 },
    /// A syntactically valid hostname (RFC 1123), trimmed. One trailing dot is kept.
    Host(String),
}

impl Target {
    pub fn ip(addr: IpAddr) -> Self {
        Self::Ip { addr, scope_id: 0 }
    }
}

/// The normalised form, used as `target` in reports: no surrounding whitespace or brackets.
impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip { addr, scope_id: 0 } => write!(f, "{addr}"),
            Self::Ip { addr, scope_id } => write!(f, "{addr}%{scope_id}"),
            Self::Host(name) => f.write_str(name),
        }
    }
}

/// Which address family to measure over. `-4` / `-6` on the CLI, `ip_version` in MCP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum IpVersion {
    /// The first address in the OS's order (RFC 6724), like `ping` and browsers.
    #[default]
    Auto,
    V4,
    V6,
}

/// The family of the address a run actually measured, `ip_version` in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum IpFamily {
    V4,
    V6,
}

impl IpFamily {
    pub fn of(addr: &IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Self::V4,
            IpAddr::V6(_) => Self::V6,
        }
    }
}

fn invalid(msg: impl Into<String>) -> ProbeError {
    ProbeError::InvalidTarget(msg.into())
}

/// Validates and normalises a target: trims it, accepts IPv4/IPv6 literals (bracketed or not,
/// with a numeric IPv6 zone id) and RFC 1123 hostnames, and rejects everything else with a
/// message that names the problem. Pure: no DNS, no I/O.
pub fn parse_target(input: &str) -> Result<Target, ProbeError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(invalid(
            "target is empty; give a hostname or IP address (for example `google.com`)",
        ));
    }
    if let Some((_, rest)) = s.split_once("://") {
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .map(|h| h.rsplit_once('@').map_or(h, |(_, h)| h))
            .filter(|h| !h.is_empty() && !h.starts_with('['))
            .map(|h| h.split(':').next().unwrap_or(h))
            .filter(|h| !h.is_empty())
            .unwrap_or("google.com");
        return Err(invalid(format!(
            "give a hostname or IP address, not a URL (try `{host}`)"
        )));
    }
    if let Some(inner) = s.strip_prefix('[') {
        return match inner.strip_suffix(']') {
            Some(inner) => parse_ipv6(inner).unwrap_or_else(|| {
                Err(invalid(format!("`[{inner}]` is not a valid IPv6 address")))
            }),
            None if inner.contains("]:") => Err(host_port_error()),
            None => Err(invalid(format!("`{s}` has an unclosed `[`"))),
        };
    }
    if let Ok(v4) = s.parse::<Ipv4Addr>() {
        return Ok(Target::ip(IpAddr::V4(v4)));
    }
    if s.contains(':') {
        if let Some(result) = parse_ipv6(s) {
            return result;
        }
        // One colon followed by digits is `host:port`; anything else is a bad character.
        if let Some((_, port)) = s.split_once(':')
            && !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(host_port_error());
        }
    }
    parse_hostname(s)
}

fn host_port_error() -> ProbeError {
    invalid(
        "give the host alone, not `host:port`; set the port with the port option \
         (`-p` on the CLI, `port` in MCP)",
    )
}

/// `None` when `s` is not an IPv6 literal at all; `Some(Err)` for a literal with a bad zone.
fn parse_ipv6(s: &str) -> Option<Result<Target, ProbeError>> {
    let (addr, zone) = match s.split_once('%') {
        Some((addr, zone)) => (addr, Some(zone)),
        None => (s, None),
    };
    let addr: Ipv6Addr = addr.parse().ok()?;
    let scope_id = match zone {
        None => 0,
        Some(zone) => match zone.parse::<u32>() {
            Ok(id) => id,
            Err(_) => {
                return Some(Err(invalid(format!(
                    "IPv6 zone id `{zone}` must be a numeric interface index \
                     (for example `fe80::1%12`)"
                ))));
            }
        },
    };
    Some(Ok(Target::Ip {
        addr: IpAddr::V6(addr),
        scope_id,
    }))
}

/// RFC 1123 / RFC 1035 hostname syntax: letters, digits and `-` in labels of 1–63
/// characters, at most 253 characters, one optional trailing dot.
fn parse_hostname(s: &str) -> Result<Target, ProbeError> {
    if !s.is_ascii() {
        return Err(invalid(format!(
            "`{s}` is not an ASCII hostname; internationalised names are not supported, \
             use the punycode (`xn--`) form"
        )));
    }
    if let Some(c) = s.chars().find(|c| c.is_ascii_whitespace()) {
        let name = if c == ' ' { "a space" } else { "whitespace" };
        return Err(invalid(format!(
            "`{s}` contains {name}; a hostname has only letters, digits, `-` and `.`"
        )));
    }
    if let Some(c) = s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '.'))
    {
        return Err(invalid(format!(
            "`{s}` contains the character `{c}`; a hostname has only letters, digits, `-` and `.`"
        )));
    }
    let name = s.strip_suffix('.').unwrap_or(s);
    if name.len() > MAX_HOSTNAME_LEN {
        return Err(invalid(format!(
            "hostname is {} characters long; the limit is {MAX_HOSTNAME_LEN}",
            name.len()
        )));
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Err(invalid(format!(
                "`{s}` has an empty label (two dots in a row, or a leading dot)"
            )));
        }
        if label.len() > MAX_LABEL_LEN {
            return Err(invalid(format!(
                "label `{label}` is {} characters long; the limit is {MAX_LABEL_LEN}",
                label.len()
            )));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(invalid(format!(
                "label `{label}` starts or ends with `-`, which a hostname does not allow"
            )));
        }
    }
    Ok(Target::Host(s.to_string()))
}

/// Whether `IpVersion::Auto` may pick an IPv6 address. Only Windows has ICMPv6 so far; on
/// Unix, auto keeps preferring IPv4 so a dual-stack host does not turn into an
/// `unsupported` error for every ICMP sample.
pub const AUTO_ALLOWS_IPV6: bool = crate::network::icmp::ICMPV6_SUPPORTED;

/// The address to measure: the first of the requested family, in the given (OS) order.
/// `auto` takes the first address, or the first IPv4 one when `auto_allows_ipv6` is false
/// (falling back to IPv6 if that is all there is). Pure, for tests.
pub fn pick_address(
    addrs: &[SocketAddr],
    version: IpVersion,
    auto_allows_ipv6: bool,
) -> Option<SocketAddr> {
    match version {
        IpVersion::V4 => addrs.iter().find(|a| a.is_ipv4()).copied(),
        IpVersion::V6 => addrs.iter().find(|a| a.is_ipv6()).copied(),
        IpVersion::Auto if auto_allows_ipv6 => addrs.first().copied(),
        IpVersion::Auto => addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied(),
    }
}

/// `pick_address` with the error each entry point reports: `no_address_for_family` when the
/// family is missing, `dns_resolution` when the answer was empty.
pub fn select_address(
    target: &Target,
    addrs: &[SocketAddr],
    version: IpVersion,
) -> Result<SocketAddr, ProbeError> {
    if let Some(addr) = pick_address(addrs, version, AUTO_ALLOWS_IPV6) {
        return Ok(addr);
    }
    let err = match (target, version) {
        (_, IpVersion::Auto) => {
            ProbeError::DnsResolution("No IP addresses found for host".to_string())
        }
        (Target::Ip { .. }, v) => {
            let (is, wanted) = match v {
                IpVersion::V4 => ("IPv6", "IPv4"),
                _ => ("IPv4", "IPv6"),
            };
            ProbeError::NoAddressForFamily(format!(
                "{target} is an {is} address, but {wanted} was requested"
            ))
        }
        (Target::Host(_), v) => {
            let family = if v == IpVersion::V4 { "IPv4" } else { "IPv6" };
            ProbeError::NoAddressForFamily(format!("{target} has no {family} address"))
        }
    };
    tracing::error!("No usable address for {}: {}", target, err);
    Err(err)
}

/// Resolves a validated target to the one address the whole run uses. IP literals skip DNS.
pub async fn resolve(
    target: &Target,
    port: u16,
    version: IpVersion,
) -> Result<SocketAddr, ProbeError> {
    let addrs: Vec<SocketAddr> = match target {
        Target::Ip {
            addr: IpAddr::V6(v6),
            scope_id,
        } => vec![SocketAddr::V6(SocketAddrV6::new(*v6, port, 0, *scope_id))],
        Target::Ip { addr, .. } => vec![SocketAddr::new(*addr, port)],
        Target::Host(name) => match tokio::net::lookup_host((name.as_str(), port)).await {
            Ok(addrs) => addrs.collect(),
            Err(e) => {
                tracing::error!("DNS resolution for {} failed: {}", name, e);
                return Err(ProbeError::DnsResolution(e.to_string()));
            }
        },
    };
    select_address(target, &addrs, version)
}

/// `parse_target` then `resolve`, for callers that hold the raw string (the TUI engine and
/// the traceroute worker).
pub async fn resolve_str(
    input: &str,
    port: u16,
    version: IpVersion,
) -> Result<SocketAddr, ProbeError> {
    let target = parse_target(input).inspect_err(|e| {
        tracing::warn!("Rejected target {:?}: {}", input, e);
    })?;
    resolve(&target, port, version).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(input: &str) -> Target {
        match parse_target(input) {
            Ok(t) => t,
            Err(e) => panic!("{input:?} should parse, got {e}"),
        }
    }

    /// Asserts `invalid_target` and returns the message.
    fn err(input: &str) -> String {
        match parse_target(input) {
            Err(e @ ProbeError::InvalidTarget(_)) => {
                assert_eq!(e.kind(), "invalid_target");
                e.to_string()
            }
            other => panic!("{input:?} should be invalid_target, got {other:?}"),
        }
    }

    fn label(n: usize) -> String {
        "a".repeat(n)
    }

    #[test]
    fn empty_and_whitespace_only_are_rejected() {
        for input in ["", " ", "\t\n "] {
            assert!(err(input).contains("target is empty"), "{input:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(ok("  google.com  "), Target::Host("google.com".into()));
        assert_eq!(ok("\t1.1.1.1\n").to_string(), "1.1.1.1");
    }

    #[test]
    fn ip_literals_are_accepted() {
        assert_eq!(ok("1.1.1.1"), Target::ip("1.1.1.1".parse().unwrap()));
        assert_eq!(ok("::1"), Target::ip("::1".parse().unwrap()));
        assert_eq!(
            ok("2606:4700::1111"),
            Target::ip("2606:4700::1111".parse().unwrap())
        );
    }

    #[test]
    fn bracketed_ipv6_loses_its_brackets() {
        assert_eq!(ok("[::1]"), Target::ip("::1".parse().unwrap()));
        assert_eq!(ok("[::1]").to_string(), "::1");
        assert!(err("[google.com]").contains("not a valid IPv6 address"));
        assert!(err("[::1").contains("unclosed"));
    }

    #[test]
    fn ipv6_zone_id_is_kept_as_scope_id() {
        let t = ok("fe80::1%12");
        assert_eq!(
            t,
            Target::Ip {
                addr: "fe80::1".parse().unwrap(),
                scope_id: 12
            }
        );
        assert_eq!(t.to_string(), "fe80::1%12");
        assert_eq!(ok("[fe80::1%3]").to_string(), "fe80::1%3");
        assert!(err("fe80::1%eth0").contains("numeric interface index"));
    }

    #[test]
    fn url_is_rejected_with_the_host_as_hint() {
        let msg = err("https://google.com");
        assert!(msg.contains("not a URL"), "{msg}");
        assert!(msg.contains("(try `google.com`)"), "{msg}");
        assert!(err("http://user@example.com:8080/x?y").contains("(try `example.com`)"));
        assert!(err("ftp://").contains("(try `google.com`)"));
    }

    #[test]
    fn host_port_is_rejected_with_a_port_hint() {
        for input in ["google.com:443", "1.1.1.1:80", "[::1]:443"] {
            let msg = err(input);
            assert!(msg.contains("not `host:port`"), "{input}: {msg}");
            assert!(msg.contains("port option"), "{input}: {msg}");
        }
    }

    #[test]
    fn shell_characters_and_inner_spaces_are_rejected() {
        let msg = err("google.com; whoami");
        assert!(msg.contains("contains a space"), "{msg}");
        let msg = err("google.com;whoami");
        assert!(msg.contains("the character `;`"), "{msg}");
        for input in ["a|b", "$(id)", "a&b", "a`b", "a_b", "a/b", "a:b"] {
            assert!(err(input).contains("the character"), "{input}");
        }
        assert!(err("goo gle.com").contains("contains a space"));
        assert!(err("goo\tgle.com").contains("whitespace"));
    }

    #[test]
    fn label_length_63_passes_and_64_fails() {
        assert_eq!(
            ok(&format!("{}.com", label(63))),
            Target::Host(format!("{}.com", label(63)))
        );
        let msg = err(&format!("{}.com", label(64)));
        assert!(msg.contains("64 characters long; the limit is 63"), "{msg}");
    }

    #[test]
    fn total_length_253_passes_and_254_fails() {
        // 3 × 63 + 61 + 3 dots = 253; one more letter = 254.
        let n253 = format!("{0}.{0}.{0}.{1}", label(63), label(61));
        let n254 = format!("{0}.{0}.{0}.{1}", label(63), label(62));
        assert_eq!((n253.len(), n254.len()), (253, 254));
        ok(&n253);
        ok(&format!("{n253}.")); // the trailing dot does not count
        let msg = err(&n254);
        assert!(
            msg.contains("254 characters long; the limit is 253"),
            "{msg}"
        );
        assert!(err(&"a".repeat(300)).contains("characters long"));
    }

    #[test]
    fn leading_or_trailing_hyphen_in_a_label_fails() {
        for input in ["-google.com", "google-.com", "www.-x.com"] {
            assert!(err(input).contains("starts or ends with `-`"), "{input}");
        }
        ok("my-host.example");
    }

    #[test]
    fn one_trailing_dot_passes_two_dots_fail() {
        assert_eq!(ok("google.com."), Target::Host("google.com.".into()));
        assert!(err("google.com..").contains("empty label"));
        assert!(err("google..com").contains("empty label"));
        assert!(err(".google.com").contains("empty label"));
        assert!(err(".").contains("empty label"));
    }

    /// Not IP literals, but valid hostname syntax: they go to DNS, which decides (and fails
    /// with `dns_resolution` for `999.1.1.1`). Pinned so a refactor changes it on purpose.
    #[test]
    fn numeric_looking_names_are_hostnames() {
        assert_eq!(ok("999.1.1.1"), Target::Host("999.1.1.1".into()));
        assert_eq!(ok("1.2.3"), Target::Host("1.2.3".into()));
        assert!(err("fe80::1%").contains("numeric interface index"));
    }

    #[test]
    fn non_ascii_name_gets_a_punycode_hint() {
        let msg = err("bücher.de");
        assert!(msg.contains("punycode (`xn--`)"), "{msg}");
        ok("xn--bcher-kva.de");
    }

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn pick_address_auto_takes_the_first_in_os_order() {
        let (v6, v4) = (sa("[2001:db8::1]:443"), sa("192.0.2.1:443"));
        assert_eq!(pick_address(&[v6, v4], IpVersion::Auto, true), Some(v6));
        assert_eq!(pick_address(&[v4, v6], IpVersion::Auto, true), Some(v4));
        assert_eq!(pick_address(&[], IpVersion::Auto, true), None);
    }

    #[test]
    fn pick_address_auto_prefers_ipv4_where_icmpv6_is_missing() {
        let (v6, v4) = (sa("[2001:db8::1]:443"), sa("192.0.2.1:443"));
        assert_eq!(pick_address(&[v6, v4], IpVersion::Auto, false), Some(v4));
        assert_eq!(pick_address(&[v6], IpVersion::Auto, false), Some(v6));
    }

    #[test]
    fn pick_address_v4_and_v6_take_the_first_of_their_family() {
        let (v6a, v4a, v6b, v4b) = (
            sa("[2001:db8::1]:443"),
            sa("192.0.2.1:443"),
            sa("[2001:db8::2]:443"),
            sa("192.0.2.2:443"),
        );
        let all = [v6a, v4a, v6b, v4b];
        for allow in [true, false] {
            assert_eq!(pick_address(&all, IpVersion::V4, allow), Some(v4a));
            assert_eq!(pick_address(&all, IpVersion::V6, allow), Some(v6a));
        }
        assert_eq!(pick_address(&[v4a], IpVersion::V6, true), None);
        assert_eq!(pick_address(&[v6a], IpVersion::V4, true), None);
    }

    #[test]
    fn select_address_names_the_missing_family() {
        let host = Target::Host("google.com".into());
        match select_address(&host, &[sa("192.0.2.1:443")], IpVersion::V6) {
            Err(e @ ProbeError::NoAddressForFamily(_)) => {
                assert_eq!(e.kind(), "no_address_for_family");
                assert_eq!(e.to_string(), "google.com has no IPv6 address");
            }
            other => panic!("unexpected {other:?}"),
        }
        let only_v6 = [sa("[2001:db8::1]:443")];
        assert!(matches!(
            select_address(&host, &only_v6, IpVersion::V4),
            Err(ProbeError::NoAddressForFamily(m)) if m == "google.com has no IPv4 address"
        ));
        assert!(matches!(
            select_address(&host, &[], IpVersion::Auto),
            Err(ProbeError::DnsResolution(_))
        ));
    }

    #[tokio::test]
    async fn ip_literal_of_the_other_family_is_no_address_for_family() {
        let v4 = parse_target("127.0.0.1").unwrap();
        match resolve(&v4, 443, IpVersion::V6).await {
            Err(ProbeError::NoAddressForFamily(m)) => {
                assert_eq!(m, "127.0.0.1 is an IPv4 address, but IPv6 was requested");
            }
            other => panic!("unexpected {other:?}"),
        }
        let v6 = parse_target("::1").unwrap();
        assert!(matches!(
            resolve(&v6, 443, IpVersion::V4).await,
            Err(ProbeError::NoAddressForFamily(m)) if m == "::1 is an IPv6 address, but IPv4 was requested"
        ));
    }

    #[tokio::test]
    async fn ip_literals_resolve_without_dns_and_keep_the_zone() {
        let t = parse_target("fe80::1%7").unwrap();
        match resolve(&t, 443, IpVersion::Auto).await.unwrap() {
            SocketAddr::V6(a) => {
                assert_eq!(a.scope_id(), 7);
                assert_eq!(a.port(), 443);
            }
            other => panic!("unexpected {other}"),
        }
        let t = parse_target("127.0.0.1").unwrap();
        assert_eq!(
            resolve(&t, 9, IpVersion::V4).await.unwrap(),
            sa("127.0.0.1:9")
        );
    }

    #[tokio::test]
    async fn resolve_str_rejects_before_dns() {
        assert!(matches!(
            resolve_str("https://x.com", 443, IpVersion::Auto).await,
            Err(ProbeError::InvalidTarget(_))
        ));
    }

    #[test]
    fn ip_version_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&IpVersion::V6).unwrap(), "\"v6\"");
        assert_eq!(
            serde_json::from_str::<IpVersion>("\"auto\"").unwrap(),
            IpVersion::Auto
        );
        assert_eq!(serde_json::to_string(&IpFamily::V4).unwrap(), "\"v4\"");
    }
}

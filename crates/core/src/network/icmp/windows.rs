#![cfg(target_os = "windows")]

//! Windows ICMP provider built on the ICMP Helper API (`IcmpSendEcho` / `Icmp6SendEcho2` in
//! `iphlpapi.dll`), the same API `ping.exe` and `tracert.exe` use. Unlike a raw socket it
//! needs no admin rights and is not blocked by Windows Firewall, and the OS matches replies to
//! requests. One provider type serves both families: the target's address picks the API.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ICMP_ECHO_REPLY, ICMPV6_ECHO_REPLY_LH, IP_DEST_ADDR_UNREACHABLE, IP_DEST_HOST_UNREACHABLE,
    IP_DEST_NET_UNREACHABLE, IP_DEST_NO_ROUTE, IP_DEST_PORT_UNREACHABLE, IP_DEST_PROHIBITED,
    IP_DEST_PROT_UNREACHABLE, IP_DEST_SCOPE_MISMATCH, IP_DEST_UNREACHABLE, IP_HOP_LIMIT_EXCEEDED,
    IP_OPTION_INFORMATION, IP_PACKET_TOO_BIG, IP_PARAMETER_PROBLEM, IP_REQ_TIMED_OUT, IP_SUCCESS,
    IP_TIME_EXCEEDED, IP_TTL_EXPIRED_REASSEM, IP_TTL_EXPIRED_TRANSIT, Icmp6CreateFile,
    Icmp6ParseReplies, Icmp6SendEcho2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_INET6, IN6_ADDR, IN6_ADDR_0, SOCKADDR_IN6, SOCKADDR_IN6_0,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::packet::{IcmpDestUnreachable, IcmpEchoReply, IcmpResponse, IcmpTimeExceeded};
use super::provider::{IcmpProvider, TracerouteHopResult};

use crate::models::ProbeError;

/// Echo payload, the same 32 bytes `ping.exe` sends.
const PAYLOAD: [u8; 32] = *b"abcdefghijklmnopqrstuvwabcdefghi";

/// Default TTL / hop limit for a plain ping, the same as `ping.exe`.
const DEFAULT_TTL: u8 = 128;

/// ICMP error message types per family (RFC 792 / RFC 4443), so `icmp_error` reports the
/// type the wire actually carried.
const V4_UNREACHABLE: u8 = 3;
const V4_TIME_EXCEEDED: u8 = 11;
const V6_UNREACHABLE: u8 = 1;
const V6_TIME_EXCEEDED: u8 = 3;

/// Reply buffer: one `ICMP_ECHO_REPLY` followed by room for the echoed payload and the
/// 8 bytes of an ICMP error message, as the `IcmpSendEcho` docs require. A struct keeps
/// the reply correctly aligned without manual pointer arithmetic.
#[repr(C)]
struct ReplyBuffer {
    reply: ICMP_ECHO_REPLY,
    data: [u8; PAYLOAD.len() + 8],
}

/// `Icmp6SendEcho2` docs: room for one `ICMPV6_ECHO_REPLY`, the request data, 8 bytes of an
/// ICMP error message and an `IO_STATUS_BLOCK`.
const REPLY6_LEN: usize = std::mem::size_of::<ICMPV6_ECHO_REPLY_LH>()
    + PAYLOAD.len()
    + 8
    + std::mem::size_of::<IO_STATUS_BLOCK>();

/// Aligned for the reply struct at its start.
#[repr(C, align(8))]
struct ReplyBuffer6([u8; REPLY6_LEN]);

/// Closes the ICMP handle on every return path. `IcmpCloseHandle` closes handles from both
/// `IcmpCreateFile` and `Icmp6CreateFile`.
struct IcmpHandle(HANDLE);

impl IcmpHandle {
    fn open(v6: bool) -> Result<Self, ProbeError> {
        let (handle, api) = if v6 {
            // SAFETY: Icmp6CreateFile takes no arguments and returns a handle or
            // INVALID_HANDLE_VALUE.
            (unsafe { Icmp6CreateFile() }, "Icmp6CreateFile")
        } else {
            // SAFETY: IcmpCreateFile takes no arguments and returns a handle or
            // INVALID_HANDLE_VALUE.
            (unsafe { IcmpCreateFile() }, "IcmpCreateFile")
        };
        if handle == INVALID_HANDLE_VALUE {
            let e = std::io::Error::last_os_error();
            tracing::error!("{} failed: {}", api, e);
            return Err(ProbeError::Socket(e));
        }
        Ok(Self(handle))
    }
}

impl Drop for IcmpHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid handle from IcmpCreateFile/Icmp6CreateFile and is closed
        // exactly once here.
        unsafe { IcmpCloseHandle(self.0) };
    }
}

/// Result of one blocking echo call.
struct EchoOutcome {
    status: u32,
    /// `None` when the API reported no responder (timeouts, local errors).
    responder: Option<IpAddr>,
    rtt_ms: f64,
}

/// For IPv6 the same `Ttl` field is the hop limit.
fn options(ttl: Option<u8>) -> IP_OPTION_INFORMATION {
    IP_OPTION_INFORMATION {
        Ttl: ttl.unwrap_or(DEFAULT_TTL),
        Tos: 0,
        Flags: 0,
        OptionsSize: 0,
        OptionsData: std::ptr::null_mut(),
    }
}

fn timeout_ms(timeout: Duration) -> u32 {
    u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX)
}

/// Sends one IPv4 echo request and blocks until a reply, an ICMP error or the timeout.
fn send_echo_v4(
    target: Ipv4Addr,
    ttl: Option<u8>,
    timeout: Duration,
) -> Result<EchoOutcome, ProbeError> {
    let handle = IcmpHandle::open(false)?;
    let options = options(ttl);
    // SAFETY: ReplyBuffer is plain old data (integers and raw pointers), so all-zero is valid.
    let mut buffer: ReplyBuffer = unsafe { std::mem::zeroed() };

    let start = Instant::now();
    // SAFETY: the handle is open; the destination is an IPv4 address in network byte order;
    // the payload, options and reply buffer outlive this blocking call, and the sizes passed
    // match their allocations.
    let replies = unsafe {
        IcmpSendEcho(
            handle.0,
            u32::from_ne_bytes(target.octets()),
            PAYLOAD.as_ptr().cast(),
            PAYLOAD.len() as u16,
            &options,
            (&mut buffer as *mut ReplyBuffer).cast(),
            std::mem::size_of::<ReplyBuffer>() as u32,
            timeout_ms(timeout),
        )
    };
    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;

    let status = if replies == 0 {
        // SAFETY: GetLastError only reads this thread's last-error value.
        unsafe { GetLastError() }
    } else {
        buffer.reply.Status
    };
    let responder = Ipv4Addr::from(buffer.reply.Address.to_ne_bytes());

    Ok(EchoOutcome {
        status,
        responder: (!responder.is_unspecified()).then_some(IpAddr::V4(responder)),
        rtt_ms,
    })
}

fn sockaddr_in6(addr: &SocketAddrV6) -> SOCKADDR_IN6 {
    SOCKADDR_IN6 {
        sin6_family: AF_INET6,
        sin6_port: 0,
        sin6_flowinfo: 0,
        sin6_addr: IN6_ADDR {
            u: IN6_ADDR_0 {
                Byte: addr.ip().octets(),
            },
        },
        Anonymous: SOCKADDR_IN6_0 {
            sin6_scope_id: addr.scope_id(),
        },
    }
}

/// `IPV6_ADDRESS_EX::sin6_addr` holds the address as eight `u16` in network byte order.
fn ipv6_from_words(words: [u16; 8]) -> Ipv6Addr {
    let mut bytes = [0u8; 16];
    for (chunk, word) in bytes.chunks_exact_mut(2).zip(words) {
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    Ipv6Addr::from(bytes)
}

/// Sends one ICMPv6 echo request (synchronous `Icmp6SendEcho2`: no event, no APC) and blocks
/// until a reply, an ICMPv6 error or the timeout. The source is `::`, so the OS picks it.
fn send_echo_v6(
    target: SocketAddrV6,
    ttl: Option<u8>,
    timeout: Duration,
) -> Result<EchoOutcome, ProbeError> {
    let handle = IcmpHandle::open(true)?;
    let options = options(ttl);
    let source = sockaddr_in6(&SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0));
    let destination = sockaddr_in6(&target);
    let mut buffer = ReplyBuffer6([0; REPLY6_LEN]);

    let start = Instant::now();
    // SAFETY: the handle is open; with a null event and no APC routine the call is
    // synchronous, so the source, destination, payload, options and reply buffer all outlive
    // it; the sizes passed match their allocations, and the buffer is at least as large as
    // the docs require (see REPLY6_LEN).
    let replies = unsafe {
        Icmp6SendEcho2(
            handle.0,
            std::ptr::null_mut(),
            None,
            std::ptr::null(),
            &source,
            &destination,
            PAYLOAD.as_ptr().cast(),
            PAYLOAD.len() as u16,
            &options,
            buffer.0.as_mut_ptr().cast(),
            REPLY6_LEN as u32,
            timeout_ms(timeout),
        )
    };
    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;

    let parsed = if replies == 0 {
        0
    } else {
        // SAFETY: the buffer was just filled by Icmp6SendEcho2 and REPLY6_LEN is its size.
        unsafe { Icmp6ParseReplies(buffer.0.as_mut_ptr().cast(), REPLY6_LEN as u32) }
    };
    if parsed == 0 {
        // SAFETY: GetLastError only reads this thread's last-error value.
        let status = unsafe { GetLastError() };
        return Ok(EchoOutcome {
            status,
            responder: None,
            rtt_ms,
        });
    }
    // SAFETY: Icmp6ParseReplies reported a reply, which the API stores as an
    // ICMPV6_ECHO_REPLY_LH at the start of the buffer; the buffer is larger than the struct,
    // the struct is plain data, and read_unaligned makes no alignment assumption.
    let reply: ICMPV6_ECHO_REPLY_LH = unsafe { std::ptr::read_unaligned(buffer.0.as_ptr().cast()) };
    // Copy out of the packed struct by value; a reference into it could be unaligned.
    let address = reply.Address;
    let responder = ipv6_from_words(address.sin6_addr);

    Ok(EchoOutcome {
        status: reply.Status,
        responder: (!responder.is_unspecified()).then_some(IpAddr::V6(responder)),
        rtt_ms,
    })
}

fn send_echo(
    target: SocketAddr,
    ttl: Option<u8>,
    timeout: Duration,
) -> Result<EchoOutcome, ProbeError> {
    match target {
        SocketAddr::V4(a) => send_echo_v4(*a.ip(), ttl, timeout),
        SocketAddr::V6(a) => send_echo_v6(a, ttl, timeout),
    }
}

fn unreachable_response(code: u8, identifier: u16, seq: u16) -> IcmpResponse {
    IcmpResponse::DestinationUnreachable(IcmpDestUnreachable {
        code,
        original_identifier: identifier,
        original_sequence: seq,
    })
}

fn time_exceeded_response(code: u8, identifier: u16, seq: u16) -> IcmpResponse {
    IcmpResponse::TimeExceeded(IcmpTimeExceeded {
        code,
        original_identifier: identifier,
        original_sequence: seq,
    })
}

fn echo_reply(identifier: u16, seq: u16) -> IcmpResponse {
    IcmpResponse::EchoReply(IcmpEchoReply {
        type_: 0,
        code: 0,
        identifier,
        sequence_number: seq,
    })
}

fn unknown_status(api: &str, status: u32) -> ProbeError {
    tracing::error!("{} failed with status {}", api, status);
    ProbeError::Socket(std::io::Error::other(format!(
        "{api} failed with status {status}"
    )))
}

/// Maps an `IcmpSendEcho` status code to the shared ICMP response model (ICMPv4 codes).
/// The API matches replies itself, so the request's own identifier and sequence are
/// echoed back to keep the cross-platform types filled in.
fn map_status(status: u32, identifier: u16, seq: u16) -> Result<IcmpResponse, ProbeError> {
    let unreachable = |code| Ok(unreachable_response(code, identifier, seq));
    let time_exceeded = |code| Ok(time_exceeded_response(code, identifier, seq));
    match status {
        IP_SUCCESS => Ok(echo_reply(identifier, seq)),
        IP_REQ_TIMED_OUT => Err(ProbeError::IcmpTimeout),
        IP_TTL_EXPIRED_TRANSIT => time_exceeded(0),
        IP_TTL_EXPIRED_REASSEM => time_exceeded(1),
        IP_DEST_NET_UNREACHABLE => unreachable(0),
        IP_DEST_HOST_UNREACHABLE => unreachable(1),
        IP_DEST_PROT_UNREACHABLE => unreachable(2),
        IP_DEST_PORT_UNREACHABLE => unreachable(3),
        other => Err(unknown_status("IcmpSendEcho", other)),
    }
}

/// Maps an `Icmp6SendEcho2` status code to the shared model with ICMPv6 codes (RFC 4443
/// §3.1, §3.3). The status names are shared with IPv4, and several values alias the IPv6
/// meaning (`IP_DEST_NO_ROUTE` = `IP_DEST_NET_UNREACHABLE`, `IP_HOP_LIMIT_EXCEEDED` =
/// `IP_TTL_EXPIRED_TRANSIT`), but the codes differ, so v6 has its own table.
fn map_status_v6(status: u32, identifier: u16, seq: u16) -> Result<IcmpResponse, ProbeError> {
    let unreachable = |code| Ok(unreachable_response(code, identifier, seq));
    let time_exceeded = |code| Ok(time_exceeded_response(code, identifier, seq));
    match status {
        IP_SUCCESS => Ok(echo_reply(identifier, seq)),
        IP_REQ_TIMED_OUT => Err(ProbeError::IcmpTimeout),
        IP_HOP_LIMIT_EXCEEDED | IP_TIME_EXCEEDED => time_exceeded(0),
        IP_TTL_EXPIRED_REASSEM => time_exceeded(1),
        IP_DEST_NO_ROUTE | IP_DEST_UNREACHABLE => unreachable(0),
        IP_DEST_PROHIBITED => unreachable(1),
        IP_DEST_SCOPE_MISMATCH => unreachable(2),
        IP_DEST_ADDR_UNREACHABLE => unreachable(3),
        IP_DEST_PORT_UNREACHABLE => unreachable(4),
        IP_PACKET_TOO_BIG => Ok(IcmpResponse::Unknown { type_: 2, code: 0 }),
        IP_PARAMETER_PROBLEM => Ok(IcmpResponse::Unknown { type_: 4, code: 0 }),
        other => Err(unknown_status("Icmp6SendEcho2", other)),
    }
}

async fn send_echo_blocking(
    target: SocketAddr,
    ttl: Option<u8>,
    timeout: Duration,
) -> Result<EchoOutcome, ProbeError> {
    match tokio::task::spawn_blocking(move || send_echo(target, ttl, timeout)).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("Thread Panic: {}", e);
            Err(ProbeError::ThreadPanic(format!("Thread Panicked: {}", e)))
        }
    }
}

pub struct IcmpApiProvider {
    identifier: u16,
}

impl IcmpApiProvider {
    pub fn new(identifier: u16) -> Self {
        Self { identifier }
    }

    fn map(&self, target: &SocketAddr, status: u32, seq: u16) -> Result<IcmpResponse, ProbeError> {
        if target.is_ipv6() {
            map_status_v6(status, self.identifier, seq)
        } else {
            map_status(status, self.identifier, seq)
        }
    }
}

impl IcmpProvider for IcmpApiProvider {
    async fn ping(
        &self,
        target: &SocketAddr,
        seq: u16,
        timeout: Duration,
    ) -> Result<f64, ProbeError> {
        let outcome = send_echo_blocking(*target, None, timeout).await?;
        let (unreachable, exceeded) = if target.is_ipv6() {
            (V6_UNREACHABLE, V6_TIME_EXCEEDED)
        } else {
            (V4_UNREACHABLE, V4_TIME_EXCEEDED)
        };
        match self.map(target, outcome.status, seq)? {
            IcmpResponse::EchoReply(_) => Ok(outcome.rtt_ms),
            IcmpResponse::TimeExceeded(t) => Err(ProbeError::IcmpError {
                type_: exceeded,
                code: t.code,
            }),
            IcmpResponse::DestinationUnreachable(d) => Err(ProbeError::IcmpError {
                type_: unreachable,
                code: d.code,
            }),
            IcmpResponse::Unknown { type_, code } => Err(ProbeError::IcmpError { type_, code }),
        }
    }

    async fn send_with_ttl(
        &self,
        target: &SocketAddr,
        seq: u16,
        ttl: u32,
        timeout: Duration,
    ) -> Result<TracerouteHopResult, ProbeError> {
        let ttl = u8::try_from(ttl).unwrap_or(u8::MAX);
        let outcome = send_echo_blocking(*target, Some(ttl), timeout).await?;
        let response = self.map(target, outcome.status, seq)?;
        Ok(TracerouteHopResult {
            rtt_ms: outcome.rtt_ms,
            // The API leaves the address zero when it has no responder to report.
            responder_ip: outcome.responder.unwrap_or(target.ip()),
            response,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_status_covers_the_traceroute_statuses() {
        assert!(matches!(
            map_status(IP_SUCCESS, 7, 9),
            Ok(IcmpResponse::EchoReply(IcmpEchoReply {
                identifier: 7,
                sequence_number: 9,
                ..
            }))
        ));
        assert!(matches!(
            map_status(IP_REQ_TIMED_OUT, 7, 9),
            Err(ProbeError::IcmpTimeout)
        ));
        assert!(matches!(
            map_status(IP_TTL_EXPIRED_TRANSIT, 7, 9),
            Ok(IcmpResponse::TimeExceeded(IcmpTimeExceeded { code: 0, .. }))
        ));
        assert!(matches!(
            map_status(IP_TTL_EXPIRED_REASSEM, 7, 9),
            Ok(IcmpResponse::TimeExceeded(IcmpTimeExceeded { code: 1, .. }))
        ));
        for (status, expected) in [
            (IP_DEST_NET_UNREACHABLE, 0),
            (IP_DEST_HOST_UNREACHABLE, 1),
            (IP_DEST_PROT_UNREACHABLE, 2),
            (IP_DEST_PORT_UNREACHABLE, 3),
        ] {
            match map_status(status, 7, 9) {
                Ok(IcmpResponse::DestinationUnreachable(d)) => assert_eq!(d.code, expected),
                other => panic!("status {status}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn map_status_turns_unknown_statuses_into_errors() {
        assert!(matches!(
            map_status(11050, 7, 9),
            Err(ProbeError::Socket(_))
        ));
    }

    /// The response kind and code (or error kind) each `Icmp6SendEcho2` status maps to.
    fn shape(r: Result<IcmpResponse, ProbeError>) -> Result<(&'static str, u8), &'static str> {
        match r {
            Ok(IcmpResponse::EchoReply(_)) => Ok(("echo", 0)),
            Ok(IcmpResponse::TimeExceeded(t)) => Ok(("exceeded", t.code)),
            Ok(IcmpResponse::DestinationUnreachable(d)) => Ok(("unreachable", d.code)),
            Ok(IcmpResponse::Unknown { type_, .. }) => Ok(("unknown", type_)),
            Err(e) => Err(e.kind()),
        }
    }

    #[test]
    fn map_status_v6_table() {
        let table = [
            (IP_SUCCESS, Ok(("echo", 0))),
            (IP_REQ_TIMED_OUT, Err("icmp_timeout")),
            (IP_HOP_LIMIT_EXCEEDED, Ok(("exceeded", 0))),
            (IP_TIME_EXCEEDED, Ok(("exceeded", 0))),
            (IP_TTL_EXPIRED_REASSEM, Ok(("exceeded", 1))),
            (IP_DEST_NO_ROUTE, Ok(("unreachable", 0))),
            (IP_DEST_UNREACHABLE, Ok(("unreachable", 0))),
            (IP_DEST_PROHIBITED, Ok(("unreachable", 1))),
            (IP_DEST_SCOPE_MISMATCH, Ok(("unreachable", 2))),
            (IP_DEST_ADDR_UNREACHABLE, Ok(("unreachable", 3))),
            (IP_DEST_PORT_UNREACHABLE, Ok(("unreachable", 4))),
            (IP_PACKET_TOO_BIG, Ok(("unknown", 2))),
            (IP_PARAMETER_PROBLEM, Ok(("unknown", 4))),
            (11050, Err("socket")),
            (u32::MAX, Err("socket")),
        ];
        for (status, expected) in table {
            assert_eq!(
                shape(map_status_v6(status, 7, 9)),
                expected,
                "status {status}"
            );
        }
        assert!(matches!(
            map_status_v6(IP_SUCCESS, 7, 9),
            Ok(IcmpResponse::EchoReply(IcmpEchoReply {
                identifier: 7,
                sequence_number: 9,
                ..
            }))
        ));
    }

    #[test]
    fn ipv6_words_are_network_order() {
        let addr: Ipv6Addr = "2606:4700::1111".parse().unwrap();
        let o = addr.octets();
        let words: [u16; 8] = std::array::from_fn(|i| u16::from_ne_bytes([o[2 * i], o[2 * i + 1]]));
        assert_eq!(ipv6_from_words(words), addr);
    }

    #[test]
    fn sockaddr_in6_carries_address_and_scope() {
        let target = SocketAddrV6::new("fe80::1".parse().unwrap(), 443, 0, 12);
        let sa = sockaddr_in6(&target);
        assert_eq!(sa.sin6_family, AF_INET6);
        assert_eq!(sa.sin6_port, 0, "ICMP has no port");
        // SAFETY (test): both union fields are plain integers written just above.
        let (bytes, scope) = unsafe { (sa.sin6_addr.u.Byte, sa.Anonymous.sin6_scope_id) };
        assert_eq!(bytes, target.ip().octets());
        assert_eq!(scope, 12);
    }

    #[tokio::test]
    async fn ping_loopback_without_admin() {
        let provider = IcmpApiProvider::new(1);
        let target: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let rtt = provider
            .ping(&target, 1, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(rtt >= 0.0);
    }

    #[tokio::test]
    async fn send_with_ttl_reaches_loopback() {
        let provider = IcmpApiProvider::new(1);
        let target: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let hop = provider
            .send_with_ttl(&target, 1, 1, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(hop.response, IcmpResponse::EchoReply(_)));
        assert_eq!(hop.responder_ip, target.ip());
    }

    /// ICMPv6 through `Icmp6SendEcho2`, no admin.
    #[tokio::test]
    async fn ping_ipv6_loopback_without_admin() {
        let provider = IcmpApiProvider::new(1);
        let target: SocketAddr = "[::1]:0".parse().unwrap();
        let rtt = provider
            .ping(&target, 1, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(rtt >= 0.0);
    }

    #[tokio::test]
    async fn send_with_hop_limit_reaches_ipv6_loopback() {
        let provider = IcmpApiProvider::new(1);
        let target: SocketAddr = "[::1]:0".parse().unwrap();
        let hop = provider
            .send_with_ttl(&target, 1, 1, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(hop.response, IcmpResponse::EchoReply(_)));
        assert_eq!(hop.responder_ip, target.ip());
    }
}

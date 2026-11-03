#![cfg(target_os = "windows")]

//! Windows ICMP provider built on the ICMP Helper API (`IcmpSendEcho` in `iphlpapi.dll`),
//! the same API `ping.exe` and `tracert.exe` use. Unlike a raw socket it needs no admin
//! rights and is not blocked by Windows Firewall, and the OS matches replies to requests.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ICMP_ECHO_REPLY, IP_DEST_HOST_UNREACHABLE, IP_DEST_NET_UNREACHABLE, IP_DEST_PORT_UNREACHABLE,
    IP_DEST_PROT_UNREACHABLE, IP_OPTION_INFORMATION, IP_REQ_TIMED_OUT, IP_SUCCESS,
    IP_TTL_EXPIRED_REASSEM, IP_TTL_EXPIRED_TRANSIT, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho,
};

use super::packet::{IcmpDestUnreachable, IcmpEchoReply, IcmpResponse, IcmpTimeExceeded};
use super::provider::{IcmpProvider, TracerouteHopResult};

use crate::models::ProbeError;

/// Echo payload, the same 32 bytes `ping.exe` sends.
const PAYLOAD: [u8; 32] = *b"abcdefghijklmnopqrstuvwabcdefghi";

/// Reply buffer: one `ICMP_ECHO_REPLY` followed by room for the echoed payload and the
/// 8 bytes of an ICMP error message, as the `IcmpSendEcho` docs require. A struct keeps
/// the reply correctly aligned without manual pointer arithmetic.
#[repr(C)]
struct ReplyBuffer {
    reply: ICMP_ECHO_REPLY,
    data: [u8; PAYLOAD.len() + 8],
}

/// Closes the ICMP handle on every return path.
struct IcmpHandle(HANDLE);

impl IcmpHandle {
    fn open() -> Result<Self, ProbeError> {
        // SAFETY: IcmpCreateFile takes no arguments and returns a handle or INVALID_HANDLE_VALUE.
        let handle = unsafe { IcmpCreateFile() };
        if handle == INVALID_HANDLE_VALUE {
            let e = std::io::Error::last_os_error();
            tracing::error!("IcmpCreateFile failed: {}", e);
            return Err(ProbeError::Socket(e));
        }
        Ok(Self(handle))
    }
}

impl Drop for IcmpHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid handle from IcmpCreateFile and is closed exactly once here.
        unsafe { IcmpCloseHandle(self.0) };
    }
}

/// Result of one blocking `IcmpSendEcho` call.
struct EchoOutcome {
    status: u32,
    responder: Ipv4Addr,
    rtt_ms: f64,
}

/// Sends one echo request and blocks until a reply, an ICMP error or the timeout.
fn send_echo(
    target: Ipv4Addr,
    ttl: Option<u8>,
    timeout: Duration,
) -> Result<EchoOutcome, ProbeError> {
    let handle = IcmpHandle::open()?;

    let options = IP_OPTION_INFORMATION {
        Ttl: ttl.unwrap_or(128),
        Tos: 0,
        Flags: 0,
        OptionsSize: 0,
        OptionsData: std::ptr::null_mut(),
    };
    // SAFETY: ReplyBuffer is plain old data (integers and raw pointers), so all-zero is valid.
    let mut buffer: ReplyBuffer = unsafe { std::mem::zeroed() };
    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

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
            timeout_ms,
        )
    };
    let rtt_ms = start.elapsed().as_secs_f64() * 1000.0;

    let status = if replies == 0 {
        // SAFETY: GetLastError only reads this thread's last-error value.
        unsafe { GetLastError() }
    } else {
        buffer.reply.Status
    };

    Ok(EchoOutcome {
        status,
        responder: Ipv4Addr::from(buffer.reply.Address.to_ne_bytes()),
        rtt_ms,
    })
}

/// Maps an `IcmpSendEcho` status code to the shared ICMP response model.
/// The API matches replies itself, so the request's own identifier and sequence are
/// echoed back to keep the cross-platform types filled in.
fn map_status(status: u32, identifier: u16, seq: u16) -> Result<IcmpResponse, ProbeError> {
    let unreachable = |code| {
        Ok(IcmpResponse::DestinationUnreachable(IcmpDestUnreachable {
            code,
            original_identifier: identifier,
            original_sequence: seq,
        }))
    };
    let time_exceeded = |code| {
        Ok(IcmpResponse::TimeExceeded(IcmpTimeExceeded {
            code,
            original_identifier: identifier,
            original_sequence: seq,
        }))
    };

    match status {
        IP_SUCCESS => Ok(IcmpResponse::EchoReply(IcmpEchoReply {
            type_: 0,
            code: 0,
            identifier,
            sequence_number: seq,
        })),
        IP_REQ_TIMED_OUT => Err(ProbeError::IcmpTimeout),
        IP_TTL_EXPIRED_TRANSIT => time_exceeded(0),
        IP_TTL_EXPIRED_REASSEM => time_exceeded(1),
        IP_DEST_NET_UNREACHABLE => unreachable(0),
        IP_DEST_HOST_UNREACHABLE => unreachable(1),
        IP_DEST_PROT_UNREACHABLE => unreachable(2),
        IP_DEST_PORT_UNREACHABLE => unreachable(3),
        other => Err(ProbeError::Socket(std::io::Error::other(format!(
            "IcmpSendEcho failed with status {other}"
        )))),
    }
}

fn ipv4_target(target: &SocketAddr) -> Result<Ipv4Addr, ProbeError> {
    match target.ip() {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(ip) => Err(ProbeError::Socket(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("IPv6 targets are not supported yet: {ip}"),
        ))),
    }
}

async fn send_echo_blocking(
    target: Ipv4Addr,
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
}

impl IcmpProvider for IcmpApiProvider {
    async fn ping(
        &self,
        target: &SocketAddr,
        seq: u16,
        timeout: Duration,
    ) -> Result<f64, ProbeError> {
        let outcome = send_echo_blocking(ipv4_target(target)?, None, timeout).await?;
        match map_status(outcome.status, self.identifier, seq)? {
            IcmpResponse::EchoReply(_) => Ok(outcome.rtt_ms),
            IcmpResponse::TimeExceeded(t) => Err(ProbeError::IcmpError {
                type_: 11,
                code: t.code,
            }),
            IcmpResponse::DestinationUnreachable(d) => Err(ProbeError::IcmpError {
                type_: 3,
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
        let outcome = send_echo_blocking(ipv4_target(target)?, Some(ttl), timeout).await?;
        let response = map_status(outcome.status, self.identifier, seq)?;
        // The API leaves Address zero when it has no reply to report.
        let responder_ip = if outcome.responder.is_unspecified() {
            target.ip()
        } else {
            IpAddr::V4(outcome.responder)
        };
        Ok(TracerouteHopResult {
            rtt_ms: outcome.rtt_ms,
            responder_ip,
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

    #[tokio::test]
    async fn ipv6_targets_are_rejected() {
        let provider = IcmpApiProvider::new(1);
        let target: SocketAddr = "[::1]:0".parse().unwrap();
        assert!(
            provider
                .ping(&target, 1, Duration::from_secs(1))
                .await
                .is_err()
        );
    }
}

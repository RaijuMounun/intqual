//! `measure` against real sockets on 127.0.0.1 only: no internet, no admin.

use intqual_core::measure::{DEFAULT_MAX_HOPS, MeasureRequest, Source, measure};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn loopback(port: u16, count: u32, timeout: Duration) -> MeasureRequest {
    MeasureRequest {
        target: "127.0.0.1".to_string(),
        port,
        count,
        interval: Duration::from_millis(20),
        timeout,
        icmp: false,
        tcp: true,
        traceroute: false,
        max_hops: DEFAULT_MAX_HOPS,
        source: Source::Network,
    }
}

#[tokio::test]
async fn measure_loopback_tcp_listener_succeeds() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept so the backlog never fills, whatever the OS default.
    tokio::spawn(async move { while listener.accept().await.is_ok() {} });

    let report = measure(
        &loopback(port, 3, Duration::from_secs(2)),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(report.resolved_ip, "127.0.0.1");
    assert!(report.icmp.is_none());
    let tcp = report.tcp.unwrap();
    assert_eq!(tcp.summary.sent, 3);
    assert_eq!(tcp.summary.received, 3, "samples: {:?}", tcp.samples);
    assert_eq!(tcp.summary.loss_pct, 0.0);
}

#[tokio::test]
async fn measure_closed_port_gives_socket_errors_and_full_loss() {
    // Bind then drop to get a port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    // Windows retries a refused loopback SYN for about 2 s before reporting it, so the
    // timeout must be longer than that or the sample would be a `tcp_timeout`.
    let report = measure(
        &loopback(port, 2, Duration::from_secs(5)),
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    let tcp = report.tcp.unwrap();
    assert_eq!(tcp.summary.sent, 2);
    assert_eq!(tcp.summary.received, 0);
    assert_eq!(tcp.summary.loss_pct, 100.0);
    for sample in &tcp.samples {
        let kind = sample.error.as_ref().map(|e| e.kind.as_str());
        assert_eq!(kind, Some("socket"), "sample: {sample:?}");
    }
}

/// The ICMP Helper API answers 127.0.0.1 without admin.
#[cfg(windows)]
#[tokio::test]
async fn measure_loopback_icmp_succeeds_on_windows() {
    let req = MeasureRequest {
        icmp: true,
        tcp: false,
        ..loopback(0, 2, Duration::from_secs(1))
    };
    let report = measure(&req, CancellationToken::new(), None).await.unwrap();
    assert!(report.tcp.is_none());
    let icmp = report.icmp.unwrap();
    assert_eq!(icmp.summary.sent, 2);
    assert!(icmp.summary.received >= 1, "samples: {:?}", icmp.samples);
}

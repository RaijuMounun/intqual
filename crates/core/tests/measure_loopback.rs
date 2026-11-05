//! `measure` against real sockets on 127.0.0.1 only: no internet, no admin.

use intqual_core::measure::{DEFAULT_MAX_HOPS, MeasureRequest, Source, measure};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn loopback(port: u16, count: u32, timeout: Duration) -> MeasureRequest {
    MeasureRequest {
        target: "127.0.0.1".to_string(),
        ip_version: intqual_core::IpVersion::Auto,
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

/// The MCP server runs measures concurrently; each must get only its own samples.
#[tokio::test]
async fn measure_loopback_two_concurrent_measures_do_not_cross_talk() {
    let mut ports = Vec::new();
    for _ in 0..2 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ports.push(listener.local_addr().unwrap().port());
        tokio::spawn(async move { while listener.accept().await.is_ok() {} });
    }
    let request = |port, count| MeasureRequest {
        // ICMP to loopback needs no admin only on Windows (ICMP Helper API).
        icmp: cfg!(windows),
        ..loopback(port, count, Duration::from_secs(2))
    };
    let (a, b) = (request(ports[0], 4), request(ports[1], 6));
    let (ra, rb) = tokio::join!(
        measure(&a, CancellationToken::new(), None),
        measure(&b, CancellationToken::new(), None)
    );
    for (report, count) in [(ra.unwrap(), 4u32), (rb.unwrap(), 6)] {
        let probes = [report.icmp, report.tcp];
        for probe in probes.into_iter().flatten() {
            let seqs: Vec<u32> = probe.samples.iter().map(|s| s.seq).collect();
            assert_eq!(seqs, (1..=count).collect::<Vec<_>>());
            assert_eq!(
                probe.summary.received, count,
                "samples: {:?}",
                probe.samples
            );
        }
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

/// H3: ping and traceroute to `::1` through the ICMPv6 Helper API, no admin. The traceroute
/// ends at hop 1 with `complete: true`.
#[cfg(windows)]
#[tokio::test]
async fn measure_ipv6_loopback_ping_and_traceroute_on_windows() {
    let req = MeasureRequest {
        target: "::1".to_string(),
        ip_version: intqual_core::IpVersion::V6,
        icmp: true,
        tcp: false,
        traceroute: true,
        ..loopback(0, 2, Duration::from_secs(1))
    };
    let report = measure(&req, CancellationToken::new(), None).await.unwrap();
    assert_eq!(report.resolved_ip, "::1");
    assert_eq!(report.ip_version, intqual_core::IpFamily::V6);
    let icmp = report.icmp.unwrap();
    assert!(icmp.summary.received >= 1, "samples: {:?}", icmp.samples);
    let trace = report.traceroute.unwrap();
    assert!(trace.complete, "{trace:?}");
    assert!(trace.error.is_none(), "{trace:?}");
    assert_eq!(trace.hops.len(), 1, "{trace:?}");
    assert_eq!(trace.hops[0].hop_number, 1);
    assert!(trace.hops[0].is_destination);
    assert_eq!(trace.hops[0].ip_address.as_deref(), Some("::1"));
}

/// Bracketed IPv6 and `auto`: the brackets are stripped and no DNS runs.
#[tokio::test]
async fn measure_bracketed_ipv6_loopback_tcp() {
    let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
        Ok(l) => l,
        // A host with IPv6 disabled has no ::1; nothing to test there.
        Err(e) => {
            eprintln!("skipping: no IPv6 loopback ({e})");
            return;
        }
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { while listener.accept().await.is_ok() {} });
    let req = MeasureRequest {
        target: " [::1] ".to_string(),
        ..loopback(port, 2, Duration::from_secs(2))
    };
    let report = measure(&req, CancellationToken::new(), None).await.unwrap();
    assert_eq!(report.target, "::1");
    assert_eq!(report.ip_version, intqual_core::IpFamily::V6);
    assert_eq!(report.tcp.unwrap().summary.received, 2);
}

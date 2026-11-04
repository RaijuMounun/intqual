use super::report::{Sample, Summary, round_ms};

/// Aggregates one probe's samples. Every error counts as loss, including `socket` (a refused
/// port) and `dropped` frames: unlike the TUI's `NetworkStats`, a script asking "did this
/// work?" needs every failed sample to show up in `loss_pct`.
pub fn summarize(samples: &[Sample]) -> Summary {
    let rtts: Vec<f64> = samples.iter().filter_map(|s| s.rtt_ms).collect();
    let sent = samples.len() as u32;
    let received = rtts.len() as u32;
    let loss_pct = if sent == 0 {
        0.0
    } else {
        f64::from(sent - received) / f64::from(sent) * 100.0
    };

    let (min_ms, avg_ms, max_ms) = if rtts.is_empty() {
        (None, None, None)
    } else {
        let min = rtts.iter().copied().fold(f64::INFINITY, f64::min);
        let max = rtts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let avg = rtts.iter().sum::<f64>() / rtts.len() as f64;
        (Some(min), Some(avg), Some(max))
    };

    // Mean absolute difference of consecutive successful RTTs, same as the TUI.
    let jitter_ms = (rtts.len() >= 2).then(|| {
        let diffs: f64 = rtts.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
        diffs / (rtts.len() - 1) as f64
    });

    // Computed on full precision, rounded once at the end.
    Summary {
        sent,
        received,
        loss_pct: round_ms(loss_pct),
        min_ms: min_ms.map(round_ms),
        avg_ms: avg_ms.map(round_ms),
        max_ms: max_ms.map(round_ms),
        jitter_ms: jitter_ms.map(round_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::report::ErrorInfo;
    use crate::models::ProbeError;

    fn ok(seq: u32, rtt: f64) -> Sample {
        Sample {
            seq,
            timestamp: 0,
            rtt_ms: Some(rtt),
            error: None,
        }
    }

    fn failed(seq: u32, err: &ProbeError) -> Sample {
        Sample {
            seq,
            timestamp: 0,
            rtt_ms: None,
            error: Some(ErrorInfo::from(err)),
        }
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn summarize_all_ok_computes_min_avg_max_jitter() {
        let s = summarize(&[ok(1, 10.0), ok(2, 20.0), ok(3, 40.0)]);
        assert_eq!((s.sent, s.received), (3, 3));
        assert_eq!(s.loss_pct, 0.0);
        assert!(close(s.min_ms, 10.0));
        assert_eq!(s.avg_ms, Some(23.333), "rounded to 3 decimals");
        assert!(close(s.max_ms, 40.0));
        assert!(close(s.jitter_ms, 15.0));
    }

    #[test]
    fn summarize_rounds_stats_to_three_decimals() {
        let s = summarize(&[
            ok(1, 1.0),
            ok(2, 2.0),
            ok(3, 2.0),
            failed(4, &ProbeError::IcmpTimeout),
        ]);
        assert_eq!(s.avg_ms, Some(1.667));
        assert_eq!(s.jitter_ms, Some(0.5));
        assert_eq!(s.loss_pct, 25.0);

        let third = summarize(&[ok(1, 1.0), ok(2, 1.0), failed(3, &ProbeError::IcmpTimeout)]);
        assert_eq!(third.loss_pct, 33.333);

        let two_thirds = summarize(&[
            ok(1, 1.0),
            failed(2, &ProbeError::IcmpTimeout),
            failed(3, &ProbeError::IcmpTimeout),
        ]);
        assert_eq!(two_thirds.loss_pct, 66.667, "rounds half up, not truncates");

        // Raw: min 1.0004, max 3.0007, avg 2.000366.., jitter 1.00015.
        let raw = summarize(&[ok(1, 1.0004), ok(2, 2.0), ok(3, 3.0007)]);
        assert_eq!(raw.min_ms, Some(1.0));
        assert_eq!(raw.max_ms, Some(3.001));
        assert_eq!(raw.avg_ms, Some(2.0));
        assert_eq!(raw.jitter_ms, Some(1.0));
    }

    #[test]
    fn summarize_all_lost_gives_100_loss_and_null_stats() {
        let s = summarize(&[
            failed(1, &ProbeError::IcmpTimeout),
            failed(2, &ProbeError::IcmpTimeout),
        ]);
        assert_eq!((s.sent, s.received), (2, 0));
        assert_eq!(s.loss_pct, 100.0);
        assert_eq!(
            (s.min_ms, s.avg_ms, s.max_ms, s.jitter_ms),
            (None, None, None, None)
        );
    }

    #[test]
    fn summarize_mixed_counts_each_error_as_loss() {
        let mut samples: Vec<Sample> = (1..=9).map(|i| ok(i, 10.0)).collect();
        samples.push(failed(10, &ProbeError::IcmpTimeout));
        let s = summarize(&samples);
        assert_eq!((s.sent, s.received), (10, 9));
        assert_eq!(s.loss_pct, 10.0);
    }

    #[test]
    fn summarize_refused_port_socket_error_counts_as_loss() {
        let refused =
            ProbeError::Socket(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let s = summarize(&[ok(1, 5.0), failed(2, &refused)]);
        assert_eq!((s.sent, s.received), (2, 1));
        assert_eq!(s.loss_pct, 50.0);
    }

    #[test]
    fn summarize_dropped_sample_counts_as_loss() {
        let s = summarize(&[ok(1, 5.0), Sample::dropped(2, 0)]);
        assert_eq!((s.sent, s.received), (2, 1));
        assert_eq!(s.loss_pct, 50.0);
    }

    #[test]
    fn summarize_single_success_has_null_jitter() {
        let s = summarize(&[ok(1, 12.5), failed(2, &ProbeError::TcpTimeout)]);
        assert!(close(s.min_ms, 12.5));
        assert!(close(s.avg_ms, 12.5));
        assert!(close(s.max_ms, 12.5));
        assert_eq!(s.jitter_ms, None);
    }

    #[test]
    fn summarize_jitter_skips_failed_samples() {
        let s = summarize(&[
            ok(1, 10.0),
            failed(2, &ProbeError::IcmpTimeout),
            ok(3, 20.0),
        ]);
        assert!(close(s.jitter_ms, 10.0));
    }

    #[test]
    fn summarize_empty_input_does_not_divide_by_zero() {
        let s = summarize(&[]);
        assert_eq!((s.sent, s.received), (0, 0));
        assert_eq!(s.loss_pct, 0.0);
        assert_eq!(
            (s.min_ms, s.avg_ms, s.max_ms, s.jitter_ms),
            (None, None, None, None)
        );
    }
}

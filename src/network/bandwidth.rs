use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;
use rustls_pki_types::ServerName;
use crate::models::{BandwidthProgress, TelemetryEvent, ProbeError};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
pub struct BandwidthEngine;

impl BandwidthEngine {
    pub async fn test_download(target_host: &str, target_path: &str, tx: mpsc::Sender<TelemetryEvent>, cancel_token: tokio_util::sync::CancellationToken) -> Result<(), ProbeError> {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));

        let addr = format!("{}:443", target_host);

        let server_name = ServerName::try_from(target_host)
            .map_err(|e| ProbeError::BandwidthTestFailed(format!("Invalid server name: {}", e)))?
            .to_owned();

        // Solution A: Removed `Connection: close` to prevent the CDN edge node from
        // prematurely closing the TCP stream before the full payload is delivered.
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\n\r\n",
            target_path, target_host
        );

        let mut buffer = [0u8; 8192];
        let total_bytes = Arc::new(AtomicUsize::new(0));
        let bytes_clone = total_bytes.clone();
        let duration_limit = std::time::Duration::from_secs(10);
        let tx_reporter = tx.clone();

        // Start time measured from the beginning of the download test phase.
        let start_time = Instant::now();

        let reporter_token = cancel_token.clone();
        let reporter = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let bytes = bytes_clone.load(Ordering::Relaxed);
                        let elapsed = start_time.elapsed().as_secs_f64();
                        if elapsed > 0.0 {
                            let mut progress = (elapsed / 10.0) * 100.0;
                            if progress > 100.0 { progress = 100.0; }
                            let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                            let event = TelemetryEvent::Bandwidth(BandwidthProgress::Downloading {
                                current_mbps: speed_mbps,
                                progress_pct: progress,
                            });
                            match tx_reporter.try_send(event) {
                                Ok(_) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    tracing::warn!("UI is lagging, dropping telemetry event");
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    tracing::error!("UI channel closed unexpectedly");
                                    break;
                                }
                            }
                        }
                    }
                    _ = reporter_token.cancelled() => {
                        break;
                    }
                }
            }
        });

        // Solution B: Time-bounded download loop with automatic reconnection.
        // The test is strictly time-bounded (10 seconds). If the stream returns EOF or
        // a socket error before duration_limit, we gracefully reconnect and continue
        // accumulating bytes. The loop only terminates on time expiry or cancellation.
        while start_time.elapsed() < duration_limit {
            if cancel_token.is_cancelled() {
                reporter.abort();
                return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
            }

            let stream = match TcpStream::connect(&addr).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Download connection failed: {}, retrying...", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };

            let mut tls_stream = match connector.connect(server_name.clone(), stream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("TLS handshake failed during download: {}, retrying...", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };

            if let Err(e) = tls_stream.write_all(request.as_bytes()).await {
                tracing::warn!("Failed to send download request: {}, retrying...", e);
                continue;
            }

            // Flush the TLS write buffer to ensure the HTTP request is actually transmitted
            // to the server. Without this, rustls may buffer the request internally,
            // causing read() to hang indefinitely waiting for a response that never arrives.
            if let Err(e) = tls_stream.flush().await {
                tracing::warn!("Failed to flush download request: {}, retrying...", e);
                continue;
            }

            // Inner read loop for the current connection
            let deadline = start_time + duration_limit;
            loop {
                if start_time.elapsed() >= duration_limit {
                    break;
                }
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        reporter.abort();
                        return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        break; // Time's up; exit even if read is hanging
                    }
                    res = tls_stream.read(&mut buffer) => {
                        match res {
                            Ok(0) => {
                                tracing::info!("Download stream EOF before time limit, reconnecting...");
                                break; // Break inner loop; outer loop will reconnect
                            }
                            Ok(n) => {
                                total_bytes.fetch_add(n, Ordering::Relaxed);
                            }
                            Err(e) => {
                                tracing::warn!("Download stream error: {}, reconnecting...", e);
                                break; // Break inner loop; outer loop will reconnect
                            }
                        }
                    }
                }
            }
        }

        reporter.abort();

        let elapsed = start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            return Err(ProbeError::BandwidthTestFailed("Elapsed time is zero".to_string()));
        }

        let final_bytes = total_bytes.load(Ordering::Relaxed);
        let final_down_mbps = ((final_bytes as f64 * 8.0) / 1_000_000.0) / elapsed;

        // Upload Phase
        let stream_up = TcpStream::connect(&addr)
            .await
            .map_err(|e| ProbeError::BandwidthTestFailed(format!("Failed to connect for upload: {}", e)))?;

        let mut tls_stream_up = connector
            .connect(server_name, stream_up)
            .await
            .map_err(|e| ProbeError::BandwidthTestFailed(format!("TLS handshake failed for upload: {}", e)))?;

        let up_request = format!(
            "POST /__up HTTP/1.1\r\nHost: {}\r\nContent-Length: 50000000\r\n\r\n",
            target_host
        );
        tls_stream_up
            .write_all(up_request.as_bytes())
            .await
            .map_err(|e| ProbeError::BandwidthTestFailed(format!("Failed to send upload request: {}", e)))?;

        let upload_chunk = [0u8; 8192];
        total_bytes.store(0, Ordering::Relaxed);
        let tx_reporter_up = tx.clone();
        let bytes_clone_up = total_bytes.clone();

        // Start time strictly after request sent
        let up_start_time = Instant::now();

        let reporter_up_token = cancel_token.clone();
        let reporter_up = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let elapsed = up_start_time.elapsed().as_secs_f64();
                        let bytes = bytes_clone_up.load(Ordering::Relaxed);
                        if elapsed > 0.0 {
                            let mut progress = (elapsed / 10.0) * 100.0;
                            if progress > 100.0 { progress = 100.0; }
                            let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                            
                            let event = TelemetryEvent::Bandwidth(BandwidthProgress::Uploading {
                                download_result_mbps: final_down_mbps,
                                current_mbps: speed_mbps,
                                progress_pct: progress,
                            });
                            match tx_reporter_up.try_send(event) {
                                Ok(_) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    tracing::warn!("UI is lagging, dropping telemetry event");
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    tracing::error!("UI channel closed unexpectedly");
                                    break;
                                }
                            }
                        }
                    }
                    _ = reporter_up_token.cancelled() => {
                        break;
                    }
                }
            }
        });

        let up_deadline = up_start_time + duration_limit;
        while up_start_time.elapsed() < duration_limit {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    reporter_up.abort();
                    return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
                }
                _ = tokio::time::sleep_until(up_deadline) => {
                    break;
                }
                res = tls_stream_up.write_all(&upload_chunk) => {
                    match res {
                        Ok(_) => {
                            total_bytes.fetch_add(upload_chunk.len(), Ordering::Relaxed);
                        },
                        Err(e) => {
                            tracing::warn!("Upload stream error: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        reporter_up.abort();

        let up_elapsed = up_start_time.elapsed().as_secs_f64();
        let final_up_bytes = total_bytes.load(Ordering::Relaxed);
        let final_up_mbps = if up_elapsed > 0.0 {
            ((final_up_bytes as f64 * 8.0) / 1_000_000.0) / up_elapsed
        } else {
            0.0
        };

        let final_event = TelemetryEvent::Bandwidth(BandwidthProgress::Finished {
            download_mbps: final_down_mbps,
            upload_mbps: final_up_mbps,
        });
        match tx.try_send(final_event) {
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("UI is lagging, dropping final telemetry event");
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!("UI channel closed unexpectedly");
            }
        }

        Ok(())
    }
}

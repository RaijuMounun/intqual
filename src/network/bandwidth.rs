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

        const CONCURRENT_CONNECTIONS: usize = 8;
        let duration_limit = std::time::Duration::from_secs(10);
        let start_time = Instant::now();

        let (chunk_tx, mut chunk_rx) = mpsc::channel::<usize>(8192);
        let worker_token = cancel_token.child_token();

        for _ in 0..CONCURRENT_CONNECTIONS {
            let tx = chunk_tx.clone();
            let token = worker_token.clone();
            let addr = addr.clone();
            let server_name = server_name.clone();
            let connector = connector.clone();
            let request = request.clone();

            tokio::spawn(async move {
                loop {
                    if token.is_cancelled() { break; }

                    let stream = match TcpStream::connect(&addr).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::debug!("Worker connection failed: {}, retrying...", e);
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                    };

                    let mut tls_stream = match connector.connect(server_name.clone(), stream).await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::debug!("Worker TLS handshake failed: {}, retrying...", e);
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                    };

                    if let Err(e) = tls_stream.write_all(request.as_bytes()).await {
                        tracing::debug!("Worker failed to send request: {}, retrying...", e);
                        continue;
                    }
                    if let Err(e) = tls_stream.flush().await {
                        tracing::debug!("Worker failed to flush request: {}, retrying...", e);
                        continue;
                    }

                    let mut buffer = [0u8; 8192];
                    loop {
                        tokio::select! {
                            _ = token.cancelled() => {
                                return;
                            }
                            res = tls_stream.read(&mut buffer) => {
                                match res {
                                    Ok(0) => {
                                        break; // EOF, reconnect
                                    }
                                    Ok(n) => {
                                        let _ = tx.try_send(n);
                                    }
                                    Err(e) => {
                                        tracing::debug!("Worker stream error: {}, reconnecting...", e);
                                        break; // reconnect
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        drop(chunk_tx); // Drop the original tx so rx can close

        let mut total_bytes_downloaded: usize = 0;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        let deadline = tokio::time::sleep(duration_limit);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    worker_token.cancel();
                    return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
                }
                _ = &mut deadline => {
                    worker_token.cancel();
                    break;
                }
                _ = interval.tick() => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        let mut progress = (elapsed / 10.0) * 100.0;
                        if progress > 100.0 { progress = 100.0; }
                        let speed_mbps = ((total_bytes_downloaded as f64 * 8.0) / 1_000_000.0) / elapsed;
                        let event = TelemetryEvent::Bandwidth(BandwidthProgress::Downloading {
                            current_mbps: speed_mbps,
                            progress_pct: progress,
                        });
                        match tx.try_send(event) {
                            Ok(_) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                tracing::warn!("UI is lagging, dropping telemetry event");
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                tracing::error!("UI channel closed unexpectedly");
                                worker_token.cancel();
                                break;
                            }
                        }
                    }
                }
                msg = chunk_rx.recv() => {
                    match msg {
                        Some(bytes) => {
                            total_bytes_downloaded += bytes;
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        }

        drop(chunk_rx);

        let elapsed = start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            return Err(ProbeError::BandwidthTestFailed("Elapsed time is zero".to_string()));
        }

        let final_down_mbps = ((total_bytes_downloaded as f64 * 8.0) / 1_000_000.0) / elapsed;

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
        let total_bytes = Arc::new(AtomicUsize::new(0));
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

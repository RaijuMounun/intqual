use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;
use rustls_pki_types::ServerName;
use crate::models::{BandwidthMetrics, TelemetryEvent};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
pub struct BandwidthEngine;

impl BandwidthEngine {
    pub async fn test_download(target_host: &str, target_path: &str, tx: mpsc::Sender<TelemetryEvent>) -> Result<(), String> {
        let mut root_store = RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));

        let addr = format!("{}:443", target_host);
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("Failed to connect to {}: {}", addr, e))?;

        let server_name = ServerName::try_from(target_host)
            .map_err(|e| format!("Invalid server name: {}", e))?
            .to_owned();

        let mut tls_stream = connector
            .connect(server_name.clone(), stream)
            .await
            .map_err(|e| format!("TLS handshake failed: {}", e))?;

        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            target_path, target_host
        );
        tls_stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Failed to send request: {}", e))?;

        let mut buffer = [0u8; 8192];
        let total_bytes = Arc::new(AtomicUsize::new(0));
        let bytes_clone = total_bytes.clone();
        let duration_limit = std::time::Duration::from_secs(10);
        let tx_reporter = tx.clone();
        
        // Start time measured strictly after connection & handshake & request sending
        let start_time = Instant::now();

        let reporter = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                interval.tick().await;
                let bytes = bytes_clone.load(Ordering::Relaxed);
                let elapsed = start_time.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let mut progress = (elapsed / 10.0) * 100.0;
                    if progress > 100.0 { progress = 100.0; }
                    let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                    let event = TelemetryEvent::Bandwidth(BandwidthMetrics {
                        download_mbps: speed_mbps,
                        is_finished: false,
                        progress_percentage: progress,
                        is_upload: false,
                        upload_mbps: None,
                    });
                    if tx_reporter.send(event).await.is_err() {
                        break;
                    }
                }
            }
        });

        loop {
            if start_time.elapsed() >= duration_limit {
                break;
            }
            match tls_stream.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes.fetch_add(n, Ordering::Relaxed);
                },
                Err(e) => {
                    reporter.abort();
                    return Err(format!("Error reading from stream: {}", e));
                },
            }
        }

        reporter.abort();

        let elapsed = start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            return Err("Elapsed time is zero".to_string());
        }

        let final_bytes = total_bytes.load(Ordering::Relaxed);
        let final_down_mbps = ((final_bytes as f64 * 8.0) / 1_000_000.0) / elapsed;

        // Upload Phase
        let stream_up = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("Failed to connect for upload: {}", e))?;

        let mut tls_stream_up = connector
            .connect(server_name, stream_up)
            .await
            .map_err(|e| format!("TLS handshake failed for upload: {}", e))?;

        let up_request = format!(
            "POST /__up HTTP/1.1\r\nHost: {}\r\nContent-Length: 50000000\r\n\r\n",
            target_host
        );
        tls_stream_up
            .write_all(up_request.as_bytes())
            .await
            .map_err(|e| format!("Failed to send upload request: {}", e))?;

        let upload_chunk = [0u8; 8192];
        total_bytes.store(0, Ordering::Relaxed);
        let tx_reporter_up = tx.clone();
        let bytes_clone_up = total_bytes.clone();

        // Start time strictly after request sent
        let up_start_time = Instant::now();

        let reporter_up = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                interval.tick().await;
                let elapsed = up_start_time.elapsed().as_secs_f64();
                let bytes = bytes_clone_up.load(Ordering::Relaxed);
                if elapsed > 0.0 {
                    let mut progress = (elapsed / 10.0) * 100.0;
                    if progress > 100.0 { progress = 100.0; }
                    let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                    
                    let event = TelemetryEvent::Bandwidth(BandwidthMetrics {
                        download_mbps: final_down_mbps,
                        is_finished: false,
                        progress_percentage: progress,
                        is_upload: true,
                        upload_mbps: Some(speed_mbps),
                    });
                    if tx_reporter_up.send(event).await.is_err() {
                        break;
                    }
                }
            }
        });

        while up_start_time.elapsed() < duration_limit {
            match tls_stream_up.write_all(&upload_chunk).await {
                Ok(_) => {
                    total_bytes.fetch_add(upload_chunk.len(), Ordering::Relaxed);
                },
                Err(_) => {
                    break;
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

        let final_event = TelemetryEvent::Bandwidth(BandwidthMetrics {
            download_mbps: final_down_mbps,
            is_finished: true,
            progress_percentage: 100.0,
            is_upload: true,
            upload_mbps: Some(final_up_mbps),
        });
        let _ = tx.send(final_event).await;

        Ok(())
    }
}

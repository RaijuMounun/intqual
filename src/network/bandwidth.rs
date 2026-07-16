use std::sync::Arc;
use tokio::time::Instant;
use crate::models::{BandwidthProgress, ProbeError};
use crate::probe::TelemetryEvent;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use tokio::sync::mpsc;
use reqwest::Client;

pub async fn test_download(
        target_host: &str,
        target_path: &str,
        tx: mpsc::Sender<TelemetryEvent>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), ProbeError> {
        let client = match Client::builder()
            .user_agent("intqual-net-tester/1.0")
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(16)
            .connect_timeout(std::time::Duration::from_secs(5))
            .danger_accept_invalid_certs(true)
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                tracing::error!("Failed to build HTTP client for bandwidth test: {:?}", e);
                return Err(ProbeError::BandwidthTestFailed(format!("Failed to build HTTP client: {}", e)));
            }
        };

        let url = Arc::new(format!("https://{}{}", target_host, target_path));

        const CONCURRENT_CONNECTIONS: usize = 4;
        let duration_limit = std::time::Duration::from_secs(10);
        let start_time = Instant::now();

        let total_bytes_downloaded = Arc::new(AtomicUsize::new(0));
        let worker_token = cancel_token.child_token();

        let mut workers = Vec::new();

        // --- Download Phase ---
        for _ in 0..CONCURRENT_CONNECTIONS {
            let token = worker_token.clone();
            let url = url.clone();
            let client = client.clone();
            let bytes_counter = total_bytes_downloaded.clone();
            let tx = tx.clone();
            
            let worker_start_time = Instant::now();

            let handle = tokio::spawn(async move {
                loop {
                    if token.is_cancelled() || worker_start_time.elapsed() >= duration_limit { 
                        return; 
                    }
                    
                    let req = client.get(url.as_str()).send().await;
                    match req {
                        Ok(mut res) => {
                            if !res.status().is_success() {
                                // If we already downloaded some data and hit a limit, just gracefully exit.
                                // Otherwise, if it's the very first request, report the error.
                                if bytes_counter.load(Ordering::Relaxed) == 0 {
                                    if res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                                        tracing::warn!("Cloudflare Rate Limit (Ban) Exceeded during download test");
                                        if tx.send(TelemetryEvent::BandwidthError(ProbeError::RateLimited(
                                            "Cloudflare Rate Limit (Ban) Exceeded. Please wait ~1 hour before testing again.".to_string()
                                        ))).await.is_err() {
                                            tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                            return;
                                        }
                                    } else {
                                        tracing::error!("HTTP Error during download test: {}", res.status());
                                        if tx.send(TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed(
                                            format!("HTTP Error: {}", res.status())
                                        ))).await.is_err() {
                                            tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                            return;
                                        }
                                    }
                                    token.cancel();
                                }
                                return;
                            }

                            loop {
                                tokio::select! {
                                    _ = token.cancelled() => { return; }
                                    chunk_res = res.chunk() => {
                                        match chunk_res {
                                            Ok(Some(chunk)) => {
                                                bytes_counter.fetch_add(chunk.len(), Ordering::Relaxed);
                                                if worker_start_time.elapsed() >= duration_limit {
                                                    return; // Time-box limit reached, graceful exit
                                                }
                                            }
                                            Ok(None) => {
                                                break; // EOF, loop will reconnect to download again
                                            }
                                            Err(e) => {
                                                tracing::error!("Stream error during download test: {}", e);
                                                if tx.send(TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed(e.to_string()))).await.is_err() {
                                                    tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                                    return;
                                                }
                                                token.cancel();
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Request error during download test: {}", e);
                            if tx.send(TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed(e.to_string()))).await.is_err() {
                                tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                return;
                            }
                            token.cancel();
                            return;
                        }
                    }
                }
            });
            workers.push(handle);
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        let deadline = tokio::time::sleep(duration_limit);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    worker_token.cancel();
                    tracing::error!("Bandwidth test cancelled by user");
                    return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
                }
                _ = worker_token.cancelled() => {
                    // One of the workers hit an error (e.g. 429) and cancelled the token.
                    tracing::error!("Test aborted internally due to server error (e.g. Rate Limit)");
                    return Err(ProbeError::BandwidthTestFailed("Test aborted internally due to server error (e.g. Rate Limit)".to_string()));
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
                        let bytes = total_bytes_downloaded.load(Ordering::Relaxed);
                        let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                        
                        let event = TelemetryEvent::Bandwidth(BandwidthProgress::Downloading {
                            current_mbps: speed_mbps,
                            progress_pct: progress,
                        });
                        if let Err(e) = tx.try_send(event) {
                            match e {
                                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                    tracing::warn!(target: "bandwidth_telemetry", "Telemetry channel full, shedding load / dropping data frame");
                                },
                                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                    worker_token.cancel();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Wait for download workers to exit gracefully
        for w in workers {
            if let Err(e) = w.await {
                tracing::error!("Thread Panic during bandwidth test: {}", e);
                return Err(ProbeError::ThreadPanic(e.to_string()));
            }
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            tracing::error!("Elapsed time is zero in bandwidth test");
            return Err(ProbeError::BandwidthTestFailed("Elapsed time is zero".to_string()));
        }

        let final_down_bytes = total_bytes_downloaded.load(Ordering::Relaxed);
        let final_down_mbps = ((final_down_bytes as f64 * 8.0) / 1_000_000.0) / elapsed;

        // --- Upload Phase ---
        let up_url = Arc::new(format!("https://{}/__up", target_host));
        let total_up_bytes = Arc::new(AtomicUsize::new(0));
        let up_worker_token = cancel_token.child_token();
        let mut up_workers = Vec::new();

        const CHUNK_SIZE: usize = 64 * 1024;
        static STATIC_PAYLOAD: [u8; CHUNK_SIZE] = [0u8; CHUNK_SIZE];
        
        let is_timeout = Arc::new(AtomicBool::new(false));
        
        let up_start_time = Instant::now();

        for _ in 0..CONCURRENT_CONNECTIONS {
            let token = up_worker_token.clone();
            let url = up_url.clone();
            let client = client.clone();
            let bytes_counter = total_up_bytes.clone();
            let tx = tx.clone();

            let handle = tokio::spawn(async move {
                let worker_start_time = Instant::now();
                let stream_token = token.clone();
                let stream_bytes_counter = bytes_counter.clone();

                let stream = async_stream::stream! {
                    loop {
                        if stream_token.is_cancelled() || worker_start_time.elapsed() >= duration_limit {
                            break;
                        }
                        
                        stream_bytes_counter.fetch_add(CHUNK_SIZE, Ordering::Relaxed);
                        yield Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::from_static(&STATIC_PAYLOAD));
                    }
                };

                let req = client.post(url.as_str()).body(reqwest::Body::wrap_stream(stream)).send();
                
                tokio::select! {
                    _ = token.cancelled() => {}
                    res = req => {
                        match res {
                            Ok(response) => {
                                if !response.status().is_success() && bytes_counter.load(Ordering::Relaxed) == 0 {
                                    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                                        tracing::warn!("Cloudflare Rate Limit (Ban) Exceeded during upload test");
                                        if tx.send(TelemetryEvent::BandwidthError(ProbeError::RateLimited(
                                            "Cloudflare Rate Limit (Ban) Exceeded during upload.".to_string()
                                        ))).await.is_err() {
                                            tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                        }
                                    } else {
                                        tracing::error!("HTTP Upload Error: {}", response.status());
                                        if tx.send(TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed(
                                            format!("HTTP Upload Error: {}", response.status())
                                        ))).await.is_err() {
                                            tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                        }
                                    }
                                    token.cancel();
                                }
                            }
                            Err(e) => {
                                tracing::error!("Upload chunk failed: {}", e);
                                if tx.send(TelemetryEvent::BandwidthError(ProbeError::BandwidthTestFailed(e.to_string()))).await.is_err() {
                                    tracing::error!("UI channel closed unexpectedly while sending critical error. Shutting down worker.");
                                }
                                token.cancel();
                            }
                        }
                    }
                }
            });
            up_workers.push(handle);
        }

        let mut interval_up = tokio::time::interval(std::time::Duration::from_millis(250));

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    up_worker_token.cancel();
                    tracing::error!("Upload bandwidth test cancelled by user");
                    return Err(ProbeError::BandwidthTestFailed("Cancelled by user".to_string()));
                }
                _ = up_worker_token.cancelled() => {
                    if is_timeout.load(Ordering::Acquire) {
                        break;
                    } else {
                        tracing::error!("Upload test aborted internally due to server error");
                        return Err(ProbeError::BandwidthTestFailed("Test aborted internally due to server error".to_string()));
                    }
                }
                _ = interval_up.tick() => {
                    let st = up_start_time;
                    let elapsed = st.elapsed().as_secs_f64();
                    
                    if elapsed >= duration_limit.as_secs_f64() {
                        is_timeout.store(true, Ordering::Release);
                        up_worker_token.cancel();
                        break;
                    }
                    
                    if elapsed > 0.0 {
                        let mut progress = (elapsed / duration_limit.as_secs_f64()) * 100.0;
                        if progress > 100.0 { progress = 100.0; }
                        let bytes = total_up_bytes.load(Ordering::Relaxed);
                        let speed_mbps = ((bytes as f64 * 8.0) / 1_000_000.0) / elapsed;
                        
                        let event = TelemetryEvent::Bandwidth(BandwidthProgress::Uploading {
                            download_result_mbps: final_down_mbps,
                            current_mbps: speed_mbps,
                            progress_pct: progress,
                        });
                        if let Err(e) = tx.try_send(event) {
                            match e {
                                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                                    tracing::warn!(target: "bandwidth_telemetry", "Telemetry channel full, shedding load / dropping data frame");
                                },
                                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                                    up_worker_token.cancel();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        for w in up_workers {
            if let Err(e) = w.await {
                tracing::error!("Thread Panic during upload test: {}", e);
                return Err(ProbeError::ThreadPanic(e.to_string()));
            }
        }

        let up_elapsed = up_start_time.elapsed().as_secs_f64();
        let final_up_bytes = total_up_bytes.load(Ordering::Relaxed);
        let final_up_mbps = if up_elapsed > 0.0 {
            ((final_up_bytes as f64 * 8.0) / 1_000_000.0) / up_elapsed
        } else {
            0.0
        };

        let final_event = TelemetryEvent::Bandwidth(BandwidthProgress::Finished {
            download_mbps: final_down_mbps,
            upload_mbps: final_up_mbps,
        });
        if let Err(e) = tx.try_send(final_event) {
            match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    tracing::warn!(target: "bandwidth_telemetry", "Telemetry channel full, shedding load / dropping data frame");
                },
                tokio::sync::mpsc::error::TrySendError::Closed(_) => return Ok(()),
            }
        }

        Ok(())
    }

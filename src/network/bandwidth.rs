use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;
use rustls_pki_types::ServerName;

pub struct BandwidthEngine;

impl BandwidthEngine {
    pub async fn test_download(target_host: &str, target_path: &str) -> Result<f64, String> {
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
            .connect(server_name, stream)
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
        let mut bytes_received: usize = 0;
        let start_time = Instant::now();

        loop {
            match tls_stream.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => bytes_received += n,
                Err(e) => return Err(format!("Error reading from stream: {}", e)),
            }
        }

        let elapsed = start_time.elapsed().as_secs_f64();
        if elapsed == 0.0 {
            return Err("Elapsed time is zero".to_string());
        }

        let speed_mbps = ((bytes_received as f64 * 8.0) / 1_000_000.0) / elapsed;

        Ok(speed_mbps)
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use crate::models::NetworkMetrics;

/// TODO: Make comments english and professional
/// FallbackEngine: Sudo gerektirmeyen, standart işletim sistemi yetenekleriyle ağ ölçümü yapar.
pub struct FallbackEngine {
    pub target_ip: String,
    pub target_port: u16,
    pub interval: Duration,
}

impl FallbackEngine {
    /// Yeni bir FallbackEngine örneği oluşturur (Constructor)
    pub fn new(target_ip: String) -> Self {
        Self {
            target_ip,
            target_port: 443, // Varsayılan olarak HTTPS (443) portuna vuruyoruz.
            interval: Duration::from_millis(500), // Tatlı nokta: 500ms
        }
    }

    /// Motoru asenkron (async) olarak başlatır ve kanala veri pompalar.
    pub async fn start(self, tx: mpsc::Sender<NetworkMetrics>) {
        // tokio::spawn, bu sonsuz döngüyü arka planda bağımsız bir Worker Thread gibi çalıştırır.
        tokio::spawn(async move {
            loop {
                // 1. ADIM: TCP Ping (El Sıkışma Süresi Ölçümü)
                let start_time = Instant::now(); // Kronometreyi başlat
                
                // Fail-Fast Mimarisi: Hedef yanıt vermezse 30 saniye beklememek için 1 saniyelik "Timeout" koyuyoruz.
                let tcp_ping_result = match tokio::time::timeout(
                    Duration::from_millis(1000), 
                    TcpStream::connect((self.target_ip.as_str(), self.target_port))
                ).await {
                    // Bağlantı başarılı olduysa geçen süreyi milisaniye (f64) olarak al.
                    Ok(Ok(_stream)) => Ok(start_time.elapsed().as_secs_f64() * 1000.0),
                    // Soket hatası (Örn: Ağ kablosu çekik veya port kapalı)
                    Ok(Err(e)) => Err(format!("Soket Hatası: {}", e)),
                    // 1 Saniye doldu, hedef hiç cevap vermedi.
                    Err(_) => Err("TCP Zaman Aşımı (Timeout)".to_string()),
                };

                // 2. ADIM: Unprivileged ICMP (Şimdilik yer tutucu/Mock)
                // İleride buraya ICMP kütüphanesi entegre edilecek.
                let icmp_ping_result = Err("Unprivileged ICMP yakında eklenecek".to_string());

                // 3. ADIM: Veriyi Pakete Koy (Data Pipeline)
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(Duration::from_secs(0))
                    .as_secs();

                let metrics = NetworkMetrics {
                    target_ip: self.target_ip.clone(),
                    icmp_ping: icmp_ping_result,
                    tcp_ping: tcp_ping_result,
                    timestamp,
                };

                // 4. ADIM: Paketi UI'a fırlat. 
                // Eğer rx (alıcı/UI) kapanmışsa (kullanıcı programdan çıkmışsa), döngüyü kır ve motoru kapat.
                if tx.send(metrics).await.is_err() {
                    break; 
                }

                // 5. ADIM: Uyku (CPU'yu %100 kullanmamak için belirlediğimiz aralık kadar bekle)
                tokio::time::sleep(self.interval).await;
            }
        });
    }
}
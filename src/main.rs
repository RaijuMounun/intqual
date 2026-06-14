// Modüllerimizi (Departmanlarımızı) ana dosyaya tanıtıyoruz
pub mod models;
pub mod engine;

// Kullanacağımız yapıları içeri aktarıyoruz
use engine::{NetworkEngine, FallbackEngine};
use tokio::sync::mpsc;

/// #[tokio::main] makrosu, Rust'ın standart senkron 'main' fonksiyonunu
/// Asenkron (async) bir çalışma zamanına (runtime) dönüştürür.
/// Bu sayede 'await' kelimesini kullanabiliriz.
#[tokio::main]
async fn main() {
    println!("Intqual Ağ Motoru Başlatılıyor...");

    // 1. İletişim Kanalını Kur (MPSC - Multi-Producer, Single-Consumer)
    // 100 değeri "Bounded Channel" (Sınırlandırılmış Kanal) kapasitesidir.
    // Eğer UI thread'i çökerse ve okumayı bırakırsa, RAM şişmesin diye kanal 100 pakette tıkanır. (Backpressure)
    let (tx, mut rx) = mpsc::channel(100);

    // 2. Hedefi Belirle (Şimdilik hardcoded, ileride CLI argümanından gelecek)
    let target = "google.com".to_string();

    // 3. Fallback Motorunu Örnekle (Instantiate)
    let fallback_engine = FallbackEngine::new(target);

    // 4. Motoru Başlat ve vericiyi (tx) içine enjekte et
    fallback_engine.start(tx).await;

    println!("Motor çalışıyor, veriler bekleniyor...\n");

    // 5. Aptal UI (Dumb UI) Simülasyonu - Ana Döngü
    // 'rx.recv().await' kanalda veri yoksa CPU'yu yormadan uyur, veri gelince uyanır.
    while let Some(metrics) = rx.recv().await {
        
        // Veriyi terminale jilet gibi basıyoruz
        println!("Hedef IP  : {}", metrics.target_ip);
        println!("Zaman     : {}", metrics.timestamp);
        
        // Pattern Matching (Desen Eşleştirme) ile TCP sonucunu güvenle okuyoruz
        match metrics.tcp_ping {
            Ok(ms) => println!("TCP Ping  : {:.2} ms 🟢", ms),
            Err(e) => println!("TCP Ping  : {} 🔴", e),
        }

        match metrics.icmp_ping {
            Ok(ms) => println!("ICMP Ping : {:.2} ms 🟢", ms),
            Err(e) => println!("ICMP Ping : {} 🟡\n", e),
        }
        
        println!("-----------------------------------");
    }
}
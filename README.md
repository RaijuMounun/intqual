# Intqual (Internet Quality)

A zero-runtime-dependency, asynchronous dual-layer network observability tool built with Rust and Ratatui.

Unlike traditional `ping` utilities that only measure network-layer (ICMP) latency, **Intqual** simultaneously probes the application layer (TCP) to instantly diagnose whether a network bottleneck is caused by local infrastructure or the target server's application stack.

## Key Features

- **Dual-Layer Probing:** Runs asynchronous TCP Handshakes and OS-level ICMP Datagram probes concurrently.
- **Unprivileged Execution:** Utilizes `SOCK_DGRAM` for ICMP requests, allowing standard users to run diagnostics without `sudo` or root privileges on modern Linux/macOS environments.
- **Zero-Leak Architecture:** Employs `SO_LINGER=0` (TCP RST) offloaded to blocking threads, preventing Ephemeral Port Exhaustion and `TIME_WAIT` socket leaks during high-frequency stress tests.
- **Gestalt UI Design:** Built with a Bento Grid layout using Ratatui. Latency is represented via line charts, while Jitter is mapped to synchronized sparklines to reduce cognitive load.
- **Immediate-Mode Optimization:** Incorporates a dirty-flag render loop to prevent Compositor/GPU crashes (e.g., KWin WebGL timeouts) by only drawing when the state explicitly mutates.
- **Active Visual Alarms:** High-contrast terminal inversion for catastrophic packet loss and threshold-based color coding for jitter instability.

## Installation

### macOS & Linux (Homebrew)

The easiest way to install on macOS and Linux is via our official Homebrew tap:

```bash
brew tap RaijuMounun/intqual
brew install intqual
```

or

```bash
brew install RaijuMounun/intqual/intqual
```

### Windows (Winget)

You can easily install `intqual` on Windows 10/11 using the official Windows Package Manager:

```powershell
winget install RaijuMounun.intqual
```

(Note: Must be run in a terminal with Administrator privileges if your system strictly blocks raw ICMP sockets).

### Arch Linux (AUR)

If you are on an Arch-based distribution, clone the repository and build the package locally via `makepkg`. It will be available on the official AUR soon:

```bash
git clone https://github.com/RaijuMounun/intqual.git
cd intqual
makepkg -si
```

### Universal (Cargo)

If you have the Rust toolchain installed, you can compile and install it globally from source:

```bash
cargo install intqual
```

### Pre-compiled Binaries

Grab the latest standalone executable for your OS from the [Releases](https://github.com/RaijuMounun/intqual/releases) page and place it in your system's `PATH`.

## Usage

Simply run the binary. By default, it targets google.com on port 443.

```bash
intqual
```

Advanced CLI Arguments:

```bash
Usage: intqual [OPTIONS] [TARGET]

Arguments:
  [TARGET]  The target IP address or hostname to analyze [default: google.com]

Options:
  -p, --port <PORT>          The target port for TCP measurements [default: 443]
  -i, --interval <INTERVAL>  Polling interval in milliseconds [default: 500]
  -t, --timeout <TIMEOUT>    Connection timeout threshold in ms [default: 1000]
  -h, --help                 Print help
  -V, --version              Print version
```

## Roadmap

### Completed

- [x] TCP Ping & ICMP Ping core implementations
- [x] Packet Loss & Jitter calculations
- [x] Real-time Jitter & Ping graphical visualization
- [x] Automated release workflows (TOML & PKGBUILD)

### Upcoming Features & Architecture Goals

- **Multi-Target Dashboard:** Expand the UI to monitor multiple endpoints (e.g., Google, Cloudflare, AWS) simultaneously on a unified dashboard.
- **Advanced Network Analysis:**
  - **Traceroute & Hop Analysis:** MTR-style hop-by-hop tracking via TTL manipulation.
  - **ISP & DPI Diagnostics:** Detect port blocking, DNS resolution delays, and route manipulation/filtering.
  - **Throughput Monitoring:** Real-time bandwidth tracking for download and upload speeds.
  - **Wi-Fi Diagnostics:** Wavemon-style wireless signal strength and quality metrics.
- **UI & UX Enhancements:**
  - **Dynamic Layouts & Widgets:** Introduce Traceroute maps, throughput gauges, and responsive sparklines using a Progressive Disclosure Bento Grid.
  - **Visual Polish:** Integrate `tachyon-fx` for terminal animations and introduce customizable color themes. Fix hardcoded chart widths for full horizontal responsiveness.
  - **User Guidance:** Intelligent permission warnings (e.g., prompting for Administrator/root privileges when raw sockets are required).
- **Architecture & Performance Optimizations:**
  - **Non-blocking Data Pipelines:** Implement lossy `try_send` or ring-buffer approaches to ensure the core probing engine never stalls if UI rendering lags behind.
  - **Thread-Pool Resilience:** Prevent async thread starvation during network blackholes to maintain absolute stability under extreme timeouts.
  - **Decoupled Architecture (SOLID):** Transition to trait-based abstractions (Dependency Inversion) for the core engine, enabling mock testing. Refactor internal metrics to dynamic collections (Open/Closed Principle) to easily support future probes like DNS or HTTP(S) handshakes.
  - **Pub/Sub Telemetry:** Evolve the current MPSC architecture into a broadcast channel, allowing independent background tasks (like CSV logging or alerting) to consume metrics without coupling to the UI (Single Responsibility Principle).
  - **Advanced Configuration:** Support robust configuration profiles (JSON/YAML) instead of relying solely on CLI arguments.
  - **Cross-Platform Refinements:** Evaluate conditional compilation (`cfg(target_os)`) vs. established crates (like `surge-ping`) for seamless, robust raw socket handling across different OS environments.

## License

This project is licensed under the MIT License.

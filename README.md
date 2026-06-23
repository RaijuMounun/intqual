# Intqual (Internet Quality)

A zero-runtime-dependency, asynchronous dual-layer network observability tool built with Rust and Ratatui. 

Unlike traditional `ping` utilities that only measure network-layer (ICMP) latency, **Intqual** simultaneously probes the application layer (TCP) to instantly diagnose whether a network bottleneck is caused by local infrastructure or the target server's application stack.

## Key Features

* **Dual-Layer Probing:** Runs asynchronous TCP Handshakes and OS-level ICMP Datagram probes concurrently.
* **Unprivileged Execution:** Utilizes `SOCK_DGRAM` for ICMP requests, allowing standard users to run diagnostics without `sudo` or root privileges on modern Linux/macOS environments.
* **Zero-Leak Architecture:** Employs `SO_LINGER=0` (TCP RST) offloaded to blocking threads, preventing Ephemeral Port Exhaustion and `TIME_WAIT` socket leaks during high-frequency stress tests.
* **Gestalt UI Design:** Built with a Bento Grid layout using Ratatui. Latency is represented via line charts, while Jitter is mapped to synchronized sparklines to reduce cognitive load.
* **Immediate-Mode Optimization:** Incorporates a dirty-flag render loop to prevent Compositor/GPU crashes (e.g., KWin WebGL timeouts) by only drawing when the state explicitly mutates.
* **Active Visual Alarms:** High-contrast terminal inversion for catastrophic packet loss and threshold-based color coding for jitter instability.

## Installation

### Arch Linux (AUR)
If you are on an Arch-based distribution, you can install Intqual directly from the Arch User Repository:
```bash
yay -S intqual
```

### From Binaries (Linux)

Grab the latest compiled, zero-dependency executable from the Releases page and place it in your PATH.

#### Build from Source (Cargo)

Ensure you have the Rust toolchain installed, then run:
```bash
git clone https://github.com/RaijuMounun/intqual.git
cd intqual
cargo build --release
```

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


## License
This project is licensed under the MIT License.

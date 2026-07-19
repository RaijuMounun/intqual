# Intqual (Internet Quality)

> *Note on nomenclature: While "Network Quality" is the conventional English term, **Intqual** is derived from "Internet Quality"—a phrasing that makes perfect semantic sense in Turkish and inspired the project's name.*

A TUI-based, asynchronous network analysis tool designed to diagnose dual-layer latency issues with precision.

Unlike traditional utilities, **Intqual** measures both application-layer (TCP) and network-layer (ICMP) latency simultaneously, providing instant visibility into whether network bottlenecks stem from local infrastructure or remote services.

---

## Core Features

- **Ratatui-based Responsive TUI:** A dynamic, visually rich user interface with real-time graphs.
- **Non-blocking Asynchronous DNS:** Fast, concurrent DNS resolution and network probing powered by Tokio.
- **Event-driven State Management:** Highly efficient UI rendering triggered strictly by state mutations, avoiding unnecessary redraws and compositor crashes.
- **Secure Privilege Separation:** Graceful root privilege management that prioritizes security without compromising user experience.

## Architecture

Intqual is built with **Security by Design** and strictly adheres to the **Principle of Least Privilege (PoLP)**. The application's main process runs entirely unprivileged. When raw socket operations are required, it dynamically spawns ephemeral **Worker Processes** via `sudo`. These isolated, asynchronous sub-processes communicate securely and efficiently back to the unprivileged main application using **JSONL IPC Streaming**. This ensures maximum security while maintaining the high performance of non-blocking telemetry.

---

## Installation

### Universal (Cargo)

You can compile and install Intqual globally using the Rust toolchain. To install the latest version from crates.io:

```bash
cargo install intqual
```

Alternatively, to install from the local source:

```bash
cargo install --path .
```

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

> **Note:** The Windows version is currently experiencing a critical bug because our traceroute implementation relies on `sudo`-based worker processes, which are incompatible with Windows. The application is likely to crash during traceroute operations. We will provide Winget installation instructions here once this architectural issue is resolved for the Windows platform.

### Arch Linux (AUR)

If you are on an Arch-based distribution, clone the repository and build the package locally via `makepkg`. It will be available on the official AUR soon:

```bash
git clone https://github.com/RaijuMounun/intqual.git
cd intqual
makepkg -si
```

### Pre-compiled Binaries

Grab the latest standalone executable for your OS from the [Releases](https://github.com/RaijuMounun/intqual/releases) page and place it in your system's `PATH`.

---

## Usage

Simply run the binary. 

```bash
intqual
```

By default, Intqual targets `google.com` on port `443`. From the main dashboard, you can use the **TUI navigation** to seamlessly switch between different analysis modules, including **Speed Tests**, **Ping**, and **Traceroute** diagnostics.

### Advanced CLI Arguments

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

---

## License

This project is licensed under the MIT License.

# Great Decoupling Design (Library & UI Separation)

## Goal
Separate the `intqual` project into a modular architecture to isolate the core networking logic from the user interface (UI). This enables a true "Great Decoupling", meaning third-party developers can use the `intqual-core` library as a dependency to build custom interfaces (e.g., Qt, Web apps, or automation scripts) without pulling in terminal-specific dependencies like `ratatui` or `crossterm`.

## Architecture
We will migrate the current monolithic crate to a Cargo Workspace with two distinct crates:

1. **`crates/core` (Library Crate: `intqual-core`)**
   - **Contains:** `engine`, `network`, `probe`, `models`, `utils`.
   - **Dependencies:** Core networking and async ecosystem (`tokio`, `socket2`, `reqwest`, `tracing`, `thiserror`, `anyhow`).
   - **Responsibility:** Expose a public API to initialize the network analysis engine, run it, and stream telemetry data via async channels.

2. **`crates/cli` (Binary Crate: `intqual`)**
   - **Contains:** `main.rs`, `ui`, and CLI configuration.
   - **Dependencies:** `intqual-core` (as a path dependency), `clap`, `ratatui`, `crossterm`, `tui-big-text`.
   - **Responsibility:** Consume the `intqual-core` API, parse user arguments, and render the terminal user interface.

## Interface Boundaries
- The UI layer will no longer have access to internal engine details unless they are explicitly marked as `pub` in `intqual-core`.
- Communication between Core and CLI will continue to use Tokio's `mpsc` channels, but the message types (e.g., `ProbeError`, `EngineCommand`) will now act as the public contract of the `intqual-core` crate.

## Strict Rules
- **Error Management:** As per the project's error management rules, `intqual-core` will never swallow errors or print them to stdout. All errors must be logged via `tracing` and returned as explicit `Result` types (using `ProbeError`) to the caller (the CLI/UI), which will handle graceful shutdown or display.

# Bluetooth Monitor v0.7.0

Cross-platform Bluetooth Low Energy GATT monitor, capture workstation and protocol analyzer written in Rust.

## Tech baseline

All direct dependencies are exact-pinned in `Cargo.toml` against the latest stable releases checked on 2026-09-21:

- Rust 1.98.1 / Edition 2024
- btleplug 0.13.1
- Tokio 1.53.1
- eframe + egui 0.36.2
- egui_plot 0.37.0
- serde 1.0.229 / serde_json 1.0.151
- futures 0.3.34
- uuid 1.26.1
- csv 1.4.0
- crc 3.4.0
- chrono 0.4.45
- rfd 0.17.2
- anyhow 1.0.104
- thiserror 2.0.20
- tracing 0.1.44 / tracing-subscriber 0.3.23

`Cargo.lock` is committed. CI uses `--locked` to verify the same dependency resolution on all three platforms.

## v0.7 highlights

### Workspace and multi-session analysis

The UI now has a persistent workspace layer above the BLE connection:

- One attached **Live Session** receives the current BLE connection.
- Switching to a different device archives the previous live context as a **Capture Session**.
- Every opened `.bmon` file becomes an independent **Replay Session**.
- Logs, traffic counters, protocol decoder state, decoded frames, notes and bookmarks are isolated per Session.
- Live BLE notifications continue to be routed to the Live Session even while a Replay Session is being viewed.
- Session tabs show retained log/frame counts and allow replay/capture sessions to coexist.

The BLE worker still intentionally owns **one live peripheral connection at a time** in v0.7. Multi-session means multiple analysis contexts, not multiple simultaneously connected BLE peripherals. This keeps connection semantics explicit while preserving work across devices.

### Workspace files

Workspaces can be opened, saved and saved-as using JSON (`*.bmw.json` convention). They persist:

- workspace name
- Session metadata
- device identity metadata
- capture/replay file references
- Session notes
- bookmarks
- active Session
- panel visibility/layout preferences

Workspaces do not duplicate large packet captures into JSON. Replay/Capture Sessions reference their `.bmon` source instead. A live session that was never captured can keep its metadata/bookmarks while the app is running, but its packet history cannot be restored after restart without a `.bmon` file.

### Bookmarks and event navigation

- Bookmark any monitor row.
- Bookmark decoded protocol frames.
- Bookmark the current BMON playback position.
- Rename and delete bookmarks.
- Jump from a bookmark to a replay timestamp.
- Select a bookmarked protocol frame.
- Replay adds Previous Event / Next Event navigation in addition to Play, Pause, Step and Seek.

### Protocol field table

The protocol analyzer keeps the scrolling frame list but now also supports selecting a frame and viewing its decoded fields in a dedicated table. This is more useful for protocols with many fields than rendering every value inline.

### Saved layout

Devices/GATT, Plot, Protocol, Replay, Bookmarks and Monitor sections can be shown or hidden. These choices are persisted in app preferences and workspace files.

### Protocol plugin boundary

`src/plugin.rs` introduces **Protocol Plugin API 1.0** host-side data types and a `ProtocolPlugin` trait. The boundary is deliberately independent of `egui` and `btleplug` internals.

v0.7 does **not** load arbitrary native, Lua or WASM code yet. The goal of this version is to stabilize the host packet/frame contract before a sandboxed runtime is introduced.

See `docs/PLUGIN_API.md`.

## Existing capabilities

- Event-driven BLE scan and live RSSI updates
- Adapter enumeration and selection
- BLE Service UUID scan filters plus app-side name/RSSI/service filtering
- Connect / disconnect / exponential-backoff reconnect
- Advertisement inspector
  - address type
  - TX power
  - appearance
  - advertised services
  - manufacturer data
  - service data
- Full GATT service / characteristic / descriptor tree
- Characteristic Read / Write / Write Without Response
- Notify / Indicate subscriptions
- Descriptor Read / Write
- HEX + ASCII traffic monitor
- RX / RD / TX filtering, search and traffic counters
- CSV + `.bmon` capture
- Device Profiles
- BMON timeline playback at 0.25x–20x
- Up to 8 realtime numeric plot channels
- Stream protocol framing
  - BLE packet
  - fixed length
  - delimiter
  - length field
- CRC validation
  - CRC-8/SMBUS
  - CRC-16/MODBUS
  - CRC-16/XMODEM
  - CRC-16/IBM-SDLC
- Named decoded fields with offset / type / scale / bias
- Protocol CSV / JSON export and presets
- TX history and periodic sending

## Project layout

```text
src/
├── main.rs
├── app.rs
├── capture.rs
├── codec.rs
├── plotting.rs
├── plugin.rs
├── profile.rs
├── protocol.rs
├── protocol_export.rs
├── protocol_preset.rs
├── replay.rs
├── workspace.rs
└── ble/
    ├── mod.rs
    ├── model.rs
    └── worker.rs
```

## Run

### Interface language

Use the **语言 / Language** selector at the top of the window to switch between
Simplified Chinese (the default) and English. Changes apply immediately and are
saved with the application's preferences, including when the window closes.
Existing preferences without a language field remain compatible.

Translations are embedded from `locales/en.json` and `locales/zh-CN.json`.
Keep both catalogs' keys and numbered placeholders (`{0}`, `{1}`, …) in sync;
`cargo test` checks their consistency. Arguments are inserted without translating
device names, user notes, UUIDs, or captured data. Low-level driver diagnostics
remain in their original language.

Chinese text uses a system font fallback: Microsoft YaHei on Windows, PingFang
on macOS, and Noto Sans CJK or WenQuanYi Micro Hei on Linux. If none is installed,
set `BLUETOOTH_MONITOR_FONT` to a CJK `.ttf`, `.otf`, or `.ttc` font file.

```bash
cargo run
```

Required validation gate before treating a build as release-ready:

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The included CI runs these checks on Windows, macOS and Linux.

## Platform notes

### Windows

Use a BLE-capable adapter and keep Bluetooth enabled.

### macOS

The terminal/application may need Bluetooth permission under System Settings. Peripheral identity is treated as opaque rather than assuming it is a MAC address.

### Linux

BlueZ and D-Bus development packages are required. The GitHub Actions job installs the required native packages on Ubuntu. See `docs/SCAN_FILTERS.md` for BlueZ filter behavior.

## Validation status

The project is validated with Rust 1.98.1. See `docs/VALIDATION.md` for the local validation gate and the Windows, macOS, and Linux CI matrix.

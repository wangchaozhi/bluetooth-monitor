# Validation

The project pins Rust 1.98.1 in `rust-toolchain.toml` and commits `Cargo.lock`.
Run the same checks as CI before pushing:

```bash
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

GitHub Actions runs this gate on Windows, macOS, and Linux. Linux runners
install the D-Bus and window-system development packages needed by btleplug
and egui. Windows builds use the MSVC Rust toolchain and Visual Studio tools.

Tests cover capture/replay, protocol decoding, UUID parsing, workspace and
profile compatibility, and localization catalog and preference consistency.
Hardware discovery, connections, and GATT operations need manual testing with
a BLE adapter and a known peripheral. CI does not validate radio behavior or
device-name availability.

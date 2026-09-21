# Validation status

## Performed in the artifact-generation environment

- `Cargo.toml` parsing
- `rust-toolchain.toml` parsing
- GitHub Actions YAML parsing
- Rust source lexical delimiter/string sanity checks
- duplicate/repeated initializer/reference scans around the v0.7 Session refactor
- local Markdown link checks
- stale-version/reference scans
- source SHA-256 manifest generation
- ZIP archive integrity check
- current API spot-checks against `eframe 0.36.2` and other pinned dependency documentation

## Not available in this environment

The environment does not contain `cargo`, `rustc`, `rustfmt` or `clippy`, and the Rust standalone toolchain download endpoint is not reachable from this runtime. Therefore this snapshot is **not claimed to have passed compilation** here.

## Required real-toolchain gate

```bash
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

The included GitHub Actions workflow performs this matrix on Windows, macOS and Linux. A real `Cargo.lock` should be generated and committed by the first successful Cargo build.

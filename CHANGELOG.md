# Changelog

## 0.7.0 - 2026-09-21

### Added

- Workspace model with JSON open/save/save-as support.
- Per-session isolated monitor logs, traffic counters, protocol decoder state and decoded frames.
- Live, Capture and Replay Session types.
- Automatic archival of the previous Live Session when connecting to a different BLE device.
- Restoration of BMON-backed Capture/Replay Sessions from workspace file references.
- Session notes and persistent bookmarks.
- Log-row, protocol-frame and replay-position bookmarks.
- Bookmark navigation to replay offsets and decoded protocol frames.
- Replay Previous Event / Next Event navigation.
- Selectable decoded protocol frame with dedicated field table.
- Persisted panel visibility for Devices/GATT, Plot, Protocol, Replay, Bookmarks and Monitor.
- Protocol Plugin API 1.0 host-side boundary in `src/plugin.rs`.
- Workspace/plugin architecture documentation.

### Changed

- Opening `.bmon` creates a new independent Replay Session instead of replacing the current replay state.
- Live BLE events are always routed to the attached Live Session, even while another Session is selected.
- Realtime plot buffers are cleared when changing Session to avoid mixing data from different analysis contexts.
- Capture startup records the generated BMON path in the current Live Session metadata.
- Application preferences now persist workspace panel visibility.
- Version bumped to 0.7.0.

### Limitations

- v0.7 still supports one simultaneously connected live BLE peripheral. Multiple Sessions provide concurrent analysis context, not multiple live BLE links.
- Workspace JSON references capture files instead of embedding packet histories. Uncaptured live packet history cannot be restored after restart.
- The protocol plugin API is a boundary only; arbitrary Lua/WASM/native plugin loading is intentionally deferred.

## 0.6.0 - 2026-09-21

- BMON timeline playback, scan filters, protocol CSV/JSON export and protocol presets.

## 0.5.0

- Protocol decoder abstraction and stream framer.
- CRC validation, decoded fields, multi-channel plots, TX history and periodic sending.
- Three-platform CI.

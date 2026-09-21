# Protocol Plugin API 1.0

v0.7 introduces a host-side protocol-plugin boundary without enabling third-party code execution yet.

## Why define the boundary first?

BLE transport objects, GUI types and platform handles are poor plugin ABI types. The plugin boundary therefore uses plain serializable data:

- `ProtocolPluginManifest`
- `PluginPacket`
- `PluginFrame`
- `PluginField`

`ProtocolPlugin` receives packets and returns zero or more decoded frames. Plugins do not receive an `egui` context, a `btleplug::Peripheral`, file handles or arbitrary application state.

## API version

The initial host version is `1.0`. Future sandboxed runtimes should reject incompatible major versions and may accept older minor versions when the host can provide the required capabilities.

## Planned runtimes

A later version can adapt this boundary to Lua or WASM. The preferred direction is sandboxed execution with explicit resource limits and capability-based host calls rather than loading arbitrary native shared libraries into the monitor process.

v0.7 contains no Lua/WASM engine dependency and does not execute external plugin code.

# Protocol Analyzer

Bluetooth Monitor v0.7 includes a stream protocol layer above BLE notifications. The layer is intentionally independent from the GUI so additional decoders can be added without changing the BLE worker.

## Pipeline

```text
BLE Notification
      |
      v
Characteristic source filter
      |
      v
StreamProtocolDecoder
      |
      +--> frame boundary detection
      +--> CRC validation
      +--> named field decoding
      |
      v
Decoded frame list / plot channels
```

The protocol analyzer consumes only RX notifications from the selected Service + Characteristic pair. Read responses and TX records remain visible in the normal monitor but are not fed into the stream decoder.

## Framing modes

### BLE packet

Each BLE notification is treated as one complete protocol frame. This is appropriate when the peripheral already aligns application frames with notification boundaries.

### Fixed length

Bytes from successive notifications are accumulated until the configured frame length is available. Multiple frames can be emitted from one notification.

### Delimiter

Bytes are accumulated until the configured byte sequence is found. The delimiter may be retained in or removed from the emitted frame.

### Length field

The decoder reads an unsigned 1-, 2-, or 4-byte length field at the configured offset. Little- and big-endian values are supported.

```text
total_frame_length = decoded_length_field + adjustment
```

`adjustment` can therefore model protocols where the length field describes only the payload or excludes a checksum/header.

Safety limits currently cap the stream buffer at 1 MiB and a single decoded frame at 64 KiB. Invalid framing settings cause byte-wise resynchronization instead of unbounded allocation.

## CRC modes

The CRC bytes are expected at the end of the decoded frame. Built-in modes are:

- CRC-8/SMBUS, one-byte tail
- CRC-16/MODBUS, little-endian tail
- CRC-16/MODBUS, big-endian tail
- CRC-16/XMODEM, big-endian tail
- CRC-16/IBM-SDLC, little-endian tail

CRC calculations use the algorithm definitions supplied by the `crc` crate rather than custom lookup tables.

## Named fields

Each field has:

- name
- byte offset
- primitive value type
- scale
- bias

Supported primitive types are u8/i8, u16/i16/u32/i32 in little- or big-endian order, and f32 in little- or big-endian order.

The displayed value is:

```text
value = decoded_primitive * scale + bias
```

An out-of-range field is shown as `N/A`; it does not invalidate the entire frame.

## Extension point

The protocol core exposes a `ProtocolDecoder` trait. `StreamProtocolDecoder` is the built-in implementation. Future protocol-specific decoders (for example Modbus-like application frames or vendor protocols) can implement the same trait while keeping BLE transport and UI concerns separate.

## Presets and export

v0.7 includes built-in framing/CRC presets plus custom JSON preset persistence under `protocol-presets/`. Decoded frames can be exported to CSV with dynamic field columns or to structured JSON; both formats retain sequence, timestamp, CRC status and raw HEX.

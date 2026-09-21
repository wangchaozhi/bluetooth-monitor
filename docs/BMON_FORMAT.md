# BMON capture format v1

The `.bmon` format is a small framed binary capture format used by Bluetooth Monitor.

All integer fields are little-endian.

## File header

| Field | Size | Value |
| --- | ---: | --- |
| magic | 5 bytes | ASCII `BMON` followed by byte `0x01` |

## Record

Records follow immediately after the file header until EOF.

| Field | Type | Meaning |
| --- | --- | --- |
| timestamp_len | u16 | UTF-8 byte length of timestamp |
| timestamp | bytes | UTF-8 timestamp string |
| direction | u8 | `1=RX`, `2=RD`, `3=TX`, `0=unknown` |
| service_uuid_len | u16 | UTF-8 byte length |
| service_uuid | bytes | Service UUID string |
| characteristic_uuid_len | u16 | UTF-8 byte length |
| characteristic_uuid | bytes | Characteristic UUID string; descriptor operations may include descriptor context in this text field |
| payload_len | u32 | Payload byte length |
| payload | bytes | Raw BLE value bytes |

Version 1 deliberately stores UUIDs and timestamps as text. This makes the initial format simple and cross-platform.

## Replay behavior

v0.7 loads a BMON v1 file into the timeline replay controller. Playback preserves recorded timestamp/direction and feeds records into the monitor, plot and protocol-analysis path. Replay intentionally does **not** re-write imported records into an active capture session.

Replay is timeline-driven: play/pause/stop, single-step, seek and 0.25x–20x speed are supported. Relative timing is derived from capture timestamps; records with unparseable timestamps receive conservative synthetic spacing.

## Reader safety limits

The v0.7 reader rejects unexpectedly large fields before allocating them:

- string field: maximum 64 KiB
- payload field: maximum 16 MiB

These are application safety limits, not changes to the on-disk v1 wire format.

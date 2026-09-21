# Workspaces and Sessions

## Session model

Bluetooth Monitor separates the current BLE transport from analysis history.

- **Live** — the Session attached to the single live BLE worker connection.
- **Capture** — an archived live-device context. If a BMON source exists it can be restored as a replayable Session.
- **Replay** — a `.bmon` timeline loaded from disk.

Each runtime Session owns its monitor logs, counters, stream decoder state, decoded frames, notes and bookmarks.

The current BLE worker remains single-connection in v0.7. Connecting a different device archives the previous Live Session and creates a fresh Live Session rather than silently mixing packets from two devices.

## Workspace file

Workspace JSON uses schema version 1 and stores metadata only. The conventional filename is `*.bmw.json`.

Persisted data includes:

- workspace name and active Session id
- Session type/name/device identity
- source BMON path where available
- bookmarks and notes
- layout visibility switches

Large packet buffers are not embedded. This keeps workspace saves small and avoids duplicating capture files.

## BMON-backed restoration

When opening a workspace, Capture/Replay Sessions that reference a readable `.bmon` are rebuilt with a replay controller. If a source file is missing, the Session metadata, notes and bookmarks are still preserved, but packet history is unavailable.

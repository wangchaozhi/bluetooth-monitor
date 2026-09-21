# BMON Replay

A `.bmon` file stores ordered transport records. v0.7 converts them into a relative timeline using the first parseable capture timestamp as zero.

## Controls

- **Play / Pause** — resume or freeze timeline time.
- **Stop** — pause and seek to zero.
- **Step** — emit the next record without starting the clock.
- **Speed** — 0.25x to 20x.
- **Timeline** — seek to a relative millisecond position.

Seeking moves the replay cursor to the first record at or after the chosen position. Replayed records pass through the same monitor, plotting and protocol-analysis ingest path as live records, but capture writing is disabled so a replay cannot recursively duplicate itself into a new capture.

If a timestamp cannot be parsed, the loader assigns a conservative synthetic 10 ms spacing for that record.

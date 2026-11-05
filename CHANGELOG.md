# Changelog

All notable changes to QStreamKit are documented in this file.
The format follows Keep a Changelog and the project uses semantic versioning.

## [0.1.0] - 2026-11-05

### Added

- QSTREAM framing, token, record, capture and GPU monitor decoders.
- TCP client with optional Unix socket support and connection retry.
- `probe`, `version`, `start`, `stop`, `scope`, `scope-off`, `fps`, `capture`,
  `capture-frame`, `frame-rate`, `delimiters`, `counters`, `monitor`, `decode`,
  `send`, `listen`, `tokens` and `perfetto` commands.
- GPU counter selection for Adreno 8xx GSL blocks and monitor mode decoding.
- Monitor CSV export with counter values and per-counter samples.
- JSON output for `probe`, `scope` and `monitor`.
- Portable Android probe app with a normal Gradle build.
- Real protocol fixtures and unit tests for records, capture and monitor data.
- CI for formatting, clippy, tests, MSRV, packaging and release binaries.
- MIT and Apache-2.0 license texts plus trademark notice.

### Changed

- `counters add` is additive by default; `--reset` removes the curated pairs.
- `monitor` replaces only the stock pairs by default; `--reset` removes all
  curated pairs.
- `version` stops the GPU scope after printing the reply.
- `scope` leaves the scope running and documents `scope-off`.
- Empty replies and zero-record runs exit with code 1 unless `--allow-empty` is
  set.
- `capture --frame` no longer sends capture stop.

### Fixed

- FPS parsing accepts a partially framed tail.
- Capture and monitor send stop on read-error paths.
- `capture` no longer waits for an idle stream before starting.
- Perfetto uses unique remote paths and reports `adb` stderr.
- The scope stop token is named `CMD_SCOPE_STOP` instead of the misleading
  `CMD_GPU_SCOPE_ENABLE`.

### Known limitations

- The protocol is undocumented and tied to the tested Adreno 8xx driver.
- Only the first selected counter pair has a decoded cumulative value in the
  compact monitor report.
- Only debuggable apps can host the vendor layer.
- The vendor layer binary is not distributed with this project.

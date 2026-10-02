# FerrisSight

An open-source Rust camera gateway and lightweight NVR.

Early-stage: the Rust package provides a small HTTP service, bounded RTSP probing,
Phase 1B local H.264 recording into independently playable MP4 segments,
the Phase 1C bounded UTC minute-session experiment, and Phase 2A local retention
with a durable SQLite upload queue.
Recording copies compressed video without transcoding; audio is omitted. Camera
discovery and the Rust ONVIF adapter remain unimplemented.

```text
IP Camera
   ↓
ONVIF / RTSP
   ↓
FerrisSight Gateway
   ├── local recording (bounded example)
   ├── remote access (planned)
   ├── cloud backup (planned)
   └── API / apps (health API available; apps planned)
```

The goal is a lightweight, vendor-neutral, async gateway suitable for continuous
operation on small Linux systems. Capability discovery and protocol adapters keep
vendor quirks outside the core. Media backends will use mature existing tools or
libraries. Phase 0 excludes a full NVR, transcoding, authentication, cloud backup,
AI detection, UI, mobile apps and infrastructure orchestration.

The `ferrissight` package contains core domain types, camera interfaces, an
explicitly unimplemented ONVIF adapter, media and storage modules, and the Axum
server. All first-party Rust implementation code lives under one `src/`:

```text
src/
  main.rs       # gateway executable
  lib.rs        # shared module entry point
  core/         # domain types and redacted secrets
  camera/       # camera interfaces
  onvif/        # protocol adapter boundary
  media/        # RTSP probing and recording sessions
  storage/      # MP4 output and metadata
  server/       # HTTP API
examples/       # bounded probe and recording runners
tests/          # integration tests
vendor/retina/  # third-party RTSP library
```

See [architecture](docs/architecture.md), [RTSP probing](docs/rtsp-probing.md),
[local recording](docs/local-recording.md), and [privacy](docs/privacy.md).

## Build and run

Use a stable Rust toolchain with rustfmt and clippy.

```sh
cargo build --workspace
cargo run -- serve
cargo run -- serve --bind 0.0.0.0:8080
```

Default binding is loopback port 8080. The API has no authentication in Phase 0;
choose wider binding deliberately. `GET /health` returns
`{"status":"ok","service":"ferrissight"}`. `GET /api/v1/cameras` returns `[]`.
Ctrl-C and SIGTERM initiate graceful shutdown.

CLI settings: `--bind`, `--data-dir` (recording root), `--log-level`,
`--retention-hours`, `--max-storage-bytes`, `--allow-unuploaded-eviction`, and
`--upload-enabled` (future uploader placeholder).
Environment equivalents: `FERRISSIGHT_BIND`, `FERRISSIGHT_DATA_DIR`,
`FERRISSIGHT_LOG_LEVEL`. Log levels are fixed levels, not arbitrary tracing filters.
TOML loading is deferred; [config example](examples/config.example.toml) contains
synthetic design values only. Logs omit configuration values and raw OS errors.
There is no telemetry or automatic media/cloud upload. The gateway reconciles
finalized UTC recordings independently and exposes aggregate status through
`GET /api/v1/storage`. See [local storage](docs/local-storage.md) for conservative
retention defaults, crash recovery and queue semantics.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
```

## License

Apache License 2.0 only. See [LICENSE](LICENSE).

For the minute-session lifecycle and validation limits, see [UTC minute recording](docs/utc-minute-recording.md).

For bounded minute-session versus persistent-session results, see
[session recording comparison](docs/session-recording-comparison.md).

Experimental recorded-media source preparation is documented in
[Recording sources](docs/recording-sources.md). The Tapo skeleton performs no network
operations and is not part of production recording or synchronization.

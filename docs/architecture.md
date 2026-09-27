# Architecture

FerrisSight is a single Rust gateway process. The six libraries establish small
boundaries, not independent services. The core has no HTTP, media backend, cloud,
UI, or database dependency.

```text
Camera/device layer
    ↓ vendor-neutral CameraProvider / CameraDevice
Protocol adapters (ONVIF, later vendor quirks)
    ↓ separate endpoint components and credentials
Media layer
    ↓ future local recording pipeline
Recording/storage
    ↓ gateway orchestration
Gateway services
    ↓
HTTP/API
    ↓
Future mobile/web clients
```

`ferrissight-core` owns generated IDs, redacted credentials, CameraInfo,
CameraCapabilities, StreamProfile, codecs, status and fixed domain errors.
CameraInfo names are sensitive local data; the public camera summary includes only
an ID. Capability flags represent discovered support, never guesses based on
model names. StreamProfile carries codec and resolution data, never stream URLs.

`ferrissight-camera` exposes object-safe async provider/device interfaces using
async-trait. An adapter discovers and connects; a device exposes information,
capabilities, profiles, a structured stream endpoint and health. Initial discovery
returns application camera records; richer discovery candidates and ID assignment
will be designed with the first real ONVIF implementation. Assigned IDs must later
be persisted independently of mutable network endpoints.

`ferrissight-onvif` is an explicit NotImplemented adapter boundary for
WS-Discovery, GetDeviceInformation, GetCapabilities, GetProfiles, GetStreamUri,
PTZ and event probes. Vendor quirks belong in adapters and capability probes.

`ferrissight-media` defines sources, backend open and stream stop. No encoded
packet interface is added until a concrete consumer needs it. Future go2rtc,
FFmpeg, GStreamer or retina adapters own process/library details; core does not.

`ferrissight-storage` defines generated recording IDs, minimal metadata and store
operations. It contains no database, media writing or cloud integration. Future
retention deletion must remove media as well as metadata. Uploads require explicit
configuration, minimum OAuth scopes and protected tokens.

`ferrissight-server` owns allowlisted HTTP responses and serves a listener with a
caller-provided shutdown future. The binary owns CLI/environment configuration,
privacy-filtered tracing, bind and Ctrl-C/SIGTERM shutdown. Local loopback is the
default; exposing an unauthenticated listener requires an explicit bind argument.
The data-directory option is reserved and unused until recording is implemented.

Every layer follows [privacy requirements](privacy.md). No raw transport errors
enter public responses or logs. Endpoint validation, credential storage and real
connection lifecycle handling are future implementation work, not working features.

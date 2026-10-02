# Privacy model

Privacy is an architectural constraint. Collect and persist only what runtime
operation requires, even for a single operator. Phase 0 has no telemetry,
analytics SDK, crash-reporting service, installation tracking, discovery,
recording, remote access, cloud uploads, or Google authentication.

## Identity and secrets

Use randomly generated FerrisSight camera IDs, never IP addresses, MAC addresses,
serial numbers, usernames, or names as identity. Network addresses are mutable
runtime state. Generated IDs are still linkable: expose only where needed and
never upload them as installation tracking. Future storage must preserve assigned
camera IDs across restarts.

Represent stream components and credentials separately. Never persist or log
credential-bearing URLs. `SecretString` has redacted Debug and no Display or
Serialize implementation; `CameraCredentials` redacts both fields, and endpoint
Debug redacts all network details. Explicit secret access belongs only at the
authentication boundary. This wrapper prevents accidental formatting; it does not
encrypt or erase memory. Future connection adapters must validate components and
avoid retaining or logging assembled credential-bearing URLs. Use established
cryptographic libraries when needed, never custom cryptography.

## Logs, errors, and metadata

Logs must be publicly shareable by default. Use structured event fields with
internal IDs (for example `camera_id=camera-1 connection_lost`) and fixed event
codes. Never log secrets, authentication headers, cookies, private keys, pairing
secrets, user-provided camera names, raw device identifiers, serial numbers,
filesystem paths, hostnames, network addresses, SSIDs, router details, account
information, or personally identifying data. Temporary address debugging requires
an explicit need and must not enter persistent diagnostics. Local errors obey the
same redaction rules. External errors exclude paths, URLs, tokens, stack traces,
and topology. Phase 0 startup errors are fixed messages only.

Health and camera API responses use allowlisted fields: status, a fixed service
name, and generated camera IDs only. Future authenticated display-name APIs require deliberate review;
names may identify people, rooms, or precise addresses and must not enter telemetry
or automatic crash reports. Metadata must omit source/configuration paths, build
identity, home paths, OS usernames, hardware identity, unnecessary cloud account
data, and unnecessary camera inventory details.

## Media, network, and cloud

Recording stays local by default. Remote access and cloud backup require explicit
configuration. Never send recordings, audio, thumbnails, screenshots, embeddings,
or video-derived identity/AI outputs to telemetry or third parties by default.
Temporary media must have a defined retention and removal policy. Keep discovery,
local topology, ONVIF results, MAC addresses, model inventories, hostnames, routers,
and SSIDs local unless needed for an explicit user-initiated operation. Remote
connection establishment discloses only necessary information.

Future cloud storage must request minimum OAuth scopes and store tokens in an OS
secret store or an appropriately encrypted local store. Never place account IDs,
OAuth/access/refresh tokens, API keys, sessions, or credentials in source, fixtures,
logs, or repository configuration. Google Drive authentication is deferred.
Any future telemetry must be opt-in, documented, minimal, privacy-preserving, and
independently disableable; camera content is never analytics input.

## Fixtures and release review

Create examples from scratch using documentation address ranges, example.invalid,
synthetic names, and placeholder secrets. Never sanitize copied production
configuration or commit private-home media. Ignore local configuration, .env files,
keys, credentials, secrets, build outputs, recordings, caches, logs, and dumps.
Synthetic examples remain tracked.

Before each release inspect package contents, generated metadata, archives, and
binaries for secrets, absolute paths, usernames, hostnames, machine identity,
local configs, caches, dumps, and test recordings. Workspace path remapping and
release stripping reduce path disclosure but do not replace artifact inspection;
dependency/compiler paths may require additional remapping in release tooling.

Every new subsystem must explicitly review exposure of personal information,
machine identity, local-network identity, camera credentials, cloud credentials,
recording content, filesystem paths, account information, and unique persistent
identifiers. Prefer collecting less. Recording retention, protected credential
storage, authenticated remote access, cloud authorization, discovery, and complete
release artifact scanning remain requirements for their future phases.

## Phase 1A probe boundary

The RTSP probe consumes separate endpoint components and redacted credentials.
It never formats dependency errors, SDP, frame payloads, or transport metadata.
The standalone runner disables dependency logging and replaces panic output with
a fixed message. Its anonymous stdin pipe accepts private runtime input; no
credential values are placed in command-line arguments, environment variables,
configuration examples or files. Reports contain only profile indices, codec
enums, codec-derived dimensions, frame-rate estimates, frame counts, audio-track
presence, readability and teardown results. Frames remain in memory and are
immediately discarded; there is no decoder, media sink, upload or recording.

Local testing can reuse an explicitly selected OS credential-store entry through
an ignored ONVIF helper. That helper and private device context are excluded from
Git and external reviews. Linux production credential-store integration and a
production Rust ONVIF adapter remain future work. Successful probing is not a
long-term reliability, full video-decoding or recording validation.

## Phase 1B recording boundary

Recording is an explicit local operation. The bounded example stores compressed
H.264 video only, using generated UUID/sequence filenames in an ignored directory.
Container metadata is limited to codec parameters and media timing; reports omit
paths, endpoints, names and accounts. No audio, thumbnails, embeddings, analytics
or uploads are produced. Recordings must never enter external agent reviews or
source packages. New Unix files request owner-only access; filesystem ACLs remain
an operator responsibility. Dependency logs and panic details are suppressed by
the example. There is no retention yet: explicitly created recordings remain
local until the operator removes them, including incomplete `.partial` files.

Cargo source packaging uses an anchored public-file allowlist in the root manifest.
Git ignores alone may not exclude local files when packaging from a checkout
without usable Git metadata. The Phase 1B workspace package listings were checked
to exclude agent guidance, internal working documents and test media. Binary
release path remapping and artifact scanning remain required before distribution.

Keepalive diagnostics expose only method enums, numeric success/fallback counts
and fixed failure categories. Reconnection does not retain endpoint identifiers,
session IDs or raw responses. The vendored dependency contains public production
source only; its original recorded-device tests are excluded. Dependency logging
must remain suppressed at every executable boundary handling real credentials.

## Phase 1C minute-session boundary

UTC paths use generated CameraId, calendar components and an exclusive sequence.
Sidecars persist only media properties, RTP intervals and explicitly labeled gateway
UTC observations. They contain no endpoints, local paths, source configuration,
accounts or device identifiers. Per-window diagnostics remain allowlisted; failed
windows never reveal dependency errors. Live media and test metrics remain local.
The example suppresses dependency logging and receives credentials only over an
anonymous pipe. No cloud service or telemetry is introduced.

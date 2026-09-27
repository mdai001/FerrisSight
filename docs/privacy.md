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

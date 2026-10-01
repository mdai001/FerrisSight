Retina 0.4.20, from the crates.io source distribution.
Upstream: https://github.com/scottlamb/retina
License: Apache-2.0 (selected from upstream dual license).
Production Rust source is retained. Upstream tests and recorded-device fixtures
are omitted for FerrisSight synthetic-fixture privacy requirements.
The local change is confined to client/mod.rs: an opt-in adaptive keepalive
policy (GET_PARAMETER preference and narrow rejection downgrade), OPTIONS-only
mode and safe numeric/enum metrics. Default upstream behavior remains Auto.
No client, parser, transport or codec is replaced.

Upstream crate SHA-256: 0e0eb740f743e678e071628ff6bf84ed5ed03df879997cc3d43b5afef64aff93
One upstream documentation address is replaced with an RFC5737 synthetic address.

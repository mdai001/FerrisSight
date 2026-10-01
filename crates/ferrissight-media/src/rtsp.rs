//! Shared generic RTSP policy. Never format dependency errors or transport diagnostics.
use ferrissight_core::StreamEndpoint;
use retina::client::{
    Credentials, KeepaliveMethod, KeepalivePolicy, KeepaliveStats, SessionGroup, SessionOptions,
    TeardownPolicy,
};
use serde::Serialize;
use std::sync::Arc;

pub(crate) fn session_options(
    endpoint: &StreamEndpoint,
    group: Arc<SessionGroup>,
) -> SessionOptions {
    SessionOptions::default()
        .session_group(group)
        .teardown(TeardownPolicy::Always)
        .keepalive_policy(KeepalivePolicy::Adaptive)
        .creds(Some(Credentials {
            username: endpoint.credentials.username.expose_secret().into(),
            password: endpoint.credentials.password.expose_secret().into(),
        }))
}
#[derive(Debug, Serialize)]
pub struct KeepaliveReport {
    pub options_succeeded: u64,
    pub get_parameter_succeeded: u64,
    pub set_parameter_succeeded: u64,
    pub fallbacks: u64,
    pub malformed_fallbacks: u64,
    pub last_successful_method: Option<&'static str>,
}
impl From<KeepaliveStats> for KeepaliveReport {
    fn from(s: KeepaliveStats) -> Self {
        Self {
            options_succeeded: s.options_succeeded,
            get_parameter_succeeded: s.get_parameter_succeeded,
            set_parameter_succeeded: s.set_parameter_succeeded,
            fallbacks: s.fallbacks,
            malformed_fallbacks: s.malformed_fallbacks,
            last_successful_method: s.last_successful_method.map(|m| match m {
                KeepaliveMethod::Options => "OPTIONS",
                KeepaliveMethod::GetParameter => "GET_PARAMETER",
                KeepaliveMethod::SetParameter => "SET_PARAMETER",
            }),
        }
    }
}

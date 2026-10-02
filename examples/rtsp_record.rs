//! Private runtime inputs arrive through an anonymous stdin pipe, never arguments/files.
use ferrissight::core::CameraId;
use ferrissight::core::{CameraCredentials, SecretString, StreamEndpoint};
use ferrissight::media::recording::{record_reconnecting, RecordingOptions};
use serde::{Deserialize, Deserializer};
use tokio::io::AsyncReadExt;

fn secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SecretString, D::Error> {
    String::deserialize(deserializer).map(SecretString::new)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    #[serde(deserialize_with = "secret")]
    username: SecretString,
    #[serde(deserialize_with = "secret")]
    password: SecretString,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    scheme: String,
    host: String,
    port: u16,
    path: String,
    credentials: Credentials,
}
impl Source {
    fn endpoint(self) -> StreamEndpoint {
        StreamEndpoint {
            scheme: self.scheme,
            host: self.host,
            port: self.port,
            path: self.path,
            credentials: CameraCredentials {
                username: self.credentials.username,
                password: self.credentials.password,
            },
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    sources: Vec<Source>,
    #[serde(default = "duration_default")]
    duration_seconds: u64,
    #[serde(default)]
    shutdown_after_seconds: Option<u64>,
    #[serde(default)]
    minute_sessions: bool,
    #[serde(default)]
    persistent_sessions: bool,
    #[serde(default)]
    profile_index: u8,
    #[serde(default)]
    udp: bool,
}
fn duration_default() -> u64 {
    120
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::OFF)
        .init();
    std::panic::set_hook(Box::new(|_| eprintln!("recording internal failure")));
    let mut bytes = Vec::new();
    let input = if tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::io::stdin().take(65537).read_to_end(&mut bytes),
    )
    .await
    .is_ok_and(|r| r.is_ok())
        && bytes.len() <= 65536
    {
        serde_json::from_slice::<Input>(&bytes).ok()
    } else {
        None
    };
    bytes.fill(0);
    let Some(input) =
        input.filter(|i| i.sources.len() == 1 && !(i.minute_sessions && i.persistent_sessions))
    else {
        eprintln!("invalid recording input");
        return std::process::ExitCode::FAILURE;
    };
    let endpoint = input.sources.into_iter().next().unwrap().endpoint();
    let shutdown = async {
        let timed = async {
            match input.shutdown_after_seconds {
                Some(s) => tokio::time::sleep(std::time::Duration::from_secs(s)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {_=process_shutdown()=>{},_=timed=>{}}
    };
    let options = RecordingOptions {
        duration: std::time::Duration::from_secs(input.duration_seconds),
        segment_seconds: 30,
    };
    let (passed, value) = if input.persistent_sessions {
        let transport = if input.udp {
            retina::client::Transport::Udp(Default::default())
        } else {
            Default::default()
        };
        let result = ferrissight::media::persistent::record_persistent(
            &endpoint,
            CameraId::generate(),
            std::path::Path::new("recordings"),
            ferrissight::media::persistent::PersistentRecordingOptions {
                duration: options.duration,
                transport,
            },
            shutdown,
            |_| {},
        )
        .await;
        let passed = matches!(&result, Ok(r) if !r.shutdown_requested && r.attempts.last()
            .and_then(|a| a.recording.as_ref()).is_some_and(|s| s.end == ferrissight::media::recording::RecordingEnd::Completed));
        let value = match result {
            Ok(report) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","persistent_run":report})
            }
            Err(error) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","error":error.to_string()})
            }
        };
        (passed, value)
    } else if input.minute_sessions {
        let transport = if input.udp {
            retina::client::Transport::Udp(Default::default())
        } else {
            Default::default()
        };
        let result=ferrissight::media::minute::record_minutes(
            &endpoint, CameraId::generate(), std::path::Path::new("recordings"),
            ferrissight::media::minute::MinuteRecordingOptions {duration:options.duration,transport},
            shutdown, |window| println!("{}",serde_json::json!({"mode":"recording","profile_index":input.profile_index,"minute_window":window})),
        ).await;
        let passed = matches!(&result,Ok(r) if r.failed_windows==0 && r.unattempted_windows==0 && !r.shutdown_requested);
        let value = match result {
            Ok(report) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","minute_run":report})
            }
            Err(error) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","error":error.to_string()})
            }
        };
        (passed, value)
    } else {
        let result = record_reconnecting(
            &endpoint,
            CameraId::generate(),
            std::path::Path::new("recordings"),
            options,
            shutdown,
        )
        .await;
        let passed = matches!(&result,Ok(r) if r.completed);
        let value = match result {
            Ok(report) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","report":report})
            }
            Err(error) => {
                serde_json::json!({"profile_index":input.profile_index,"mode":"recording","error":error.to_string()})
            }
        };
        (passed, value)
    };
    println!("{value}");
    if passed {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

async fn process_shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

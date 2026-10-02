//! Private runtime inputs arrive through an anonymous stdin pipe, never arguments/files.
use ferrissight::core::{CameraCredentials, SecretString, StreamEndpoint};
use ferrissight::media::probe::{probe, ProbeOptions};
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
}
async fn run(profile_index: usize, mode: &str, endpoint: &StreamEndpoint) -> bool {
    let result = probe(endpoint, ProbeOptions::default()).await;
    let passed = matches!(&result,Ok(r) if r.readable && r.clean_disconnect);
    let value = match result {
        Ok(report) => {
            serde_json::json!({"profile_index":profile_index,"mode":mode,"report":report})
        }
        Err(error) => {
            serde_json::json!({"profile_index":profile_index,"mode":mode,"error":error.to_string()})
        }
    };
    // Only explicitly allowlisted media properties and fixed error messages reach stdout.
    println!("{value}");
    passed
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Retina's optional diagnostics can contain URLs/SDP. This probe disables all logs.
    use tracing_subscriber::prelude::*;
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::OFF)
        .init();
    std::panic::set_hook(Box::new(|_| eprintln!("media probe internal failure")));
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
    let Some(input) = input.filter(|i| i.sources.len() == 2) else {
        eprintln!("invalid media probe input");
        return std::process::ExitCode::FAILURE;
    };
    let endpoints: Vec<_> = input.sources.into_iter().map(Source::endpoint).collect();
    let mut passed = true;
    for (i, endpoint) in endpoints.iter().enumerate() {
        passed &= run(i, "individual", endpoint).await;
    }
    // Fresh sessions after acknowledged teardown exercise disconnect/reconnect once.
    for (i, endpoint) in endpoints.iter().enumerate() {
        passed &= run(i, "reconnect", endpoint).await;
    }
    let (a, b) = tokio::join!(
        run(0, "simultaneous", &endpoints[0]),
        run(1, "simultaneous", &endpoints[1])
    );
    passed &= a && b;
    if passed {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

//! `linuxplay api`: one call to the running engine, from the command line.
//! Handy for scripts and for trying the API a front end will use.

use anyhow::{Context, Result, bail};
use linuxplay_proto::api::{Request, Response, method};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::service::socket_path;

/// Call `method` with `params` (JSON text) and print the result. After
/// `subscribe`, keep printing the engine's notifications until it goes away.
pub async fn call(name: &str, params: Option<&str>) -> Result<()> {
    let params: Value = match params {
        Some(text) => serde_json::from_str(text).context("the parameters are not valid JSON")?,
        None => Value::Null,
    };
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("no engine is running (start one with `linuxplay service`); tried {}", path.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let request = serde_json::to_string(&Request::new(1, name, params))?;
    writer.write_all(format!("{request}\n").as_bytes()).await?;
    let line = lines.next_line().await?.context("the engine closed the connection")?;
    let response: Response = serde_json::from_str(&line).context("the engine's answer made no sense")?;
    if let Some(error) = response.error {
        bail!(error.message);
    }
    println!("{}", serde_json::to_string_pretty(&response.result.unwrap_or(Value::Null))?);

    if name == method::SUBSCRIBE {
        while let Some(line) = lines.next_line().await? {
            let notification: Value = serde_json::from_str(&line)?;
            println!("{}", serde_json::to_string_pretty(&notification["params"])?);
        }
    }
    Ok(())
}

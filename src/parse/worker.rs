//! Out-of-process parsing.
//!
//! OpenBoardView's parsers are C++ code written for an interactive viewer: a
//! corrupt or unexpected file can crash them, and they change process-global
//! state (the C locale). The server therefore re-executes its own binary in
//! worker mode for every parse. A crash only kills the worker and is reported
//! as an ordinary error.
//!
//! Protocol: the server writes a JSON [`WorkerRequest`] to the worker's stdin
//! (keys never appear on the command line) and reads a JSON
//! [`WorkerResponse`] from its stdout. Parser log lines arrive on stderr.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::keys::DecryptionKeys;
use super::native;
use super::raw::RawBoard;

/// Hidden subcommand that runs the worker.
pub const WORKER_SUBCOMMAND: &str = "parse-worker";

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub path: PathBuf,
    pub keys: DecryptionKeys,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerResponse {
    Ok { board: RawBoard },
    Error { message: String },
}

/// A successful parse plus whatever the parser logged along the way.
#[derive(Debug)]
pub struct ParseOutcome {
    pub board: RawBoard,
    pub warnings: Vec<String>,
}

/// Entry point of the worker process.
pub fn run_worker() -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).context("reading worker request")?;
    let request: WorkerRequest = serde_json::from_slice(&input).context("decoding worker request")?;

    let response = match native::parse_file(&request.path, &request.keys) {
        Ok(board) => WorkerResponse::Ok { board },
        Err(e) => WorkerResponse::Error { message: format!("{e:#}") },
    };

    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response).context("writing worker response")?;
    stdout.flush()?;
    Ok(())
}

/// Parses `path` in a worker process.
pub async fn parse_in_worker(path: &Path, keys: &DecryptionKeys, timeout: Duration) -> Result<ParseOutcome> {
    let exe = std::env::current_exe().context("locating own executable")?;
    let request = serde_json::to_vec(&WorkerRequest { path: path.to_path_buf(), keys: keys.clone() })?;

    let mut child = Command::new(exe)
        .arg(WORKER_SUBCOMMAND)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting parse worker")?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let run = async {
        let write = async {
            stdin.write_all(&request).await?;
            drop(stdin); // EOF tells the worker the request is complete
            Ok::<_, std::io::Error>(())
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (w, o, e) = tokio::join!(write, stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        w.context("sending request to parse worker")?;
        o.context("reading parse worker output")?;
        e.context("reading parse worker log")?;
        let status = child.wait().await.context("waiting for parse worker")?;
        Ok::<_, anyhow::Error>((status, out, err))
    };

    let (status, out, err) = tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow!("parsing took longer than {}s and was aborted", timeout.as_secs()))??;

    let warnings: Vec<String> = String::from_utf8_lossy(&err)
        .lines()
        .map(|l| l.trim().trim_start_matches("[obv warn] ").trim_start_matches("[obv error] ").to_string())
        .filter(|l| !l.is_empty())
        .collect();
    for line in &warnings {
        tracing::debug!(target: "obv_parser", "{line}");
    }

    if !status.success() {
        bail!(
            "the OpenBoardView parser crashed while reading this file ({}). \
             The file is probably corrupt or uses an unsupported variant.{}",
            describe_exit(&status),
            warnings.last().map(|w| format!(" Last parser message: {w}")).unwrap_or_default()
        );
    }

    match serde_json::from_slice::<WorkerResponse>(&out).context("decoding parse worker output")? {
        WorkerResponse::Ok { board } => Ok(ParseOutcome { board, warnings }),
        WorkerResponse::Error { message } => Err(anyhow!(message)),
    }
}

fn describe_exit(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "abnormal exit".to_string(),
    }
}

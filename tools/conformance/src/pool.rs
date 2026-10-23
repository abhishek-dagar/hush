//! Worker pool: schedules tests across worker processes with timeouts and
//! retries (spec 0005, AC-8, AC-9).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

use crate::discover;
use crate::discover::Test;
use crate::status::Status;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ImagePaths {
    pub test: String,
    pub ref_: String,
    pub diff: String,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub test: Test,
    pub status: Status,
    pub message: String,
    pub retried: bool,
    pub duration_ms: u64,
    pub images: Option<ImagePaths>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    status: String,
    message: String,
    duration_ms: u64,
    images: Option<ImagePaths>,
    #[allow(dead_code, reason = "deserialized but not currently used")]
    parsed_as: String,
}

struct ChildHandles {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<Option<String>>,
}

fn spawn_child(wpt_dir: &Path, no_render: bool, subset_path: &Path) -> Result<ChildHandles, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("--worker")
        .arg("--wpt-dir")
        .arg(wpt_dir)
        .arg("--subset")
        .arg(subset_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if no_render {
        cmd.arg("--no-render");
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(Some(l)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });
    Ok(ChildHandles {
        child,
        stdin,
        rx,
    })
}

fn kill_child(handles: &mut ChildHandles) {
    let _ = handles.child.kill();
    let _ = handles.child.wait();
}

fn run_one(
    handles: &mut ChildHandles,
    test: &Test,
    timeout: Duration,
) -> Result<RunResult, String> {
    let req = serde_json::json!({"test": test.path});
    writeln!(handles.stdin, "{}", req).map_err(|e| e.to_string())?;
    handles.stdin.flush().map_err(|e| e.to_string())?;
    match handles.rx.recv_timeout(timeout) {
        Ok(Some(line)) => {
            let resp: WorkerResponse = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            let status = Status::parse(&resp.status).unwrap_or(Status::Error);
            Ok(RunResult {
                test: test.clone(),
                status,
                message: resp.message,
                retried: false,
                duration_ms: resp.duration_ms,
                images: resp.images,
            })
        }
        Ok(None) => Err("crash".to_owned()),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("timeout".to_owned()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("crash".to_owned()),
    }
}

/// Runs all tests, returning results in completion order.
pub fn run_pool(
    wpt_dir: &Path,
    no_render: bool,
    subset_path: &Path,
    jobs: usize,
    tests: Vec<Test>,
) -> Vec<RunResult> {
    let jobs = jobs.max(1);
    let queue = Arc::new(Mutex::new(VecDeque::from(tests)));
    let (tx, rx) = mpsc::channel::<RunResult>();
    let mut handles = Vec::new();
    for _ in 0..jobs {
        let queue = Arc::clone(&queue);
        let tx = tx.clone();
        let wpt_dir = wpt_dir.to_path_buf();
        let subset_path = subset_path.to_path_buf();
        let h = thread::spawn(move || {
            let mut child: Option<ChildHandles> = None;
            loop {
                let test = {
                    let mut q = queue.lock().expect("lock poisoned");
                    q.pop_front()
                };
                let Some(test) = test else { break; };
                let timeout = match test.timeout {
                    discover::Timeout::Normal => Duration::from_secs(10),
                    discover::Timeout::Long => Duration::from_secs(60),
                };
                // ensure child
                if child.is_none() {
                    match spawn_child(&wpt_dir, no_render, &subset_path) {
                        Ok(c) => child = Some(c),
                        Err(_) => {
                            let _ = tx.send(RunResult {
                                test: test.clone(),
                                status: Status::Error,
                                message: "could not spawn worker".to_owned(),
                                retried: false,
                                duration_ms: 0,
                                images: None,
                            });
                            continue;
                        }
                    }
                }
                let handles = child.as_mut().expect("child should exist after spawn");
                match run_one(handles, &test, timeout) {
                    Ok(res) => {
                        let _ = tx.send(res);
                    }
                    Err(kind) => {
                        let status = if kind == "timeout" {
                            Status::Timeout
                        } else {
                            Status::Crash
                        };
                        // kill child
                        if let Some(h) = child.as_mut() {
                            kill_child(h);
                        }
                        child = None;
                        let _ = tx.send(RunResult {
                            test: test.clone(),
                            status,
                            message: kind,
                            retried: false,
                            duration_ms: 0,
                            images: None,
                        });
                    }
                }
            }
            if let Some(h) = child.as_mut() {
                kill_child(h);
            }
        });
        handles.push(h);
    }
    drop(tx);
    let mut results = Vec::new();
    for r in rx {
        results.push(r);
    }
    for h in handles {
        let _ = h.join();
    }
    results
}

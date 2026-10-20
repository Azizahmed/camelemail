//! stdio transport: a child process speaking newline-delimited JSON-RPC.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::mcp::client::{handle_peer_message, PeerState};
use crate::mcp::{lock, protocol};

const STDERR_LINES: usize = 64;
const STDERR_LINE_MAX: usize = 400;
const STDERR_IN_ERRORS: usize = 20;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub(crate) struct StdioConn {
    label: String,
    state: Arc<PeerState>,
    pending: Pending,
    stderr: Arc<Mutex<VecDeque<String>>>,
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    child: tokio::sync::Mutex<Option<Child>>,
    /// The child's pid, which is also its process group id on unix.
    pid: Option<u32>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
}

impl StdioConn {
    pub(crate) async fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&str>,
        state: Arc<PeerState>,
    ) -> anyhow::Result<Self> {
        let (program, path) = {
            let command = command.to_string();
            tokio::task::spawn_blocking(move || prepare(&command)).await??
        };
        let mut cmd = Command::new(&program);
        cmd.args(args)
            .env("PATH", &path)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
            cmd.current_dir(cwd);
        }
        // Its own process group, so closing also stops what `npx`/`uvx`/shell wrappers start.
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().with_context(|| format!("failed to start `{command}`"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            bail!("failed to open pipes to `{command}`");
        };

        let label = command.to_string();
        let pid = child.id();
        let pending: Pending = Arc::default();
        let stderr_buf: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = rx.recv().await {
                let write = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if write.await.is_err() {
                    break;
                }
            }
        });

        let stderr_task = {
            let buf = stderr_buf.clone();
            let label = label.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("mcp[{label}] stderr: {line}");
                    let mut buf = lock(&buf);
                    if buf.len() == STDERR_LINES {
                        buf.pop_front();
                    }
                    buf.push_back(line.chars().take(STDERR_LINE_MAX).collect());
                }
            })
        };

        {
            let pending = pending.clone();
            let state = state.clone();
            let replies = tx.downgrade();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(text) else {
                        tracing::debug!("mcp: ignoring non-JSON stdout line: {text}");
                        continue;
                    };
                    for msg in protocol::messages(value) {
                        if protocol::kind(&msg) == protocol::Kind::Response {
                            let waiter = protocol::numeric_id(&msg).and_then(|id| lock(&pending).remove(&id));
                            if let Some(waiter) = waiter {
                                let _ = waiter.send(msg);
                            }
                        } else if let Some(reply) = handle_peer_message(&state, &msg) {
                            if let Some(tx) = replies.upgrade() {
                                let _ = tx.send(reply.to_string());
                            }
                        }
                    }
                }
                state.alive.store(false, Ordering::SeqCst);
                lock(&pending).clear();
            });
        }

        Ok(Self {
            label,
            state,
            pending,
            stderr: stderr_buf,
            writer: Mutex::new(Some(tx)),
            child: tokio::sync::Mutex::new(Some(child)),
            pid,
            stderr_task: Mutex::new(Some(stderr_task)),
        })
    }

    fn send(&self, msg: &Value) -> anyhow::Result<()> {
        let writer = lock(&self.writer);
        match writer.as_ref() {
            Some(tx) if self.state.alive.load(Ordering::SeqCst) => {
                tx.send(msg.to_string()).map_err(|_| self.closed_error())
            }
            _ => Err(self.closed_error()),
        }
    }

    pub(crate) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.state.next_id();
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        if let Err(e) = self.send(&protocol::request(id, method, params)) {
            lock(&self.pending).remove(&id);
            return Err(e);
        }
        // Dropped mid-wait (a stopped chat): forget the request and tell the server.
        let mut guard = InFlight { conn: self, id, reason: Some("cancelled") };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(msg)) => {
                guard.reason = None;
                protocol::into_result(msg)
            }
            Ok(Err(_)) => {
                guard.reason = None;
                Err(self.closed_error())
            }
            Err(_) => {
                guard.reason = Some("timeout");
                bail!("`{method}` timed out after {}s", timeout.as_secs())
            }
        }
    }

    pub(crate) fn notify(&self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        self.send(&protocol::notification(method, params))
    }

    fn closed_error(&self) -> anyhow::Error {
        match self.stderr_tail() {
            Some(tail) => anyhow!("MCP server `{}` exited\n{tail}", self.label),
            None => anyhow!("MCP server `{}` exited", self.label),
        }
    }

    /// The last stderr lines, formatted for an error message.
    pub(crate) fn stderr_tail(&self) -> Option<String> {
        let buf = lock(&self.stderr);
        if buf.is_empty() {
            return None;
        }
        let skip = buf.len().saturating_sub(STDERR_IN_ERRORS);
        let lines: Vec<&str> = buf.iter().skip(skip).map(String::as_str).collect();
        Some(format!("--- server stderr ---\n{}", lines.join("\n")))
    }

    /// Waits briefly for a dead process's stderr to drain, then returns its tail.
    pub(crate) async fn failure_detail(&self) -> Option<String> {
        if !self.state.alive.load(Ordering::SeqCst) {
            let task = lock(&self.stderr_task).take();
            if let Some(task) = task {
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            }
        }
        self.stderr_tail()
    }

    /// Closes stdin and gives the server a second to exit; then (on unix) sends its
    /// process group SIGTERM, and SIGKILL a second later if anything is left.
    pub(crate) async fn close(&self) {
        self.state.alive.store(false, Ordering::SeqCst);
        lock(&self.writer).take();
        lock(&self.pending).clear();
        let Some(mut child) = self.child.lock().await.take() else { return };
        let exited = tokio::time::timeout(Duration::from_secs(1), child.wait()).await.is_ok();
        // The group outlives the child if it left processes behind (npx exits, its server doesn't).
        #[cfg(unix)]
        if let Some(group) = self.pid.and_then(process_group) {
            signal_group(group, libc::SIGTERM);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            loop {
                let _ = child.try_wait();
                if !group_alive(group) {
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            signal_group(group, libc::SIGKILL);
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            return;
        }
        if !exited {
            let _ = child.kill().await;
        }
    }
}

impl Drop for StdioConn {
    /// A connection dropped without [`close`](Self::close) kills its whole process group.
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.child.get_mut().is_some() {
            if let Some(group) = self.pid.and_then(process_group) {
                signal_group(group, libc::SIGKILL);
            }
        }
    }
}

/// A request awaiting its response; dropping it unanswered removes the waiter and sends `notifications/cancelled`.
struct InFlight<'a> {
    conn: &'a StdioConn,
    id: u64,
    /// Why it is being abandoned, or None once answered.
    reason: Option<&'static str>,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let Some(reason) = self.reason else { return };
        lock(&self.conn.pending).remove(&self.id);
        let _ = self.conn.notify("notifications/cancelled", Some(cancel_params(self.id, reason)));
    }
}

pub(crate) fn cancel_params(id: u64, reason: &str) -> Value {
    serde_json::json!({ "requestId": id, "reason": reason })
}

/// The process group a child started with `process_group(0)` leads.
#[cfg(unix)]
fn process_group(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 1)
}

#[cfg(unix)]
fn signal_group(group: libc::pid_t, signal: libc::c_int) {
    // SAFETY: kill(2) with a negative pid signals that process group; no memory is involved.
    unsafe {
        libc::kill(-group, signal);
    }
}

/// Whether any process of the group still exists.
#[cfg(unix)]
fn group_alive(group: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(-group, 0) == 0 }
}

/// Resolves `command` against an augmented PATH; returns the program and that PATH.
fn prepare(command: &str) -> anyhow::Result<(PathBuf, OsString)> {
    let path = augmented_path(std::env::var_os("PATH").as_deref(), dirs::home_dir().as_deref());
    let program = resolve_command(command, &path).ok_or_else(|| {
        anyhow!(
            "command not found: `{command}` (searched PATH: {})",
            std::env::split_paths(&path).map(|p| p.display().to_string()).collect::<Vec<_>>().join(":")
        )
    })?;
    Ok((program, path))
}

/// PATH with common user tool directories prepended and system directories
/// appended, for desktop sessions that start apps with a minimal PATH.
pub(crate) fn augmented_path(current: Option<&OsStr>, home: Option<&Path>) -> OsString {
    let existing: Vec<PathBuf> = current.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    let mut user_dirs = Vec::new();
    if let Some(home) = home {
        for rel in
            [".local/bin", ".cargo/bin", ".bun/bin", ".deno/bin", ".volta/bin", ".npm-global/bin", ".local/share/pnpm"]
        {
            user_dirs.push(home.join(rel));
        }
        if let Some(nvm) = nvm_bin(home) {
            user_dirs.push(nvm);
        }
    }
    let system_dirs = ["/usr/local/bin", "/opt/homebrew/bin", "/snap/bin", "/usr/bin", "/bin"].map(PathBuf::from);
    let wanted = |d: &PathBuf| d.is_dir() && !existing.contains(d);
    let merged: Vec<PathBuf> = user_dirs
        .iter()
        .filter(|d| wanted(d))
        .cloned()
        .chain(existing.iter().cloned())
        .chain(system_dirs.iter().filter(|d| wanted(d)).cloned())
        .collect();
    std::env::join_paths(merged).unwrap_or_else(|_| current.map(OsStr::to_os_string).unwrap_or_default())
}

/// The bin dir of nvm's active or default node, falling back to the newest install.
fn nvm_bin(home: &Path) -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os("NVM_BIN").map(PathBuf::from).filter(|p| p.is_dir()) {
        return Some(bin);
    }
    let versions_dir = home.join(".nvm/versions/node");
    let mut versions: Vec<(Vec<u64>, String)> = std::fs::read_dir(&versions_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let parts =
                name.trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect::<Option<Vec<u64>>>()?;
            Some((parts, name))
        })
        .collect();
    versions.sort();
    let alias = std::fs::read_to_string(home.join(".nvm/alias/default")).unwrap_or_default();
    let alias = alias.trim().trim_start_matches('v');
    let pick = versions
        .iter()
        .rev()
        .find(|(_, name)| {
            let v = name.trim_start_matches('v');
            !alias.is_empty() && (v == alias || v.starts_with(&format!("{alias}.")))
        })
        .or_else(|| versions.last())?;
    Some(versions_dir.join(&pick.1).join("bin"))
}

/// Finds an executable for `command` (paths are used as given).
pub(crate) fn resolve_command(command: &str, path: &OsStr) -> Option<PathBuf> {
    let as_path = Path::new(command);
    if as_path.components().count() > 1 || as_path.is_absolute() {
        return Some(as_path.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(path).find_map(|dir| {
        exts.iter().map(|ext| dir.join(format!("{command}{ext}"))).find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn close_stops_the_whole_process_group() {
        let state = Arc::new(PeerState::new());
        let args = vec!["-c".to_string(), "sleep 300 & echo \"bg=$!\" >&2; wait".to_string()];
        let conn = StdioConn::spawn("sh", &args, &BTreeMap::new(), None, state).await.expect("spawn");
        let mut sleeper = None;
        for _ in 0..100 {
            sleeper = conn.stderr_tail().and_then(|t| t.lines().find_map(|l| l.strip_prefix("bg=")?.parse::<i32>().ok()));
            if sleeper.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let sleeper = sleeper.expect("background pid");
        conn.close().await;
        // Gone, or a zombie waiting for init to reap it.
        let mut gone = false;
        for _ in 0..100 {
            let stat = std::fs::read_to_string(format!("/proc/{sleeper}/stat")).unwrap_or_default();
            let state = stat.rsplit_once(") ").and_then(|(_, rest)| rest.chars().next());
            if stat.is_empty() || state == Some('Z') {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        if !gone {
            // SAFETY: kill(2) on a pid this test started.
            unsafe { libc::kill(sleeper, libc::SIGKILL) };
        }
        assert!(gone, "background process {sleeper} survived close()");
    }

    #[tokio::test]
    async fn dropped_requests_are_forgotten() {
        let state = Arc::new(PeerState::new());
        let args = vec!["-c".to_string(), "cat >/dev/null".to_string()];
        let conn = StdioConn::spawn("sh", &args, &BTreeMap::new(), None, state).await.expect("spawn");
        let request = conn.request("tools/call", None, Duration::from_secs(30));
        assert!(tokio::time::timeout(Duration::from_millis(100), request).await.is_err());
        assert!(lock(&conn.pending).is_empty());
        conn.close().await;
    }

    #[test]
    fn resolves_commands_via_augmented_path() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let tool = bin.join("fake-tool");
        std::fs::write(&tool, "#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let nvm = home.path().join(".nvm/versions/node");
        for v in ["v18.20.0", "v22.3.0", "v20.1.0"] {
            std::fs::create_dir_all(nvm.join(v).join("bin")).expect("mkdir");
        }
        let path = augmented_path(Some(OsStr::new("/nonexistent")), Some(home.path()));
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs[0], bin);
        assert!(dirs.contains(&PathBuf::from("/nonexistent")));
        if std::env::var_os("NVM_BIN").is_none() {
            assert!(dirs.contains(&nvm.join("v22.3.0/bin")));
        }
        assert_eq!(resolve_command("fake-tool", &path), Some(tool));
        assert_eq!(resolve_command("definitely-missing-tool", &path), None);
        assert_eq!(resolve_command("/abs/x", &path), Some(PathBuf::from("/abs/x")));
    }
}

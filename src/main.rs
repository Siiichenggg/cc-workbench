use anyhow::{anyhow, Result};
use chrono::Utc;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use ratatui::backend::CrosstermBackend;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn main() -> Result<()> {
    let mut args: Vec<String> = env::args().collect();
    let claude_args = if let Some(pos) = args.iter().position(|a| a == "--") {
        args.split_off(pos + 1)
    } else {
        Vec::new()
    };

    let workspace = env::current_dir()?;
    let config = Config::load(&workspace)?;

    let data_dir = workspace.join(".cc-workbench");
    fs::create_dir_all(&data_dir)?;

    let db_path = data_dir.join("ccwb.sqlite");
    let mut db = Database::new(&db_path)?;
    let workspace_id = db.ensure_workspace(&workspace)?;
    let session_id = db.create_session(&workspace_id)?;

    let snapshot_manager = SnapshotManager::new(&workspace, &data_dir)?;

    let (output_tx, output_rx) = mpsc::channel::<OutputChunk>();
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<SnapshotResult>();
    let (snapshot_job_tx, snapshot_job_rx) = mpsc::channel::<SnapshotJob>();

    spawn_snapshot_worker(snapshot_manager.clone(), snapshot_job_rx, snapshot_tx);

    let mut pty = PtyProcess::spawn(&config.claude_cmd, &claude_args, output_tx)?;

    let mut app = App::new(config, session_id, snapshot_manager, snapshot_job_tx);

    let mut terminal = setup_terminal()?;
    let res = run_app(&mut terminal, &mut pty, &mut db, &mut app, output_rx, snapshot_rx);
    restore_terminal(&mut terminal)?;
    res
}

#[derive(Clone)]
struct Config {
    claude_cmd: String,
    context_limit: u32,
    compress_threshold: f32,
    usage_poll_seconds: u64,
    providers: Vec<ProviderConfig>,
}

impl Config {
    fn load(workspace: &Path) -> Result<Self> {
        let claude_cmd = match env::var("CCWB_CLAUDE_CMD") {
            Ok(val) => val,
            Err(_) => detect_claude_cmd().unwrap_or_else(|| "claude".to_string()),
        };
        let mut context_limit = 200_000;
        let mut compress_threshold = 0.85;
        let mut providers: Vec<ProviderConfig> = Vec::new();
        let mut usage_poll_seconds = 30;

        if let Some(file) = load_config_file(workspace) {
            if let Some(val) = file.context_limit {
                context_limit = val;
            }
            if let Some(val) = file.compress_threshold {
                compress_threshold = val;
            }
            if let Some(list) = file.providers {
                providers = list;
            }
            if let Some(val) = file.usage_poll_seconds {
                usage_poll_seconds = val;
            }
        }

        if providers.is_empty() {
            providers.push(ProviderConfig::Local {
                name: Some("local-estimate".to_string()),
                limit_tokens: Some(context_limit as u64),
            });
        }
        Ok(Self {
            claude_cmd,
            context_limit,
            compress_threshold,
            usage_poll_seconds,
            providers,
        })
    }
}

fn detect_claude_cmd() -> Option<String> {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("claude.real");
            if candidate.exists() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }
    None
}

#[derive(Deserialize)]
struct ConfigFile {
    context_limit: Option<u32>,
    compress_threshold: Option<f32>,
    usage_poll_seconds: Option<u64>,
    providers: Option<Vec<ProviderConfig>>,
}

#[derive(Deserialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ProviderConfig {
    Local {
        name: Option<String>,
        limit_tokens: Option<u64>,
    },
    Manual {
        name: String,
        limit_tokens: u64,
        used_tokens: u64,
    },
    HttpJson {
        name: String,
        url: String,
        method: Option<String>,
        headers: Option<HashMap<String, String>>,
        body: Option<serde_json::Value>,
        used_pointer: String,
        limit_pointer: String,
    },
}

fn load_config_file(workspace: &Path) -> Option<ConfigFile> {
    let workspace_path = workspace.join(".cc-workbench").join("config.json");
    if let Ok(contents) = fs::read_to_string(&workspace_path) {
        if let Ok(parsed) = serde_json::from_str::<ConfigFile>(&contents) {
            return Some(parsed);
        }
    }
    if let Ok(home) = env::var("HOME") {
        let home_path = Path::new(&home).join(".cc-workbench").join("config.json");
        if let Ok(contents) = fs::read_to_string(&home_path) {
            if let Ok(parsed) = serde_json::from_str::<ConfigFile>(&contents) {
                return Some(parsed);
            }
        }
    }
    None
}

#[derive(Clone)]
struct SnapshotManager {
    workspace: PathBuf,
    git_dir: PathBuf,
    backup_dir: PathBuf,
}

impl SnapshotManager {
    fn new(workspace: &Path, data_dir: &Path) -> Result<Self> {
        let git_dir = data_dir.join("snapshots.git");
        let backup_dir = data_dir.join("backup");
        fs::create_dir_all(&backup_dir)?;
        if !git_dir.exists() {
            run_git_bare(&git_dir, &["init", "--bare"], None)?;
        }
        Ok(Self {
            workspace: workspace.to_path_buf(),
            git_dir,
            backup_dir,
        })
    }

    fn snapshot(&self, message_idx: i64) -> Result<String> {
        run_git(
            &self.workspace,
            &self.git_dir,
            &["add", "-A", "--", ".", ":(exclude).cc-workbench"],
            None,
        )?;
        let msg = format!("snapshot {}", message_idx);
        run_git(
            &self.workspace,
            &self.git_dir,
            &["-c", "user.name=ccwb", "-c", "user.email=ccwb@local", "commit", "-m", &msg, "--allow-empty"],
            None,
        )?;
        let commit = run_git(
            &self.workspace,
            &self.git_dir,
            &["rev-parse", "HEAD"],
            None,
        )?;
        Ok(commit.trim().to_string())
    }

    fn diff_preview(&self, commit: &str) -> Result<String> {
        let diff = run_git(
            &self.workspace,
            &self.git_dir,
            &["diff", commit, "--"],
            None,
        )?;
        Ok(diff)
    }

    fn diff_name_status(&self, commit: &str) -> Result<String> {
        let diff = run_git(
            &self.workspace,
            &self.git_dir,
            &["diff", "--name-status", commit, "--"],
            None,
        )?;
        Ok(diff)
    }

    fn restore(&self, commit: &str) -> Result<()> {
        let status = self.diff_name_status(commit)?;
        let files = parse_name_status(&status);
        let backup_dir = self.backup_dir.join(Utc::now().format("%Y%m%dT%H%M%S").to_string());
        fs::create_dir_all(&backup_dir)?;
        for entry in &files {
            let src = self.workspace.join(&entry.path);
            if src.exists() {
                let dst = backup_dir.join(&entry.path);
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&src, &dst)?;
            }
        }

        run_git(
            &self.workspace,
            &self.git_dir,
            &["checkout", commit, "--", "."],
            None,
        )?;

        for entry in &files {
            if entry.status == 'A' {
                let target = self.workspace.join(&entry.path);
                if target.exists() {
                    let _ = fs::remove_file(&target);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct NameStatusEntry {
    status: char,
    path: String,
}

fn parse_name_status(input: &str) -> Vec<NameStatusEntry> {
    input
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let status = parts.next()?.chars().next()?;
            let path = parts.next()?.to_string();
            Some(NameStatusEntry { status, path })
        })
        .collect()
}

fn run_git(workspace: &Path, git_dir: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg(format!("--work-tree={}", workspace.display()))
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data)?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_git_bare(git_dir: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg(format!("--git-dir={}", git_dir.display()))
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    let mut child = cmd.spawn()?;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(data)?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Clone)]
struct UsageEntry {
    name: String,
    used: Option<u64>,
    limit: Option<u64>,
    status: Option<String>,
}

#[derive(Clone)]
enum ProviderState {
    Local { name: String, limit: u64 },
    Manual { name: String, used: u64, limit: u64 },
    HttpJson {
        config: HttpJsonConfig,
        last: Option<UsageEntry>,
        last_error: Option<String>,
    },
}

#[derive(Clone)]
struct HttpJsonConfig {
    name: String,
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body: Option<serde_json::Value>,
    used_pointer: String,
    limit_pointer: String,
}

struct UsageManager {
    state: Arc<Mutex<Vec<ProviderState>>>,
    poll_seconds: u64,
}

impl UsageManager {
    fn new(config: &Config) -> Self {
        let mut providers: Vec<ProviderState> = Vec::new();
        for cfg in &config.providers {
            match cfg {
                ProviderConfig::Local { name, limit_tokens } => {
                    providers.push(ProviderState::Local {
                        name: name.clone().unwrap_or_else(|| "local-estimate".to_string()),
                        limit: limit_tokens.unwrap_or(config.context_limit as u64),
                    });
                }
                ProviderConfig::Manual { name, limit_tokens, used_tokens } => {
                    providers.push(ProviderState::Manual {
                        name: name.clone(),
                        used: *used_tokens,
                        limit: *limit_tokens,
                    });
                }
                ProviderConfig::HttpJson {
                    name,
                    url,
                    method,
                    headers,
                    body,
                    used_pointer,
                    limit_pointer,
                } => {
                    providers.push(ProviderState::HttpJson {
                        config: HttpJsonConfig {
                            name: name.clone(),
                            url: url.clone(),
                            method: method.clone().unwrap_or_else(|| "GET".to_string()),
                            headers: headers.clone().unwrap_or_default(),
                            body: body.clone(),
                            used_pointer: used_pointer.clone(),
                            limit_pointer: limit_pointer.clone(),
                        },
                        last: None,
                        last_error: None,
                    });
                }
            }
        }
        let state = Arc::new(Mutex::new(providers));
        let manager = Self {
            state: Arc::clone(&state),
            poll_seconds: config.usage_poll_seconds,
        };
        manager.spawn_pollers();
        manager
    }

    fn spawn_pollers(&self) {
        let state = Arc::clone(&self.state);
        let poll = self.poll_seconds.max(5);
        thread::spawn(move || {
            loop {
                let configs = {
                    let guard = state.lock().ok();
                    guard
                        .map(|g| {
                            g.iter()
                                .enumerate()
                                .filter_map(|(idx, p)| match p {
                                    ProviderState::HttpJson { config, .. } => Some((idx, config.clone())),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };

                for (idx, cfg) in configs {
                    let result = fetch_http_usage(&cfg);
                    if let Ok(mut guard) = state.lock() {
                        if let Some(state_entry) = guard.get_mut(idx) {
                            if let ProviderState::HttpJson { last, last_error, .. } = state_entry {
                                match result {
                                    Ok(entry) => {
                                        *last = Some(entry);
                                        *last_error = None;
                                    }
                                    Err(err) => {
                                        *last_error = Some(err);
                                    }
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_secs(poll));
            }
        });
    }

    fn entries(&self, context_tokens: u64) -> Vec<UsageEntry> {
        let mut out = Vec::new();
        if let Ok(guard) = self.state.lock() {
            for provider in guard.iter() {
                match provider {
                    ProviderState::Local { name, limit } => out.push(UsageEntry {
                        name: name.clone(),
                        used: Some(context_tokens),
                        limit: Some(*limit),
                        status: None,
                    }),
                    ProviderState::Manual { name, used, limit } => out.push(UsageEntry {
                        name: name.clone(),
                        used: Some(*used),
                        limit: Some(*limit),
                        status: None,
                    }),
                    ProviderState::HttpJson { config, last, last_error } => {
                        if let Some(entry) = last.clone() {
                            out.push(entry);
                        } else {
                            out.push(UsageEntry {
                                name: config.name.clone(),
                                used: None,
                                limit: None,
                                status: last_error.clone().or_else(|| Some("loading".to_string())),
                            });
                        }
                    }
                }
            }
        }
        out
    }
}

fn fetch_http_usage(cfg: &HttpJsonConfig) -> Result<UsageEntry, String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.arg("-sS").arg("-f").arg("-X").arg(&cfg.method).arg(&cfg.url);
    for (k, v) in &cfg.headers {
        cmd.arg("-H").arg(format!("{}: {}", k, v));
    }
    if let Some(body) = &cfg.body {
        cmd.arg("-H").arg("Content-Type: application/json");
        cmd.arg("-d").arg(body.to_string());
    }
    let output = cmd.output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let used = extract_u64(&json, &cfg.used_pointer)?;
    let limit = extract_u64(&json, &cfg.limit_pointer)?;
    Ok(UsageEntry {
        name: cfg.name.clone(),
        used: Some(used),
        limit: Some(limit),
        status: None,
    })
}

fn extract_u64(value: &serde_json::Value, pointer: &str) -> Result<u64, String> {
    let node = value
        .pointer(pointer)
        .ok_or_else(|| format!("missing {}", pointer))?;
    match node {
        serde_json::Value::Number(n) => n.as_u64().ok_or_else(|| "not u64".to_string()),
        serde_json::Value::String(s) => s.parse::<u64>().map_err(|_| "not u64".to_string()),
        _ => Err("not u64".to_string()),
    }
}

#[derive(Clone)]
struct SnapshotJob {
    message_id: String,
    message_idx: i64,
}

#[derive(Clone)]
struct SnapshotResult {
    message_id: String,
    commit: Option<String>,
}

fn spawn_snapshot_worker(
    manager: SnapshotManager,
    rx: Receiver<SnapshotJob>,
    tx: Sender<SnapshotResult>,
) {
    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            let result = manager.snapshot(job.message_idx);
            let res = match result {
                Ok(commit) => SnapshotResult {
                    message_id: job.message_id,
                    commit: Some(commit),
                },
                Err(_err) => SnapshotResult {
                    message_id: job.message_id,
                    commit: None,
                },
            };
            let _ = tx.send(res);
        }
    });
}

#[derive(Clone)]
struct MessageEntry {
    id: String,
    idx: i64,
    content: String,
    output_line: usize,
    assistant_text: String,
    snapshot_commit: Option<String>,
}

struct App {
    config: Config,
    session_id: String,
    messages: Vec<MessageEntry>,
    output_lines: Vec<String>,
    output_scroll: usize,
    follow_output: bool,
    input_buffer: String,
    focus: Focus,
    selected_message: usize,
    diff_preview: Option<DiffPreview>,
    usage_manager: UsageManager,
    snapshot_job_tx: Sender<SnapshotJob>,
    snapshot_manager: SnapshotManager,
    dirty: bool,
}

#[derive(Clone, Copy)]
enum Focus {
    Output,
    History,
}

struct DiffPreview {
    title: String,
    lines: Vec<String>,
    scroll: usize,
    pending_restore: Option<String>,
}

impl App {
    fn new(
        config: Config,
        session_id: String,
        snapshot_manager: SnapshotManager,
        snapshot_job_tx: Sender<SnapshotJob>,
    ) -> Self {
        Self {
            usage_manager: UsageManager::new(&config),
            config,
            session_id,
            messages: Vec::new(),
            output_lines: vec![String::new()],
            output_scroll: 0,
            follow_output: true,
            input_buffer: String::new(),
            focus: Focus::Output,
            selected_message: 0,
            diff_preview: None,
            snapshot_job_tx,
            snapshot_manager,
            dirty: true,
        }
    }

    fn handle_output(&mut self, chunk: OutputChunk) {
        let cleaned = strip_ansi(&chunk.text);
        // Only mark as dirty if there's actual content
        if !cleaned.is_empty() {
            append_output_lines(&mut self.output_lines, &cleaned);
            if let Some(last) = self.messages.last_mut() {
                last.assistant_text.push_str(&cleaned);
            }
            if self.follow_output {
                let total_lines = self.output_lines.len();
                self.output_scroll = total_lines.saturating_sub(1);
            }
            self.dirty = true;
        }
    }

    fn estimate_context_tokens(&self) -> u32 {
        let mut total = 0u32;
        for msg in &self.messages {
            total += estimate_tokens(&msg.content);
            total += estimate_tokens(&msg.assistant_text);
        }
        total
    }

    fn record_user_message(&mut self, db: &mut Database, content: String, output_line: usize) -> Result<()> {
        let idx = self.messages.len() as i64 + 1;
        let message_id = db.insert_message(&self.session_id, idx, &content)?;
        let entry = MessageEntry {
            id: message_id.clone(),
            idx,
            content,
            output_line,
            assistant_text: String::new(),
            snapshot_commit: None,
        };
        self.messages.push(entry);
        self.selected_message = self.messages.len().saturating_sub(1);
        let _ = self.snapshot_job_tx.send(SnapshotJob {
            message_id,
            message_idx: idx,
        });
        Ok(())
    }

    fn update_snapshot(&mut self, db: &mut Database, res: SnapshotResult) -> Result<()> {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.id == res.message_id) {
            if let Some(commit) = res.commit.clone() {
                msg.snapshot_commit = Some(commit.clone());
                db.insert_snapshot(&self.session_id, msg.idx, &commit)?;
            }
        }
        Ok(())
    }
}

struct Database {
    conn: Connection,
}

impl Database {
    fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS workspaces (
                id TEXT PRIMARY KEY,
                path TEXT UNIQUE,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                workspace_id TEXT,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                idx INTEGER,
                role TEXT,
                content TEXT,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                idx INTEGER,
                [commit] TEXT,
                created_at TEXT
            );
            ",
        )?;
        Ok(())
    }

    fn ensure_workspace(&mut self, path: &Path) -> Result<String> {
        let path_str = path.to_string_lossy();
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM workspaces WHERE path = ?1")?;
        let mut rows = stmt.query(params![path_str.as_ref()])?;
        if let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            return Ok(id);
        }
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO workspaces (id, path, created_at) VALUES (?1, ?2, ?3)",
            params![id, path_str.as_ref(), now],
        )?;
        Ok(id)
    }

    fn create_session(&mut self, workspace_id: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, workspace_id, created_at) VALUES (?1, ?2, ?3)",
            params![id, workspace_id, now],
        )?;
        Ok(id)
    }

    fn insert_message(&mut self, session_id: &str, idx: i64, content: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO messages (id, session_id, idx, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, session_id, idx, "user", content, now],
        )?;
        Ok(id)
    }

    fn insert_snapshot(&mut self, session_id: &str, idx: i64, commit: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO snapshots (id, session_id, idx, [commit], created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, session_id, idx, commit, now],
        )?;
        Ok(id)
    }
}

struct PtyProcess {
    master: Box<dyn portable_pty::MasterPty>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send>,
}

impl PtyProcess {
    fn spawn(cmd: &str, args: &[String], output_tx: Sender<OutputChunk>) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut command = CommandBuilder::new(cmd);
        for arg in args {
            command.arg(arg);
        }
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = output_tx.send(OutputChunk { text });
                    }
                    Err(_) => break,
                }
            }
        });

        let writer = pair.master.take_writer()?;
        Ok(Self {
            master: pair.master,
            writer,
            _child: child,
        })
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }
}

#[derive(Clone)]
struct OutputChunk {
    text: String,
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    pty: &mut PtyProcess,
    db: &mut Database,
    app: &mut App,
    output_rx: Receiver<OutputChunk>,
    snapshot_rx: Receiver<SnapshotResult>,
) -> Result<()> {
    let mut last_tick = Instant::now();
    let mut last_left: Rect = Rect::default();
    loop {
        let size = terminal.size()?;
        let left = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(size)[0];
        if left != last_left {
            let cols = left.width.saturating_sub(2);
            let rows = left.height.saturating_sub(2);
            pty.resize(cols, rows);
            last_left = left;
            app.dirty = true;
        }

        // Only redraw if there's something to update
        if app.dirty {
            terminal.draw(|f| draw_ui(f, app))?;
            app.dirty = false;
        }

        while let Ok(chunk) = output_rx.try_recv() {
            app.handle_output(chunk);
        }
        while let Ok(res) = snapshot_rx.try_recv() {
            app.update_snapshot(db, res)?;
            app.dirty = true;
        }

        let timeout = Duration::from_millis(50);
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    app.dirty = true;  // Mark dirty on any key event
                    if handle_key_event(key, pty, db, app)? {
                        break;
                    }
                }
                Event::Resize(cols, rows) => {
                    pty.resize(cols, rows);
                    app.dirty = true;
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= Duration::from_millis(200) {
            last_tick = Instant::now();
        }
    }
    Ok(())
}

fn handle_key_event(key: KeyEvent, pty: &mut PtyProcess, db: &mut Database, app: &mut App) -> Result<bool> {
    if app.diff_preview.is_some() {
        return handle_diff_keys(key, app);
    }

    match key {
        KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => return Ok(true),
        KeyEvent {
            code: KeyCode::Tab,
            ..
        } => {
            app.focus = match app.focus {
                Focus::Output => Focus::History,
                Focus::History => Focus::Output,
            };
        }
        KeyEvent {
            code: KeyCode::Up,
            ..
        } => {
            if matches!(app.focus, Focus::History) {
                if app.selected_message > 0 {
                    app.selected_message -= 1;
                }
            } else {
                pty.send_bytes(b"\x1b[A")?;
            }
        }
        KeyEvent {
            code: KeyCode::Down,
            ..
        } => {
            if matches!(app.focus, Focus::History) {
                if app.selected_message + 1 < app.messages.len() {
                    app.selected_message += 1;
                }
            } else {
                pty.send_bytes(b"\x1b[B")?;
            }
        }
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => {
            if matches!(app.focus, Focus::Output) {
                pty.send_bytes(b"\x1b[D")?;
            }
        }
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => {
            if matches!(app.focus, Focus::Output) {
                pty.send_bytes(b"\x1b[C")?;
            }
        }
        KeyEvent {
            code: KeyCode::PageUp,
            ..
        } => {
            app.follow_output = false;
            app.output_scroll = app.output_scroll.saturating_sub(10);
        }
        KeyEvent {
            code: KeyCode::PageDown,
            ..
        } => {
            app.output_scroll = (app.output_scroll + 10).min(app.output_lines.len().saturating_sub(1));
        }
        KeyEvent {
            code: KeyCode::End,
            ..
        } => {
            app.follow_output = true;
            app.output_scroll = app.output_lines.len().saturating_sub(1);
        }
        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => {
            if matches!(app.focus, Focus::History) {
                if let Some(msg) = app.messages.get(app.selected_message) {
                    app.output_scroll = msg.output_line;
                    app.follow_output = false;
                }
            } else {
                pty.send_bytes(b"\r")?;
                let content = app.input_buffer.trim_end().to_string();
                if !content.is_empty() {
                    let output_line = app.output_lines.len().saturating_sub(1);
                    app.record_user_message(db, content, output_line)?;
                }
                app.input_buffer.clear();
            }
        }
        KeyEvent {
            code: KeyCode::Char('d'),
            ..
        } => {
            if matches!(app.focus, Focus::History) {
                if let Some(msg) = app.messages.get(app.selected_message) {
                    if let Some(commit) = msg.snapshot_commit.clone() {
                        open_diff_preview(app, &commit, false)?;
                    }
                }
            }
        }
        KeyEvent {
            code: KeyCode::Char('r'),
            ..
        } => {
            if matches!(app.focus, Focus::History) {
                if let Some(msg) = app.messages.get(app.selected_message) {
                    if let Some(commit) = msg.snapshot_commit.clone() {
                        open_diff_preview(app, &commit, true)?;
                    }
                }
            }
        }
        KeyEvent {
            code: KeyCode::Backspace,
            ..
        } => {
            if matches!(app.focus, Focus::Output) {
                app.input_buffer.pop();
                pty.send_bytes(&[0x7f])?;
            }
        }
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            ..
        } => {
            if matches!(app.focus, Focus::Output) {
                app.input_buffer.push(c);
                pty.send_bytes(c.to_string().as_bytes())?;
            }
        }
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            ..
        } => {
            if matches!(app.focus, Focus::Output) {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    let ctrl = (c as u8) & 0x1f;
                    pty.send_bytes(&[ctrl])?;
                } else {
                    pty.send_bytes(c.to_string().as_bytes())?;
                }
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_diff_keys(key: KeyEvent, app: &mut App) -> Result<bool> {
    let preview = app.diff_preview.as_mut().unwrap();
    match key.code {
        KeyCode::Esc => {
            app.diff_preview = None;
        }
        KeyCode::Char('q') => {
            app.diff_preview = None;
        }
        KeyCode::Up => {
            preview.scroll = preview.scroll.saturating_sub(1);
        }
        KeyCode::Down => {
            preview.scroll = (preview.scroll + 1).min(preview.lines.len().saturating_sub(1));
        }
        KeyCode::PageUp => {
            preview.scroll = preview.scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            preview.scroll = (preview.scroll + 10).min(preview.lines.len().saturating_sub(1));
        }
        KeyCode::Char('y') => {
            if let Some(commit) = preview.pending_restore.clone() {
                app.snapshot_manager.restore(&commit)?;
            }
            app.diff_preview = None;
        }
        KeyCode::Char('n') => {
            app.diff_preview = None;
        }
        _ => {}
    }
    Ok(false)
}

fn open_diff_preview(app: &mut App, commit: &str, pending_restore: bool) -> Result<()> {
    let diff = app.snapshot_manager.diff_preview(commit)?;
    let lines: Vec<String> = if diff.is_empty() {
        vec!["(no changes)".to_string()]
    } else {
        diff.lines().map(|l| l.to_string()).collect()
    };
    app.diff_preview = Some(DiffPreview {
        title: format!("Diff for {}", commit),
        lines,
        scroll: 0,
        pending_restore: if pending_restore {
            Some(commit.to_string())
        } else {
            None
        },
    });
    Ok(())
}

fn draw_ui(f: &mut Frame, app: &mut App) {
    let size = f.size();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(size);

    draw_output_panel(f, app, chunks[0]);
    draw_workbench(f, app, chunks[1]);

    if let Some(preview) = &app.diff_preview {
        draw_diff_preview(f, preview, size);
    }
}

fn draw_output_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if matches!(app.focus, Focus::Output) {
        "Claude (focused)"
    } else {
        "Claude"
    };
    let block = Block::default().title(title).borders(Borders::ALL);
    let visible_height = area.height.saturating_sub(2) as usize;
    let start = app.output_scroll.saturating_sub(visible_height.saturating_sub(1));
    let end = (start + visible_height).min(app.output_lines.len());
    let lines: Vec<Line> = app.output_lines[start..end]
        .iter()
        .map(|l| Line::raw(l.clone()))
        .collect();
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_workbench(f: &mut Frame, app: &mut App, area: Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(5),
        ])
        .split(area);

    draw_usage_panel(f, app, sections[0]);
    draw_context_panel(f, app, sections[1]);
    draw_history_panel(f, app, sections[2]);
}

fn draw_usage_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let context_tokens = app.estimate_context_tokens() as u64;
    let entries = app.usage_manager.entries(context_tokens);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(primary) = entries.first() {
        if let (Some(used), Some(limit)) = (primary.used, primary.limit) {
            let pct = if limit == 0 { 0.0 } else { used as f64 / limit as f64 };
            let bar_width = area.width.saturating_sub(2) as usize;
            let filled = ((bar_width as f64) * pct).round() as usize;
            let mut bar = String::new();
            for i in 0..bar_width {
                if i < filled {
                    bar.push('█');
                } else {
                    bar.push('░');
                }
            }
            lines.push(Line::from(Span::raw(format!(
                "{}: {} / {} tokens",
                primary.name, used, limit
            ))));
            lines.push(Line::from(Span::raw(bar)));
        } else {
            lines.push(Line::from(Span::raw(format!(
                "{}: {}",
                primary.name,
                primary.status.clone().unwrap_or_else(|| "unavailable".to_string())
            ))));
        }
    }
    for entry in entries.iter().skip(1) {
        let line = match (entry.used, entry.limit) {
            (Some(used), Some(limit)) => format!("{}: {} / {} tokens", entry.name, used, limit),
            _ => format!(
                "{}: {}",
                entry.name,
                entry.status.clone().unwrap_or_else(|| "unavailable".to_string())
            ),
        };
        lines.push(Line::from(Span::raw(line)));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::raw("No providers configured")));
    }
    let paragraph = Paragraph::new(lines)
        .block(Block::default().title("Usage").borders(Borders::ALL));
    f.render_widget(paragraph, area);
}

fn draw_context_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let used = app.estimate_context_tokens() as f32;
    let limit = app.config.context_limit as f32;
    let pct = if limit == 0.0 { 0.0 } else { used / limit };
    let threshold = app.config.compress_threshold;
    let remaining_pct = (threshold - pct).max(0.0);
    let bar_width = area.width.saturating_sub(2) as usize;
    let filled = ((bar_width as f32) * pct).round() as usize;
    let mut bar = String::new();
    for i in 0..bar_width {
        if i < filled {
            bar.push('█');
        } else {
            bar.push('░');
        }
    }
    let color = if pct >= threshold { Color::Red } else { Color::Green };
    let lines = vec![
        Line::from(vec![
            Span::raw("Context: "),
            Span::styled(format!("{:.1}%", pct * 100.0), Style::default().fg(color)),
        ]),
        Line::from(Span::raw(bar)),
        Line::from(Span::raw(format!("Distance to compression: {:.1}%", remaining_pct * 100.0))),
    ];
    let paragraph = Paragraph::new(lines)
        .block(Block::default().title("Context").borders(Borders::ALL));
    f.render_widget(paragraph, area);
}

fn draw_history_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if matches!(app.focus, Focus::History) {
        "History (Tab to focus, d diff, r restore)"
    } else {
        "History"
    };
    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|m| {
            let mut preview = m.content.clone();
            if preview.len() > 40 {
                preview.truncate(40);
                preview.push_str("…");
            }
            let suffix = if m.snapshot_commit.is_some() { "✓" } else { "…" };
            ListItem::new(Line::from(Span::raw(format!("{} {}", preview, suffix))))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .highlight_symbol("➜ ");
    let mut state = ListState::default();
    if !app.messages.is_empty() {
        state.select(Some(app.selected_message.min(app.messages.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

use ratatui::widgets::ListState;

fn draw_diff_preview(f: &mut Frame, preview: &DiffPreview, area: Rect) {
    let popup = centered_rect(90, 80, area);
    let block = Block::default().title(preview.title.clone()).borders(Borders::ALL);
    let height = popup.height.saturating_sub(2) as usize;
    let start = preview.scroll.saturating_sub(height.saturating_sub(1));
    let end = (start + height).min(preview.lines.len());
    let lines: Vec<Line> = preview.lines[start..end]
        .iter()
        .map(|l| Line::raw(l.clone()))
        .collect();
    let mut footer = Vec::new();
    if preview.pending_restore.is_some() {
        footer.push(Line::from(Span::styled(
            "Press y to restore, n to cancel",
            Style::default().fg(Color::Yellow),
        )));
    } else {
        footer.push(Line::from(Span::raw("Press q or Esc to close")));
    }
    let mut text = Text::from(lines);
    text.lines.extend(footer);
    let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(paragraph, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn append_output_lines(lines: &mut Vec<String>, chunk: &str) {
    let chunk = chunk.replace('\r', "");
    let mut iter = chunk.split('\n');
    if let Some(first) = iter.next() {
        if let Some(last) = lines.last_mut() {
            last.push_str(first);
        } else {
            lines.push(first.to_string());
        }
    }
    for part in iter {
        lines.push(part.to_string());
    }
    let max_lines = 5000;
    if lines.len() > max_lines {
        let excess = lines.len() - max_lines;
        lines.drain(0..excess);
    }
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if let Some('[') = chars.peek().copied() {
                chars.next();
                while let Some(ch) = chars.next() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as f32;
    (chars / 4.0).ceil() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn snapshot_and_restore() -> Result<()> {
        let tmp = TempDir::new()?;
        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace)?;
        let data_dir = workspace.join(".cc-workbench");
        fs::create_dir_all(&data_dir)?;
        let manager = SnapshotManager::new(&workspace, &data_dir)?;

        let file = workspace.join("main.txt");
        fs::write(&file, "hello")?;
        let commit1 = manager.snapshot(1)?;

        fs::write(&file, "hello world")?;
        let _commit2 = manager.snapshot(2)?;

        let diff = manager.diff_preview(&commit1)?;
        assert!(diff.contains("hello world"));

        manager.restore(&commit1)?;
        let contents = fs::read_to_string(&file)?;
        assert_eq!(contents, "hello");
        Ok(())
    }

    #[test]
    fn test_extract_u64() {
        let json = serde_json::json!({
            "data": {
                "used": 123,
                "limit": "456"
            }
        });
        assert_eq!(extract_u64(&json, "/data/used").unwrap(), 123);
        assert_eq!(extract_u64(&json, "/data/limit").unwrap(), 456);
        assert!(extract_u64(&json, "/missing").is_err());
    }
}

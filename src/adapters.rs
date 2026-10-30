//! Adapters: optional add-ons, each a folder with an `adapter.toml` and,
//! usually, a program in any language that talks to the app with JSON
//! messages, one per line, over stdin and stdout. Every sister in the 9000
//! series runs them the same way, with this host; the messages are in
//! [`crate::family`].
//!
//! An adapter can add commands (to any menu, and to keys.toml) and panels,
//! and whatever its app adds on top (export formats, file types, snapping):
//! those sections of `adapter.toml` are the app's own type `X`, read along
//! with the rest.
//!
//! Adapters come from two places. Built-in ones ship inside the app and can
//! be removed (and added back); installed ones are dropped on the window as
//! a zip or folder and live in the config folder's `adapters`. Every
//! program-backed adapter the user installed is approved before it first
//! runs, and any of them can crash without taking the app down.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::family;

/// The sections of `adapter.toml` that one app adds: its export formats,
/// file types and so on. `()` for an app with none.
pub trait Extra: DeserializeOwned + Clone + Default + std::fmt::Debug + Send + 'static {
    /// What they add, in words for the install prompt ("2 export formats").
    fn adds(&self) -> Vec<String> {
        Vec::new()
    }
    /// They do something with no program to run (presets, settings).
    fn without_program(&self) -> bool {
        false
    }
    /// What runs, in words, when `run` is empty but these sections have
    /// programs of their own (file types grace9000 opens with one).
    fn own_programs(&self) -> Option<String> {
        None
    }
    /// What `adapter.toml` needs when it has no program, for the error.
    const ALONE: &'static str = "";
}

impl Extra for () {}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(bound = "X: Extra")]
pub struct Manifest<X: Extra = ()> {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    /// The oldest version of the app it works with, such as "0.0.6".
    #[serde(default)]
    pub requires: String,
    /// The sisters it works in, such as ["splice9000", "wave9000"]; left
    /// out, any that installs it (most adapters are made for one).
    #[serde(default)]
    pub apps: Vec<String>,
    /// The program and its arguments, run from the adapter's folder. Empty
    /// for an adapter that only adds export formats.
    #[serde(default)]
    pub run: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub start: Start,
    /// Events to receive while running, such as "changed" or "saved".
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub panels: Vec<PanelSpec>,
    /// The app's own sections.
    #[serde(flatten)]
    pub extra: X,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// When one of its commands is first chosen, or its panel is shown.
    #[default]
    Command,
    /// When the app opens.
    Launch,
}

#[derive(Deserialize, Clone, Debug)]
pub struct CommandSpec {
    pub id: String,
    pub label: String,
    /// A default shortcut, such as "cmd+alt+k". keys.toml can override it.
    #[serde(default)]
    pub keys: Option<String>,
    /// One of the app's menus ("file", "edit", ...) to show it there
    /// instead of the Adapters menu.
    #[serde(default)]
    pub menu: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct PanelSpec {
    pub id: String,
    pub title: String,
}

/// What the reader threads tell the interface.
pub enum Message {
    /// A notification or request from an adapter. Requests have an `id` and
    /// must be answered with [`Host::reply`].
    Call {
        adapter: String,
        id: Option<Value>,
        method: String,
        params: Value,
    },
    Stopped {
        adapter: String,
        generation: u64,
    },
}

/// A running adapter's pipe.
pub struct Link {
    name: String,
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, String>>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
}

impl Link {
    fn write(&self, message: Value) {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        // An adapter that has quit just misses the message.
        let _ = writeln!(stdin, "{message}").and_then(|()| stdin.flush());
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.write(json!({ "method": method, "params": params }));
    }

    /// Sends a request and waits for the answer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(format!("{} has stopped", self.name));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        self.write(json!({ "id": id, "method": method, "params": params }));
        rx.recv_timeout(timeout).unwrap_or_else(|_| {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            Err(format!("{} didn't answer {method} in time", self.name))
        })
    }

    fn answered(&self, id: u64, result: Result<Value, String>) {
        if let Some(tx) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
            let _ = tx.send(result);
        }
    }

    fn stopped(&self) {
        self.alive.store(false, Ordering::SeqCst);
        for (_, tx) in self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            let _ = tx.send(Err(format!("{} stopped", self.name)));
        }
    }
}

struct Running {
    child: Child,
    link: Arc<Link>,
    generation: u64,
}

pub struct Adapter<X: Extra = ()> {
    pub dir: PathBuf,
    pub manifest: Manifest<X>,
    /// Ships inside the app.
    pub builtin: bool,
    running: Option<Running>,
}

impl<X: Extra> Adapter<X> {
    pub fn is_running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|r| r.link.alive.load(Ordering::SeqCst))
    }

    pub fn has(&self, permission: &str) -> bool {
        self.manifest.permissions.iter().any(|p| p == permission)
    }
}

/// Why an adapter didn't start.
#[derive(Debug, PartialEq)]
pub enum StartError {
    /// The user hasn't allowed it yet (or it asks for more than before).
    NeedsApproval,
    Failed(String),
}

type Deliver = Arc<dyn Fn(Message) + Send + Sync>;

/// What adapters see of the app's surroundings.
#[derive(Clone, Default)]
pub struct Env {
    pub config: PathBuf,
    /// Where the built-in adapters are, inside the app.
    pub builtin: Option<PathBuf>,
    /// More for `initialize`, such as `"ffmpeg": "/usr/bin/ffmpeg"`.
    pub init: serde_json::Map<String, Value>,
    /// More environment variables for adapters' programs, such as
    /// `SPLICE9000_FFMPEG`.
    pub vars: Vec<(String, std::ffi::OsString)>,
}

/// All adapters, owned by the interface thread.
pub struct Host<X: Extra = ()> {
    pub all: Vec<Adapter<X>>,
    pub env: Env,
    send: Deliver,
    generation: u64,
}

impl<X: Extra> Host<X> {
    pub fn new(env: Env, send: impl Fn(Message) + Send + Sync + 'static) -> Self {
        let all = discover(&env);
        Host {
            all,
            env,
            send: Arc::new(send),
            generation: 0,
        }
    }

    /// Looks for adapters again, stopping the ones that were running.
    pub fn reload(&mut self) {
        self.stop_all();
        self.all = discover(&self.env);
    }

    /// Starts the adapters that ask to start with the app and may.
    pub fn start_launch_adapters(&mut self) {
        for i in 0..self.all.len() {
            if self.all[i].manifest.start == Start::Launch
                && let Err(StartError::Failed(e)) = self.start(i)
            {
                crate::log::error(e);
            }
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.all.iter().position(|a| a.manifest.name == name)
    }

    pub fn approved(&self, i: usize) -> bool {
        let a = &self.all[i];
        a.builtin || is_approved(&self.env.config, &a.manifest)
    }

    /// Starts adapter `i` if it isn't running. An adapter the user installed
    /// must have been allowed first ([`Host::allow`]).
    pub fn start(&mut self, i: usize) -> Result<Arc<Link>, StartError> {
        if let Some(r) = &self.all[i].running
            && r.link.alive.load(Ordering::SeqCst)
        {
            return Ok(r.link.clone());
        }
        let manifest = self.all[i].manifest.clone();
        if manifest.run.is_empty() {
            return Err(StartError::Failed(format!(
                "{} has no program to run",
                manifest.name
            )));
        }
        if !self.approved(i) {
            return Err(StartError::NeedsApproval);
        }
        let mut child = spawn(&self.all[i].dir, &manifest.run, &self.env)
            .map_err(|e| StartError::Failed(format!("Couldn't start {}: {e}", manifest.name)))?;
        self.generation += 1;
        let generation = self.generation;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take().expect("stdin is piped");
        let link = Arc::new(Link {
            name: manifest.name.clone(),
            stdin: Mutex::new(stdin),
            pending: Mutex::default(),
            next_id: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        if let Some(stderr) = child.stderr.take() {
            let name = manifest.name.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    crate::log::info(format!("{name}: {line}"));
                }
            });
        }
        let (reader, send, name) = (link.clone(), self.send.clone(), manifest.name.clone());
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let line = match read_message(&mut stdout) {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        crate::log::error(format!("{name}: {e}"));
                        continue;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    crate::log::error(format!("{name}: not a JSON message: {}", line.trim()));
                    continue;
                };
                match message.get("method").and_then(Value::as_str) {
                    Some(method) => send(Message::Call {
                        adapter: name.clone(),
                        id: message.get("id").cloned(),
                        method: method.to_owned(),
                        params: message.get("params").cloned().unwrap_or(Value::Null),
                    }),
                    None => {
                        let Some(id) = message.get("id").and_then(Value::as_u64) else {
                            continue;
                        };
                        let result = match message.get("error") {
                            Some(e) => Err(e
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|| e.to_string())),
                            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        reader.answered(id, result);
                    }
                }
            }
            reader.stopped();
            send(Message::Stopped {
                adapter: name,
                generation,
            });
        });

        link.notify(
            "initialize",
            family::initialize(&manifest.permissions, &self.env.init),
        );
        crate::log::info(format!("Started {} {}", manifest.name, manifest.version));
        self.all[i].running = Some(Running {
            child,
            link: link.clone(),
            generation,
        });
        Ok(link)
    }

    /// Runs an adapter's command, starting the adapter if needed.
    pub fn run_command(&mut self, name: &str, id: &str) -> Result<(), StartError> {
        let i = self
            .index(name)
            .ok_or_else(|| StartError::Failed(format!("No adapter called \"{name}\"")))?;
        if !self.all[i].manifest.commands.iter().any(|c| c.id == id) {
            return Err(StartError::Failed(format!(
                "{name} has no command \"{id}\""
            )));
        }
        self.start(i)?.notify("command", json!({ "command": id }));
        Ok(())
    }

    /// Sends an event to the running adapters that asked for it and may see
    /// the work (the app's events permission).
    pub fn emit(&self, kind: &str, mut params: Value) {
        params["kind"] = json!(kind);
        for a in &self.all {
            if let Some(r) = &a.running
                && a.manifest.events.iter().any(|e| e == kind)
                && a.has(crate::app().events_permission)
            {
                r.link.notify("event", params.clone());
            }
        }
    }

    pub fn reply(&self, adapter: &str, id: Value, result: Result<Value, String>) {
        let Some(link) = self
            .index(adapter)
            .and_then(|i| self.all[i].running.as_ref())
            .map(|r| &r.link)
        else {
            return;
        };
        link.write(match result {
            Ok(value) => json!({ "id": id, "result": value }),
            Err(e) => json!({ "id": id, "error": e }),
        });
    }

    /// An adapter's program ended. Ignored (and false) if it has been
    /// stopped on purpose or restarted since.
    pub fn stopped(&mut self, adapter: &str, generation: u64) -> bool {
        let Some(i) = self.index(adapter) else {
            return false;
        };
        if !self.all[i]
            .running
            .as_ref()
            .is_some_and(|r| r.generation == generation)
        {
            return false;
        }
        let mut r = self.all[i].running.take().expect("checked");
        let status = r
            .child
            .wait()
            .map(|s| s.to_string())
            .unwrap_or_else(|e| e.to_string());
        crate::log::info(format!("{adapter} stopped ({status})"));
        true
    }

    /// Asks every running adapter to shut down, then stops the ones that don't.
    pub fn stop_all(&mut self) {
        let mut children = Vec::new();
        for a in &mut self.all {
            if let Some(r) = a.running.take() {
                r.link.notify("shutdown", Value::Null);
                children.push(r.child);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        for mut child in children {
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The user allowed adapter `i` with its current permissions.
    pub fn allow(&self, i: usize) {
        remember_approval(&self.env.config, &self.all[i].manifest);
    }

    /// Takes an adapter out. A built-in one is only switched off (it's part
    /// of the app) and can be added back; an installed one goes to the trash.
    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let i = self
            .index(name)
            .ok_or_else(|| format!("No adapter called \"{name}\""))?;
        self.stop_all();
        if self.all[i].builtin {
            let mut list = Removed::load(&self.env.config);
            if !list.removed.iter().any(|n| n == name) {
                list.removed.push(name.to_owned());
            }
            list.save(&self.env.config)?;
        } else {
            to_trash(&self.all[i].dir)?;
            forget_approval(&self.env.config, name);
        }
        self.all = discover(&self.env);
        Ok(())
    }

    /// Built-in adapters that were removed, to offer back.
    pub fn removed_builtins(&self) -> Vec<Manifest<X>> {
        let removed = Removed::load(&self.env.config).removed;
        let Some(dir) = &self.env.builtin else {
            return Vec::new();
        };
        discover_in(dir)
            .into_iter()
            .map(|a| a.manifest)
            .filter(|m| removed.contains(&m.name))
            .collect()
    }

    pub fn add_back(&mut self, name: &str) -> Result<(), String> {
        let mut list = Removed::load(&self.env.config);
        list.removed.retain(|n| n != name);
        list.save(&self.env.config)?;
        self.reload();
        Ok(())
    }
}

impl<X: Extra> Drop for Host<X> {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// Built-in adapters the user took out, in the config folder's adapters.toml.
#[derive(Serialize, Deserialize, Default)]
struct Removed {
    #[serde(default)]
    removed: Vec<String>,
}

impl Removed {
    fn load(config: &Path) -> Self {
        std::fs::read_to_string(config.join("adapters.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, config: &Path) -> Result<(), String> {
        let text = format!(
            "# Built-in adapters you've removed. Take a name out (or use Add Back in\n\
             # the Adapters panel) to have it again.\n\n{}",
            toml::to_string(self).map_err(|e| e.to_string())?
        );
        std::fs::write(config.join("adapters.toml"), text).map_err(|e| e.to_string())
    }
}

/// The longest message the app reads from an adapter.
const MAX_MESSAGE: u64 = 32 << 20;

/// Reads one line, refusing (and skipping) one longer than MAX_MESSAGE.
/// None at the end of the output.
fn read_message(r: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_MESSAGE)
        .read_until(b'\n', &mut buf)
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') && n as u64 == MAX_MESSAGE {
        loop {
            buf.clear();
            let n = r
                .by_ref()
                .take(1 << 20)
                .read_until(b'\n', &mut buf)
                .map_err(|e| e.to_string())?;
            if n == 0 || buf.last() == Some(&b'\n') {
                break;
            }
        }
        return Err(format!(
            "sent a message over {} MB; it was ignored",
            MAX_MESSAGE >> 20
        ));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Refuses an adapter whose program was built for another kind of computer,
/// with a message saying which to get, instead of failing to start later.
pub fn check_platform<X: Extra>(dir: &Path, manifest: &Manifest<X>) -> Result<(), String> {
    let Some(program) = manifest.run.first().filter(|p| p.contains(['/', '\\'])) else {
        return Ok(()); // no program, a script run by an interpreter, or one on the PATH
    };
    let path = program_path(dir, program);
    let mut magic = [0u8; 20];
    let n = std::fs::File::open(&path)
        .and_then(|mut f| f.read(&mut magic))
        .map_err(|_| format!("{} is missing its program ({program})", manifest.name))?;
    let magic = &magic[..n];
    let built_for = if magic.starts_with(b"MZ") {
        "Windows"
    } else if magic.starts_with(b"\x7fELF") {
        "Linux"
    } else if magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || magic.starts_with(&[0xca, 0xfe, 0xba, 0xbe])
    {
        // A thin Mach-O names its processor; Apple Silicon runs Intel ones too.
        let arm_only = magic.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
            && magic.get(4..8) == Some(&[0x0c, 0x00, 0x00, 0x01]);
        if arm_only && cfg!(target_arch = "x86_64") {
            "Apple Silicon Macs"
        } else {
            "macOS"
        }
    } else {
        return Ok(()); // a script; let it try
    };
    let ours = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    if built_for == ours {
        Ok(())
    } else {
        let get = if built_for == "Apple Silicon Macs" {
            "Intel Mac (macos-x64)"
        } else {
            ours
        };
        Err(format!(
            "{} is built for {built_for}, not this computer. Download its {get} version.",
            manifest.name
        ))
    }
}

/// A program given as a path (`./frame-grab`) is found in the adapter's
/// folder; on Windows `.exe` is added when that's what's there, so one
/// adapter.toml works everywhere.
fn program_path(dir: &Path, program: &str) -> PathBuf {
    let path = dir.join(program);
    if cfg!(windows) && !path.exists() {
        let exe = path.with_extension("exe");
        if exe.exists() {
            return exe;
        }
    }
    path
}

/// Starts an adapter's program from its folder, with the app's environment
/// for adapters, its pipes ready.
pub fn spawn(dir: &Path, run: &[String], env: &Env) -> Result<Child, String> {
    let (program, args) = run.split_first().ok_or("adapter.toml has an empty `run`")?;
    let program = if program.contains(['/', '\\']) {
        program_path(dir, program).into_os_string()
    } else {
        program.into()
    };
    let mut cmd = Command::new(&program);
    cmd.args(args)
        .current_dir(dir)
        // So adapters can keep their own files.
        .env(
            format!("{}_CONFIG_DIR", crate::app().env_prefix),
            &env.config,
        )
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &env.vars {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("{} not found", program.to_string_lossy()),
        _ => e.to_string(),
    })
}

// ---- Finding and installing ----------------------------------------------

pub fn adapters_dir(config: &Path) -> PathBuf {
    config.join("adapters")
}

/// Where the built-in adapters are: inside the macOS app, or beside the
/// program elsewhere. <PREFIX>_BUILTIN_ADAPTERS (SPLICE9000_BUILTIN_ADAPTERS,
/// ...) points somewhere else, for development.
pub fn builtin_dir() -> Option<PathBuf> {
    if let Some(dir) = crate::env("BUILTIN_ADAPTERS") {
        return Some(dir.into());
    }
    let exe = std::env::current_exe().ok()?;
    let here = exe.parent()?;
    [here.join("../Resources/adapters"), here.join("adapters")]
        .into_iter()
        .find(|d| d.is_dir())
}

/// Built-in adapters that weren't removed, then installed ones; an installed
/// adapter replaces a built-in one of the same name (a newer version).
fn discover<X: Extra>(env: &Env) -> Vec<Adapter<X>> {
    let removed = Removed::load(&env.config).removed;
    let mut all: Vec<Adapter<X>> = env
        .builtin
        .as_deref()
        .map(discover_in)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !removed.contains(&a.manifest.name))
        .map(|a| Adapter { builtin: true, ..a })
        .collect();
    for a in discover_in(&adapters_dir(&env.config)) {
        all.retain(|b| b.manifest.name != a.manifest.name);
        all.push(a);
    }
    all
}

fn discover_in<X: Extra>(root: &Path) -> Vec<Adapter<X>> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("adapter.toml").is_file())
        .collect();
    dirs.sort();
    let mut found: Vec<Adapter<X>> = Vec::new();
    for dir in dirs {
        match load_manifest(&dir).and_then(|m| check_platform(&dir, &m).map(|()| m)) {
            Ok(manifest) => {
                found.retain(|a| a.manifest.name != manifest.name);
                found.push(Adapter {
                    dir,
                    manifest,
                    builtin: false,
                    running: None,
                });
            }
            Err(e) => crate::log::error(e),
        }
    }
    found
}

/// Whether `have` is at least `version` ("0.0.6").
pub fn new_enough(have: &str, version: &str) -> bool {
    let parts = |v: &str| {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(have) >= parts(version)
}

pub fn parse_manifest<X: Extra>(text: &str, at: &str) -> Result<Manifest<X>, String> {
    let app = crate::app();
    let manifest: Manifest<X> =
        toml::from_str(text).map_err(|e| format!("{at}: {}", e.message()))?;
    if manifest.name.trim().is_empty() {
        return Err(format!("{at}: `name` is empty"));
    }
    if !manifest.apps.is_empty()
        && !manifest
            .apps
            .iter()
            .any(|a| a.eq_ignore_ascii_case(app.name))
    {
        return Err(format!(
            "{} is an adapter for {}, not {}.",
            manifest.name,
            manifest.apps.join(" and "),
            app.name
        ));
    }
    if !manifest.requires.is_empty() && !new_enough(app.version, &manifest.requires) {
        return Err(format!(
            "{} needs {} {} or newer (this is {}). Get the latest from github.com/solacecasino/{}/releases.",
            manifest.name, app.name, manifest.requires, app.version, app.name
        ));
    }
    if manifest.run.is_empty() && !manifest.extra.without_program() {
        let alone = if X::ALONE.is_empty() {
            String::new()
        } else {
            format!(" or {}", X::ALONE)
        };
        return Err(format!("{at}: needs a program to `run`{alone}"));
    }
    if manifest.run.is_empty() && !(manifest.commands.is_empty() && manifest.panels.is_empty()) {
        return Err(format!("{at}: commands and panels need a program to `run`"));
    }
    for p in &manifest.permissions {
        if !family::PERMISSIONS
            .iter()
            .chain(app.permissions)
            .any(|(name, _)| name == p)
        {
            crate::log::error(format!("{}: unknown permission \"{p}\"", manifest.name));
        }
    }
    Ok(manifest)
}

fn load_manifest<X: Extra>(dir: &Path) -> Result<Manifest<X>, String> {
    let path = dir.join("adapter.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_manifest(&text, &path.display().to_string())
}

/// Whether `path` is an adapter to install rather than media: a folder with
/// `adapter.toml`, or a zip with one at its top or one folder down.
pub fn is_package(path: &Path) -> bool {
    if path.is_dir() {
        return path.join("adapter.toml").is_file();
    }
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
        && zip_manifest(path).is_some()
}

fn zip_manifest(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let archive = zip::ZipArchive::new(std::io::BufReader::new(file)).ok()?;
    archive
        .file_names()
        .filter_map(Result::ok)
        .find(|n| n.rsplit('/').next() == Some("adapter.toml") && n.matches('/').count() <= 1)
        .map(|n| n.into_owned())
}

/// Reads the manifest of a package without installing it.
pub fn package_manifest<X: Extra>(path: &Path) -> Result<Manifest<X>, String> {
    if path.is_dir() {
        return load_manifest(path);
    }
    let name = zip_manifest(path).ok_or("not an adapter package")?;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let mut text = String::new();
    archive
        .by_name(&name)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    parse_manifest(&text, "adapter.toml")
}

/// Copies a package into the adapters folder, replacing an older copy of
/// the same adapter. The new copy is put together beside it first, so a
/// failure leaves what was there.
pub fn install<X: Extra>(source: &Path, config: &Path) -> Result<Manifest<X>, String> {
    install_into(source, &adapters_dir(config))
}

fn install_into<X: Extra>(source: &Path, root: &Path) -> Result<Manifest<X>, String> {
    let manifest: Manifest<X> = package_manifest(source)?;
    let folder: String = manifest
        .name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let incoming = root.join(format!(".incoming-{folder}"));
    let _ = std::fs::remove_dir_all(&incoming);
    let result = if source.is_dir() {
        copy_dir(source, &incoming).map_err(|e| e.to_string())
    } else {
        unzip(source, &incoming)
    };
    if let Err(e) = result.and_then(|()| check_platform(&incoming, &manifest)) {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("Couldn't install {}: {e}", manifest.name));
    }
    let target = root.join(&folder);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&incoming, &target).map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if [".git", "target", "dist"].iter().any(|skip| name == *skip) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            std::fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

/// Unpacks a zip into `to`. A zip holding one folder is unpacked from inside
/// that folder. Paths that would escape `to` are refused.
fn unzip(zip_path: &Path, to: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let manifest = zip_manifest(zip_path).ok_or("not an adapter package")?;
    let prefix = manifest.trim_end_matches("adapter.toml").to_owned();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(inside) = entry.enclosed_name() else {
            return Err(format!("unsafe path in zip: entry {i}"));
        };
        let Ok(rel) = inside.strip_prefix(&prefix) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = to.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// Moves a folder to the system's trash, so a removed adapter can still be
/// fished out.
fn to_trash(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2_foundation::NSString;
        let path = NSString::from_str(&path.to_string_lossy());
        // SAFETY: plain Foundation calls with live objects.
        let done = unsafe {
            let (Some(manager), Some(url)) =
                (AnyClass::get(c"NSFileManager"), AnyClass::get(c"NSURL"))
            else {
                return Err("no file manager".into());
            };
            let manager: *mut AnyObject = msg_send![manager, defaultManager];
            let url: *mut AnyObject = msg_send![url, fileURLWithPath: &*path];
            let ok: Bool = msg_send![manager, trashItemAtURL: url, resultingItemURL: std::ptr::null_mut::<*mut AnyObject>(), error: std::ptr::null_mut::<*mut AnyObject>()];
            ok.as_bool()
        };
        if done {
            Ok(())
        } else {
            Err(format!("couldn't move {} to the Trash", path))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

// ---- Approval --------------------------------------------------------------

/// What a permission allows, in words.
pub fn describe(permission: &str) -> String {
    family::PERMISSIONS
        .iter()
        .chain(crate::app().permissions)
        .find(|(name, _)| *name == permission)
        .map_or_else(|| permission.to_owned(), |(_, words)| (*words).to_owned())
}

/// What an adapter is and may do, in words, for the install and approval
/// prompts.
pub fn summary<X: Extra>(m: &Manifest<X>) -> Vec<String> {
    let mut lines = Vec::new();
    let by = if m.author.is_empty() {
        String::new()
    } else {
        format!(" by {}", m.author)
    };
    let license = if m.license.is_empty() {
        String::new()
    } else {
        format!(" · {}", m.license)
    };
    lines.push(format!("{} {}{by}{license}", m.name, m.version));
    if !m.description.is_empty() {
        lines.push(m.description.clone());
    }
    let mut adds = Vec::new();
    if !m.commands.is_empty() {
        adds.push(format!(
            "{} command{}",
            m.commands.len(),
            if m.commands.len() == 1 { "" } else { "s" }
        ));
    }
    if !m.panels.is_empty() {
        adds.push(format!(
            "{} panel{}",
            m.panels.len(),
            if m.panels.len() == 1 { "" } else { "s" }
        ));
    }
    adds.extend(m.extra.adds());
    if !adds.is_empty() {
        lines.push(format!("Adds {}.", adds.join(", ")));
    }
    if let Some(note) = m.extra.own_programs().filter(|_| m.run.is_empty()) {
        lines.push(note);
    } else if m.run.is_empty() {
        lines.push("It has no program: nothing of it runs.".into());
    } else if m.permissions.is_empty() {
        lines.push("It doesn't ask for any permissions.".into());
    } else {
        lines.push("It asks to:".into());
        lines.extend(m.permissions.iter().map(|p| format!("•  {}", describe(p))));
    }
    lines
}

fn approvals_path(config: &Path) -> PathBuf {
    config.join("approvals.json")
}

fn load_approvals(config: &Path) -> BTreeMap<String, Vec<String>> {
    std::fs::read_to_string(approvals_path(config))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn is_approved<X: Extra>(config: &Path, m: &Manifest<X>) -> bool {
    load_approvals(config)
        .get(&m.name)
        .is_some_and(|granted| m.permissions.iter().all(|p| granted.contains(p)))
}

/// Records that the user allowed `m` with its current permissions.
/// Installing through the app's prompt counts, since it showed the same.
pub fn remember_approval<X: Extra>(config: &Path, m: &Manifest<X>) {
    let mut approvals = load_approvals(config);
    approvals.insert(m.name.clone(), m.permissions.clone());
    let _ = std::fs::write(
        approvals_path(config),
        serde_json::to_string_pretty(&approvals).expect("serializes"),
    );
}

/// Forgets an approval, for a removed adapter: if it comes back, it's asked
/// about again.
fn forget_approval(config: &Path, name: &str) {
    let mut approvals = load_approvals(config);
    if approvals.remove(name).is_some() {
        let _ = std::fs::write(
            approvals_path(config),
            serde_json::to_string_pretty(&approvals).expect("serializes"),
        );
    }
}

/// Reads `adapter "Name" id` (or `export "Name" id`) from keys.toml or a menu.
pub fn parse_command<'a>(text: &'a str, kind: &str) -> Option<(&'a str, &'a str)> {
    let rest = text
        .strip_prefix(kind)?
        .strip_prefix(' ')?
        .trim_start()
        .strip_prefix('"')?;
    let (name, id) = rest.split_once('"')?;
    let id = id.trim();
    (!name.is_empty() && !id.is_empty()).then_some((name, id))
}

pub fn command_name(kind: &str, adapter: &str, id: &str) -> String {
    format!("{kind} \"{adapter}\" {id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Clone, Debug, Default)]
    struct Presets {
        #[serde(default)]
        exports: Vec<toml::Value>,
    }

    impl Extra for Presets {
        fn adds(&self) -> Vec<String> {
            vec![format!("{} export formats", self.exports.len())]
        }
        fn without_program(&self) -> bool {
            !self.exports.is_empty()
        }
        const ALONE: &'static str = "`[[exports]]`";
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("core9000-adapters-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_apps_own_sections() {
        let m: Manifest<Presets> =
            parse_manifest("name = \"P\"\n[[exports]]\nid = \"gif\"\n", "x").unwrap();
        assert_eq!(m.extra.exports.len(), 1);
        assert!(summary(&m).iter().any(|l| l.contains("1 export formats")));
        let e = parse_manifest::<Presets>("name = \"Empty\"\n", "x").unwrap_err();
        assert!(e.contains("`[[exports]]`"), "{e}");
        assert!(parse_manifest::<()>("name = \"Empty\"\n", "x").is_err());
        // Commands need a program even with presets.
        assert!(
            parse_manifest::<Presets>(
                "name = \"P\"\n[[exports]]\nid = \"g\"\n[[commands]]\nid = \"c\"\nlabel = \"C\"\n",
                "x"
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_other_sisters_adapters_and_newer_ones() {
        let me = crate::app().name;
        assert!(
            parse_manifest::<()>(
                &format!("name = \"A\"\nrun = [\"x\"]\napps = [\"wave9000\", \"{me}\"]\n"),
                "x"
            )
            .is_ok()
        );
        let e = parse_manifest::<()>("name = \"A\"\nrun = [\"x\"]\napps = [\"wave9000\"]\n", "x")
            .unwrap_err();
        assert!(e.contains("for wave9000, not"), "{e}");
        assert!(
            parse_manifest::<()>("name = \"A\"\nrun = [\"x\"]\nrequires = \"999.0\"\n", "x")
                .unwrap_err()
                .contains("or newer")
        );
        assert!(new_enough("0.0.10", "0.0.9") && !new_enough("0.0.9", "0.0.10"));
    }

    #[test]
    fn installs_runs_and_removes() {
        let dir = temp("run");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        // An adapter in sh: answers `initialize` by asking for the sisters.
        std::fs::write(
            src.join("adapter.toml"),
            "name = \"Echo\"\nrun = [\"sh\", \"echo.sh\"]\n[[commands]]\nid = \"go\"\nlabel = \"Go\"\n",
        )
        .unwrap();
        std::fs::write(src.join("echo.sh"), "read init\necho '{\"id\":1,\"method\":\"sisters.list\"}'\nread cmd\ncase \"$cmd\" in *go*) echo '{\"method\":\"log\",\"params\":{\"message\":\"got go\"}}';; esac\nread x\n").unwrap();
        let config = dir.join("config");
        let m: Manifest = install(&src, &config).unwrap();
        remember_approval(&config, &m);
        let (tx, rx) = channel();
        let mut host: Host = Host::new(
            Env {
                config: config.clone(),
                ..Env::default()
            },
            move |m| {
                let _ = tx.send(m);
            },
        );
        assert_eq!(host.all.len(), 1);
        host.run_command("Echo", "go").unwrap();
        let mut seen = Vec::new();
        while let Ok(Message::Call { method, params, .. }) = rx.recv_timeout(Duration::from_secs(5))
        {
            seen.push(method.clone());
            if method == "log" {
                assert_eq!(params["message"], "got go");
                break;
            }
        }
        assert_eq!(seen, ["sisters.list", "log"]);
        host.remove("Echo").unwrap();
        assert!(host.all.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

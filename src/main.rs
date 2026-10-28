//! lemonade-engines: add engines to a stock Lemonade server.
//!
//! Each engine is an OpenAI-compatible server on 127.0.0.1. This program installs it (or attaches to one
//! that already runs), registers it with Lemonade as a provider through the `lemonade` CLI, and tests it
//! with a chat sent through Lemonade. Lemonade itself is not modified.
//!
//! Env: LE_LISTEN (127.0.0.1:13500), LE_CATALOG (built-in assets/engines.toml), LE_HOME (~/.local/share/lemonade-engines),
//!      LEMONADE_HOST / LEMONADE_PORT (127.0.0.1 / 13305), LEMONADE_API_KEY (passed through to Lemonade when set).

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, process::Command, sync::Mutex};

const PAGE: &str = include_str!("../assets/index.html");
const DEFAULT_CATALOG: &str = include_str!("../assets/engines.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Catalog {
    engine: Vec<Engine>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Engine {
    id: String,
    name: String,
    summary: String,
    licence: String,
    #[serde(default)]
    licence_url: Option<String>,
    #[serde(default)]
    accept_licence: bool,
    #[serde(default)]
    hardware: Option<String>,
    mode: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    test_model: Option<String>,
    #[serde(default)]
    exe: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    files: Vec<FileSpec>,
    #[serde(default)]
    preload: Option<Value>,
    /// Keys of `preload` replaced on a given platform (e.g. a Windows command line), keyed like `exe_platform`.
    #[serde(default)]
    preload_platform: std::collections::BTreeMap<String, Value>,
    /// Posted to the engine's /api/v1/pull before `preload`, to register a model it does not list yet.
    #[serde(default)]
    pull: Option<Value>,
    #[serde(default)]
    get_url: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// Environment for install commands, plan, start and stop; {dir} and {port} are filled in.
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    /// Command path per platform when it differs from `exe` (e.g. a .cmd on Windows).
    #[serde(default)]
    exe_platform: std::collections::BTreeMap<String, String>,
    /// Shown to the user and accepted before the first start (for engines that download a lot then).
    #[serde(default)]
    plan_args: Vec<String>,
    /// Run instead of killing the process on Stop (for engines that serve in the background).
    #[serde(default)]
    stop_args: Vec<String>,
    /// How long Start waits for /v1/models, in seconds (default 120).
    #[serde(default)]
    start_wait_s: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FileSpec {
    source: String,
    dest: String,
    #[serde(default)]
    sha256: Option<String>,
    /// e.g. "x86_64-linux", "x86_64-windows", "aarch64-macos"; files without one apply everywhere.
    #[serde(default)]
    platform: Option<String>,
    /// Command run after the file is in place (e.g. an installer script); {dir} is filled in.
    #[serde(default)]
    run: Vec<String>,
}

/// This computer, in the form catalogue files use: "<arch>-<os>".
fn this_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

impl Engine {
    /// Files to install here: the platform-neutral ones plus those built for this computer.
    fn files_here(&self) -> Vec<&FileSpec> {
        let here = this_platform();
        self.files.iter().filter(|f| f.platform.as_deref().map_or(true, |p| p == here)).collect()
    }
    /// A managed engine with platform-specific files is only available where one of them matches.
    fn available_here(&self) -> bool {
        let specific: Vec<_> = self.files.iter().filter_map(|f| f.platform.as_deref()).collect();
        specific.is_empty() || specific.contains(&this_platform().as_str())
    }
}

struct App {
    catalog: Catalog,
    home: PathBuf,
    lemonade: String,
    http: reqwest::Client,
    children: Mutex<HashMap<String, tokio::process::Child>>,
}

type Shared = Arc<App>;

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({"ok": false, "error": msg.into()}))).into_response()
}

impl App {
    fn engine(&self, id: &str) -> Option<&Engine> {
        self.catalog.engine.iter().find(|e| e.id == id)
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.home.join("engines").join(id)
    }

    /// Base URL (without /v1) of an engine's own server.
    fn base(&self, e: &Engine) -> Option<String> {
        match e.mode.as_str() {
            "attach" => e.url.clone(),
            _ => e.port.map(|p| format!("http://127.0.0.1:{p}")),
        }
    }

    fn fill(&self, e: &Engine, s: &str) -> String {
        s.replace("{dir}", &self.dir(&e.id).to_string_lossy()).replace("{port}", &e.port.unwrap_or(0).to_string())
    }
    fn exe_path(&self, e: &Engine) -> PathBuf {
        let rel = e.exe_platform.get(&this_platform()).or(e.exe.as_ref()).cloned().unwrap_or_else(|| e.id.clone());
        let mut exe = self.dir(&e.id).join(rel);
        if cfg!(windows) && exe.extension().is_none() { exe.set_extension("exe"); }
        exe
    }
    /// Where this engine's program is: `LE_<ID>_BIN` if set, else `exe` inside the engine folder, else a program
    /// of that name on PATH. Engines we may not redistribute (Cake) are found this way instead of being bundled.
    fn find_bin(&self, e: &Engine) -> Result<PathBuf, String> {
        let var = format!("LE_{}_BIN", e.id.to_uppercase());
        if let Ok(p) = std::env::var(&var) {
            if !p.is_empty() {
                return if Path::new(&p).is_file() { Ok(PathBuf::from(p)) } else { Err(format!("{var} is set to {p}, which is not a file")) };
            }
        }
        let own = self.exe_path(e);
        if own.is_file() { return Ok(own); }
        let name = own.file_name().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&e.id));
        if let Some(paths) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&paths) {
                let c = d.join(&name);
                if c.is_file() { return Ok(c); }
            }
        }
        Err(format!("{} is not installed here. Put its program at {} or on PATH, or set {var} to its full path.", e.name, own.display()))
    }

    /// The preload body for this computer, with {bin} and {dir} filled in; {model}, {port} and {worker_port}
    /// are left for the engine itself.
    fn preload_body(&self, e: &Engine) -> Result<Option<Value>, String> {
        let Some(base) = &e.preload else { return Ok(None) };
        let mut body = base.clone();
        if let (Some(o), Some(Value::Object(over))) = (body.as_object_mut(), e.preload_platform.get(&this_platform())) {
            for (k, v) in over { o.insert(k.clone(), v.clone()); }
        }
        let needs_bin = body.to_string().contains("{bin}");
        let bin = if needs_bin { self.find_bin(e)?.to_string_lossy().to_string() } else { String::new() };
        let dir = self.dir(&e.id).to_string_lossy().to_string();
        fn fill(v: &mut Value, bin: &str, dir: &str) {
            match v {
                Value::String(s) => *s = s.replace("{bin}", bin).replace("{dir}", dir),
                Value::Array(a) => a.iter_mut().for_each(|x| fill(x, bin, dir)),
                Value::Object(o) => o.values_mut().for_each(|x| fill(x, bin, dir)),
                _ => {}
            }
        }
        fill(&mut body, &bin, &dir);
        Ok(Some(body))
    }

    /// A command for this engine: its folder as working directory and its environment filled in.
    fn command(&self, e: &Engine, program: impl AsRef<std::ffi::OsStr>, args: &[String]) -> Command {
        let mut c = Command::new(program);
        c.args(args.iter().map(|a| self.fill(e, a))).current_dir(self.dir(&e.id)).stdin(Stdio::null());
        for (k, v) in &e.env { c.env(k, self.fill(e, v)); }
        c
    }
    fn installed(&self, e: &Engine) -> bool {
        e.mode == "attach" || (e.available_here() && e.files_here().iter().all(|f| self.dir(&e.id).join(&f.dest).exists()))
    }

    async fn lemonade_cli(&self, args: &[&str]) -> Result<String, String> {
        let (host, port) = self.lemonade.trim_start_matches("http://").rsplit_once(':').unwrap_or(("127.0.0.1", "13305"));
        let out = Command::new("lemonade")
            .args(args)
            .env("LEMONADE_HOST", host)
            .env("LEMONADE_PORT", port)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| format!("could not run the lemonade CLI: {e}"))?;
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        if out.status.success() { Ok(text) } else { Err(text) }
    }

    async fn providers(&self) -> Vec<Value> {
        match self.lemonade_cli(&["cloud", "list", "--json"]).await {
            Ok(t) => serde_json::from_str::<Value>(t.trim())
                .ok()
                .and_then(|v| v.as_array().cloned().or_else(|| v.get("providers").and_then(Value::as_array).cloned()))
                .unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    fn lemonade_req(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match std::env::var("LEMONADE_API_KEY") {
            Ok(k) if !k.is_empty() => rb.bearer_auth(k),
            _ => rb,
        }
    }

    /// Models Lemonade lists for a provider (ids start with "<id>.").
    async fn lemonade_models(&self, id: &str) -> Vec<String> {
        let r = self.lemonade_req(self.http.get(format!("{}/api/v1/models", self.lemonade))).send().await;
        let Ok(r) = r else { return vec![] };
        let v: Value = r.json().await.unwrap_or(Value::Null);
        v["data"].as_array().map(|a| {
            a.iter().filter_map(|m| m["id"].as_str()).filter(|m| m.starts_with(&format!("{id}."))).map(String::from).collect()
        }).unwrap_or_default()
    }

    fn token(&self, id: &str) -> String {
        let p = self.dir(id).join("provider-token");
        if let Ok(t) = std::fs::read_to_string(&p) {
            return t.trim().to_string();
        }
        // Engines here check no key, but Lemonade only discovers a provider's models once one is set.
        let t: String = (0..24).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect();
        let _ = std::fs::create_dir_all(self.dir(id));
        let _ = std::fs::write(&p, &t);
        t
    }
}

async fn page() -> Html<&'static str> {
    Html(PAGE)
}

async fn state(State(app): State<Shared>) -> Json<Value> {
    let health = app.lemonade_req(app.http.get(format!("{}/api/v1/health", app.lemonade))).timeout(Duration::from_secs(3)).send().await;
    let (lemonade_ok, version) = match health {
        Ok(r) if r.status().is_success() => (true, r.json::<Value>().await.ok().and_then(|v| v["version"].as_str().map(String::from))),
        Ok(r) => (false, Some(format!("HTTP {}", r.status()))),
        Err(e) => (false, Some(if e.is_timeout() { "no answer".into() } else { "not reachable".into() })),
    };
    let providers = app.providers().await;
    let mut engines = vec![];
    for e in &app.catalog.engine {
        let base = app.base(e);
        let up = match &base {
            Some(b) => app.http.get(format!("{b}/v1/models")).timeout(Duration::from_secs(2)).send().await.map(|r| r.status().is_success()).unwrap_or(false),
            None => false,
        };
        let listed = providers.iter().any(|p| p["name"].as_str() == Some(&e.id));
        let models = if listed { app.lemonade_models(&e.id).await } else { vec![] };
        // Lemonade keeps the provider across restarts but forgets its runtime key, and then lists no models.
        // Count that as not registered, so Set up runs "Add to Lemonade" (which sets the key) again.
        let registered = listed && !models.is_empty();
        let running_here = app.children.lock().await.contains_key(&e.id);
        engines.push(json!({
            "engine": e, "installed": app.installed(e), "up": up, "running_here": running_here,
            "registered": registered, "models": models, "base": base,
            "accepted": app.dir(&e.id).join("licence-accepted").exists(),
            "available": e.mode == "attach" || e.available_here(),
            "plan_accepted": e.plan_args.is_empty() || app.dir(&e.id).join("plan-accepted").exists(),
        }));
    }
    Json(json!({"lemonade": {"url": app.lemonade, "ok": lemonade_ok, "version": version}, "platform": this_platform(), "engines": engines}))
}

#[derive(Deserialize, Default)]
struct InstallReq {
    #[serde(default)]
    accept_licence: bool,
}

#[derive(Debug, Default, Deserialize)]
struct StartReq {
    #[serde(default)]
    accept_plan: bool,
}

async fn copy_path(src: &Path, dst: &Path) -> Result<(), String> {
    if src.is_dir() {
        let st = Command::new("cp").arg("-a").arg(src).arg(dst).status().await.map_err(|e| e.to_string())?;
        if !st.success() { return Err(format!("copy of {} failed", src.display())); }
        Ok(())
    } else {
        tokio::fs::copy(src, dst).await.map(|_| ()).map_err(|e| format!("copy of {}: {e}", src.display()))
    }
}

/// Extract a .zip archive into `dir`. Entries whose path would escape `dir` are refused.
fn unpack_zip(archive: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("opening {}: {e}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("reading {}: {e}", archive.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("reading entry {i} of {}: {e}", archive.display()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!("{}: entry {i} has a path outside the archive root", archive.display()));
        };
        let out = dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        let mut out_file = std::fs::File::create(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
        let written = std::io::copy(&mut entry, &mut out_file).map_err(|e| format!("writing {}: {e}", out.display()))?;
        if written != entry.size() {
            return Err(format!("{}: wrote {written} bytes, expected {}", out.display(), entry.size()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let exec = mode & 0o111;
                if exec != 0 {
                    let mut perms = out_file.metadata().map_err(|e| e.to_string())?.permissions();
                    // Keep the entry's execute bits, without the file-type bits of the current mode.
                    perms.set_mode((perms.mode() & 0o7777 & !0o111) | exec);
                    std::fs::set_permissions(&out, perms).map_err(|e| format!("setting permissions on {}: {e}", out.display()))?;
                }
            }
        }
    }
    Ok(())
}

/// Unpack a downloaded archive into `dir`: .tar.gz through tar, .zip through unpack_zip.
fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.ends_with(".tar.gz") {
        let st = std::process::Command::new("tar")
            .arg("xzf")
            .arg(archive)
            .arg("-C")
            .arg(dir)
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(format!("unpacking {name} failed"));
        }
        Ok(())
    } else if name.ends_with(".zip") {
        unpack_zip(archive, dir)
    } else {
        Err(format!("unsupported archive {name}"))
    }
}

async fn fetch(app: &App, e: &Engine, f: &FileSpec, dir: &Path) -> Result<(), String> {
    let dst = dir.join(&f.dest);
    if let Some(p) = dst.parent() { tokio::fs::create_dir_all(p).await.map_err(|e| e.to_string())?; }
    if let Some(path) = f.source.strip_prefix("file://") {
        copy_path(Path::new(path), &dst).await?;
    } else if f.source.starts_with("https://") {
        // Stream to a temporary file while hashing, so a 1 GB model never sits in memory;
        // the file only takes its real name once the checksum (when given) matches.
        let mut r = app.http.get(&f.source).send().await.map_err(|e| format!("download {}: {e}", f.source))?;
        if !r.status().is_success() { return Err(format!("download {}: HTTP {}", f.source, r.status())); }
        let tmp = dst.with_extension("part");
        let mut out = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = r.chunk().await.map_err(|e| format!("download {}: {e}", f.source))? {
            hasher.update(&chunk);
            out.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        out.flush().await.map_err(|e| e.to_string())?;
        drop(out);
        if let Some(want) = &f.sha256 {
            let got = format!("{:x}", hasher.finalize());
            if !got.eq_ignore_ascii_case(want) {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(format!("{}: sha256 {got} does not match {want}", f.source));
            }
        }
        tokio::fs::rename(&tmp, &dst).await.map_err(|e| e.to_string())?;
    } else {
        return Err(format!("unsupported source {}", f.source));
    }
    if f.dest.ends_with(".tar.gz") {
        let st = Command::new("tar").arg("xzf").arg(&dst).arg("-C").arg(dir).status().await.map_err(|e| e.to_string())?;
        if !st.success() { return Err(format!("unpacking {} failed", f.dest)); }
        // Keep an empty marker under the archive's name (install skips files that exist) instead of a second copy on disk.
        tokio::fs::write(&dst, b"").await.map_err(|e| e.to_string())?;
    }
    if f.dest.ends_with(".zip") {
        let archive = dst.clone();
        let target = dir.to_path_buf();
        let result = tokio::task::spawn_blocking(move || unpack(&archive, &target)).await.unwrap_or_else(|e| Err(e.to_string()));
        if let Err(m) = result {
            // Remove the archive so the next Install unpacks it again instead of skipping it.
            let _ = tokio::fs::remove_file(&dst).await;
            return Err(format!("unpacking {} failed: {m}", f.dest));
        }
        // Keep an empty marker under the archive's name (install skips files that exist) instead of a second copy on disk.
        tokio::fs::write(&dst, b"").await.map_err(|e| e.to_string())?;
    }
    if let Some((prog, args)) = f.run.split_first() {
        let log = dir.join("install.log");
        let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let st = app.command(e, app.fill(e, prog), args).stdout(out.try_clone().map_err(|e| e.to_string())?).stderr(out).status().await;
        if !matches!(st, Ok(s) if s.success()) {
            // Remove the file so the next Install runs the command again.
            let _ = tokio::fs::remove_file(&dst).await;
            return Err(format!("{} failed: {}", f.dest, tail(&log, 3)));
        }
    }
    Ok(())
}

/// Last `n` non-empty lines of a log file, joined with " | ".
fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

async fn install(State(app): State<Shared>, UrlPath(id): UrlPath<String>, body: Option<Json<InstallReq>>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let req = body.map(|b| b.0).unwrap_or_default();
    let dir = app.dir(&id);
    if e.accept_licence {
        if !req.accept_licence && !dir.join("licence-accepted").exists() {
            return err(StatusCode::FORBIDDEN, format!("{} is under {}. Accept its licence to continue.", e.name, e.licence));
        }
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("licence-accepted"), &e.licence);
    }
    if e.mode == "attach" {
        return Json(json!({"ok": true, "message": "Nothing to install: this engine runs as its own server. Start it, then register it."})).into_response();
    }
    if !e.available_here() {
        return err(StatusCode::NOT_IMPLEMENTED, format!("{} has no build for this computer ({}) yet.", e.name, this_platform()));
    }
    let t0 = Instant::now();
    for f in e.files_here() {
        if dir.join(&f.dest).exists() { continue; }
        if let Err(m) = fetch(&app, &e, f, &dir).await { return err(StatusCode::BAD_GATEWAY, m); }
    }
    Json(json!({"ok": true, "message": format!("Installed in {} ({:.1} s)", dir.display(), t0.elapsed().as_secs_f32())})).into_response()
}

async fn wait_up(app: &App, base: &str, secs: u64) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if let Ok(r) = app.http.get(format!("{base}/v1/models")).timeout(Duration::from_secs(2)).send().await {
            if r.status().is_success() { return true; }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn start(State(app): State<Shared>, UrlPath(id): UrlPath<String>, body: Option<Json<StartReq>>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let Some(base) = app.base(&e) else { return err(StatusCode::BAD_REQUEST, "engine has no address") };
    if e.mode == "attach" {
        return if wait_up(&app, &base, 3).await {
            Json(json!({"ok": true, "message": format!("Found it running at {base}")})).into_response()
        } else {
            err(StatusCode::BAD_GATEWAY, format!("Nothing answers at {base}/v1/models. Start it with its own installer first."))
        };
    }
    if !app.installed(&e) { return err(StatusCode::CONFLICT, "Install it first."); }
    if wait_up(&app, &base, 1).await {
        return Json(json!({"ok": true, "message": format!("Already running at {base}")})).into_response();
    }
    let dir = app.dir(&id);
    if !e.plan_args.is_empty() && !dir.join("plan-accepted").exists() {
        if !body.map(|b| b.0).unwrap_or_default().accept_plan {
            return (StatusCode::CONFLICT, Json(json!({"ok": false, "needs_plan": true, "error": "Review what it will download first."}))).into_response();
        }
        let _ = std::fs::write(dir.join("plan-accepted"), "");
    }
    let exe = app.exe_path(&e);
    #[cfg(unix)]
    let _ = Command::new("chmod").arg("+x").arg(&exe).status().await;
    let log = match std::fs::File::create(dir.join("engine.log")) { Ok(f) => f, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) };
    let mut cmd = app.command(&e, &exe, &e.args);
    cmd.stdout(log.try_clone().unwrap()).stderr(log).kill_on_drop(e.stop_args.is_empty());
    // Own process group, so stopping the engine also stops anything it started.
    #[cfg(unix)]
    cmd.process_group(0);
    let child = match cmd.spawn() { Ok(c) => c, Err(x) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("could not start {}: {x}", exe.display())) };
    app.children.lock().await.insert(id.clone(), child);
    let wait = e.start_wait_s.unwrap_or(120);
    let t0 = Instant::now();
    loop {
        if wait_up(&app, &base, 1).await {
            return Json(json!({"ok": true, "message": format!("Running at {base}")})).into_response();
        }
        // A launcher may hand the server to a background service and exit 0; keep waiting then.
        let exited = {
            let mut kids = app.children.lock().await;
            match kids.get_mut(&id).map(|c| c.try_wait()) {
                Some(Ok(Some(st))) => { kids.remove(&id); Some(st.success()) }
                None => Some(false), // stopped from the page while starting
                _ => None,
            }
        };
        if exited == Some(false) {
            return err(StatusCode::BAD_GATEWAY, format!("It stopped before answering: {}", tail(&dir.join("engine.log"), 3)));
        }
        if t0.elapsed().as_secs() >= wait {
            return err(StatusCode::GATEWAY_TIMEOUT, format!("Started, but {base}/v1/models did not answer within {wait} s. See {}/engine.log", dir.display()));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// TERM the engine's process group, give it a few seconds, then KILL what is left.
async fn stop_child(mut c: tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = c.id() {
        let group = format!("-{pid}");
        let _ = Command::new("kill").args(["-TERM", "--", &group]).status().await;
        for _ in 0..50 {
            if matches!(c.try_wait(), Ok(Some(_))) { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = Command::new("kill").args(["-KILL", "--", &group]).stderr(Stdio::null()).status().await;
    }
    // Windows has no process groups here: kill the whole tree, or a server's own children (e.g. the llama-server
    // behind a Lemonade) keep running after Stop.
    #[cfg(windows)]
    if let Some(pid) = c.id() {
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    }
    let _ = c.kill().await;
    let _ = c.wait().await;
}

async fn stop(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let child = app.children.lock().await.remove(&id);
    let had_child = child.is_some();
    if let Some(c) = child { stop_child(c).await; }
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    if !e.stop_args.is_empty() {
        let out = app.command(&e, app.exe_path(&e), &e.stop_args).output().await;
        return match out {
            Ok(o) if o.status.success() => Json(json!({"ok": true, "message": "Stopped."})).into_response(),
            Ok(o) => err(StatusCode::BAD_GATEWAY, format!("stop failed: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(x) => err(StatusCode::INTERNAL_SERVER_ERROR, x.to_string()),
        };
    }
    if had_child { Json(json!({"ok": true, "message": "Stopped."})).into_response() } else { err(StatusCode::NOT_FOUND, "Not started by lemonade-engines.") }
}

async fn shutdown_signal() {
    let ctrl_c = async { let _ = tokio::signal::ctrl_c().await; };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) { s.recv().await; }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

async fn plan(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    if e.plan_args.is_empty() { return Json(json!({"ok": true, "plan": null})).into_response(); }
    if !app.installed(&e) { return err(StatusCode::CONFLICT, "Install it first."); }
    let run = app.command(&e, app.exe_path(&e), &e.plan_args).output();
    match tokio::time::timeout(Duration::from_secs(300), run).await {
        Ok(Ok(o)) if o.status.success() => Json(json!({"ok": true, "plan": String::from_utf8_lossy(&o.stdout).trim()})).into_response(),
        Ok(Ok(o)) => err(StatusCode::BAD_GATEWAY, format!("plan failed: {}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Ok(Err(x)) => err(StatusCode::INTERNAL_SERVER_ERROR, x.to_string()),
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "plan took longer than 300 s"),
    }
}

/// Last lines of the engine's start log (or install log), so the page can show progress.
async fn log_tail(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id) else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let dir = app.dir(&e.id);
    let newest = ["engine.log", "install.log"].iter().map(|n| dir.join(n))
        .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|t| (t, p)))
        .max_by_key(|(t, _)| *t);
    let line = newest.map(|(_, p)| tail(&p, 1)).unwrap_or_default();
    Json(json!({"ok": true, "line": line})).into_response()
}

async fn register(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let Some(base) = app.base(&e) else { return err(StatusCode::BAD_REQUEST, "engine has no address") };
    if e.accept_licence && !app.dir(&id).join("licence-accepted").exists() {
        return err(StatusCode::FORBIDDEN, "Accept the licence first.");
    }
    let mut log = String::new();
    // Engines that need load options (e.g. Cake's port and command) are loaded on the engine first;
    // a request routed through Lemonade cannot pass them.
    let preload = match app.preload_body(&e) { Ok(b) => b, Err(m) => return err(StatusCode::CONFLICT, m) };
    if let Some(pull) = &e.pull {
        match app.http.post(format!("{base}/api/v1/pull")).json(pull).timeout(Duration::from_secs(3600)).send().await {
            Ok(r) if r.status().is_success() => log += "Model pulled on the engine. ",
            Ok(r) => {
                let t = r.text().await.unwrap_or_default();
                if !t.contains("already") { return err(StatusCode::BAD_GATEWAY, format!("Pull failed: {t}")); }
            }
            Err(x) => return err(StatusCode::BAD_GATEWAY, format!("Pull failed: {x}")),
        }
    }
    if let Some(body) = &preload {
        match app.http.post(format!("{base}/api/v1/load")).json(body).timeout(Duration::from_secs(900)).send().await {
            Ok(r) if r.status().is_success() => log += "Preloaded on the engine. ",
            Ok(r) => return err(StatusCode::BAD_GATEWAY, format!("Preload failed: HTTP {} {}", r.status(), r.text().await.unwrap_or_default())),
            Err(x) => return err(StatusCode::BAD_GATEWAY, format!("Preload failed: {x}")),
        }
    }
    let url = format!("{base}/v1");
    let insecure = url.starts_with("http://");
    let mut args = vec!["cloud", "install", id.as_str(), "--base-url", url.as_str()];
    if insecure { args.push("--allow-insecure-http"); }
    if let Err(m) = app.lemonade_cli(&args).await {
        if !m.contains("already") { return err(StatusCode::BAD_GATEWAY, format!("lemonade cloud install failed: {m}")); }
    }
    let token = app.token(&id);
    let mut auth = vec!["cloud", "auth", id.as_str(), "--api-key", token.as_str()];
    if insecure { auth.push("--allow-insecure-http"); }
    if let Err(m) = app.lemonade_cli(&auth).await { return err(StatusCode::BAD_GATEWAY, format!("lemonade cloud auth failed: {m}")); }
    let models = app.lemonade_models(&id).await;
    log += &format!("Registered as provider '{id}'. Lemonade lists {} model(s).", models.len());
    Json(json!({"ok": true, "message": log, "models": models})).into_response()
}

async fn unregister(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    match app.lemonade_cli(&["cloud", "uninstall", &id]).await {
        Ok(_) => Json(json!({"ok": true, "message": format!("Removed provider '{id}' from Lemonade.")})).into_response(),
        Err(m) => err(StatusCode::BAD_GATEWAY, m),
    }
}

async fn test(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let models = app.lemonade_models(&id).await;
    let model = match &e.test_model {
        Some(m) if models.iter().any(|x| x == &format!("{id}.{m}")) => format!("{id}.{m}"),
        _ => match models.first() { Some(m) => m.clone(), None => return err(StatusCode::CONFLICT, "Lemonade lists no models for this engine. Register it first.") },
    };
    let t0 = Instant::now();
    let r = app.lemonade_req(app.http.post(format!("{}/api/v1/chat/completions", app.lemonade)))
        .json(&json!({"model": model, "max_tokens": 48, "messages": [{"role": "user", "content": "In one sentence, what is a lighthouse for?"}]}))
        .timeout(Duration::from_secs(900)).send().await;
    let secs = t0.elapsed().as_secs_f32();
    match r {
        Ok(r) => {
            let status = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if !status.is_success() { return err(StatusCode::BAD_GATEWAY, format!("Lemonade answered HTTP {status}: {v}")); }
            let answer = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
            Json(json!({"ok": true, "model": model, "answer": answer, "seconds": secs, "served_by": v["model"], "usage": v["usage"]})).into_response()
        }
        Err(x) => err(StatusCode::BAD_GATEWAY, format!("Chat through Lemonade failed: {x}")),
    }
}

/// One click: double-clicking the binary opens the page. Failure to open is not fatal; the address is printed.
fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        println!("could not open a browser ({e}); open {url} yourself");
    }
}

#[tokio::main]
async fn main() {
    let env = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
    let catalog_text = match std::env::var("LE_CATALOG") {
        Ok(p) => std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("LE_CATALOG {p}: {e}")),
        Err(_) => DEFAULT_CATALOG.to_string(),
    };
    let catalog: Catalog = toml::from_str(&catalog_text).expect("engine catalogue does not parse");
    let home = PathBuf::from(env("LE_HOME", &format!("{}/.local/share/lemonade-engines", env("HOME", "."))));
    let lemonade = format!("http://{}:{}", env("LEMONADE_HOST", "127.0.0.1"), env("LEMONADE_PORT", "13305"));
    let app = Arc::new(App { catalog, home, lemonade, http: reqwest::Client::new(), children: Mutex::new(HashMap::new()) });
    let router = Router::new()
        .route("/", get(page))
        .route("/api/state", get(state))
        .route("/api/engines/{id}/install", post(install))
        .route("/api/engines/{id}/start", post(start))
        .route("/api/engines/{id}/stop", post(stop))
        .route("/api/engines/{id}/register", post(register))
        .route("/api/engines/{id}/unregister", post(unregister))
        .route("/api/engines/{id}/test", post(test))
        .route("/api/engines/{id}/plan", post(plan))
        .route("/api/engines/{id}/log", get(log_tail))
        .with_state(app.clone());
    let addr: SocketAddr = env("LE_LISTEN", "127.0.0.1:13500").parse().expect("LE_LISTEN");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    println!("lemonade-engines on http://{addr}");
    if std::env::var_os("LE_NO_BROWSER").is_none() {
        open_browser(&format!("http://{addr}"));
    }
    axum::serve(listener, router).with_graceful_shutdown(shutdown_signal()).await.expect("serve");
    // Engines started here stop with lemonade-engines; a stopped add-on must not leave servers running.
    let children: Vec<_> = app.children.lock().await.drain().collect();
    for (id, c) in children {
        println!("stopping engine {id}");
        stop_child(c).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn platform_filter_picks_this_computer() {
        let here = super::this_platform();
        let e: super::Engine = toml::from_str(&format!(r#"
            id = "x"
            name = "x"
            summary = "x"
            licence = "x"
            mode = "managed"
            [[files]]
            source = "https://a/common"
            dest = "common"
            [[files]]
            source = "https://a/here"
            dest = "here"
            platform = "{here}"
            [[files]]
            source = "https://a/other"
            dest = "other"
            platform = "riscv64-plan9"
        "#)).unwrap();
        let names: Vec<_> = e.files_here().iter().map(|f| f.dest.as_str()).collect();
        assert_eq!(names, ["common", "here"]);
        assert!(e.available_here());
        let only_other: super::Engine = toml::from_str(r#"
            id = "y"
            name = "y"
            summary = "y"
            licence = "y"
            mode = "managed"
            [[files]]
            source = "https://a/other"
            dest = "other"
            platform = "riscv64-plan9"
        "#).unwrap();
        assert!(!only_other.available_here());
    }

    #[test]
    fn builtin_catalog_parses_and_is_consistent() {
        let c: Catalog = toml::from_str(DEFAULT_CATALOG).unwrap();
        assert!(c.engine.len() >= 4);
        for e in &c.engine {
            match e.mode.as_str() {
                "attach" => assert!(e.url.is_some(), "{} attach needs url", e.id),
                "managed" => {
                    assert!(e.port.is_some() && e.exe.is_some() && !e.files.is_empty(), "{} managed needs port, exe, files", e.id);
                    assert!(e.args.iter().any(|a| a.contains("{port}")), "{} args should use {{port}}", e.id);
                }
                m => panic!("{}: unknown mode {m}", e.id),
            }
            assert!(e.id.chars().all(|c| c.is_ascii_alphanumeric()), "{}: provider names should be plain", e.id);
        }
        let burn = c.engine.iter().find(|e| e.id == "burn").unwrap();
        for p in ["x86_64-linux", "x86_64-windows", "x86_64-macos", "aarch64-macos"] {
            assert!(burn.files.iter().any(|f| f.platform.as_deref() == Some(p)), "burn has no server for {p}");
        }
        assert!(burn.files.iter().all(|f| f.source.starts_with("https://") && f.sha256.as_ref().map_or(false, |h| h.len() == 64)), "burn files must be pinned downloads");
        let cake = c.engine.iter().find(|e| e.id == "cake").unwrap();
        assert!(cake.accept_licence, "Cake is non-commercial: it must ask first");
    }

    /// A directory under the system temp dir, removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("lemonade-engines-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("could not create the temporary directory");
            TempDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn zip_unpacks_files_and_dirs() {
        let tmp = TempDir::new("zip-files");
        let archive = tmp.path().join("build.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer.start_file("llama-server.exe", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"exe").unwrap();
        writer.add_directory("sub", zip::write::SimpleFileOptions::default()).unwrap();
        writer.start_file("sub/llama.dll", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"dll").unwrap();
        writer.finish().unwrap();

        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        unpack(&archive, &out).unwrap();

        assert_eq!(std::fs::read(out.join("llama-server.exe")).unwrap(), b"exe");
        assert_eq!(std::fs::read(out.join("sub/llama.dll")).unwrap(), b"dll");
        assert!(out.join("sub").is_dir());
    }

    #[test]
    fn zip_rejects_path_escape() {
        let tmp = TempDir::new("zip-escape");
        let archive = tmp.path().join("evil.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer.start_file("../evil", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"nope").unwrap();
        writer.finish().unwrap();

        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let result = unpack(&archive, &out);
        assert!(result.is_err(), "an entry that escapes the target directory must be refused");
        assert!(!tmp.path().join("evil").exists(), "nothing may be written outside the target directory");
    }

    #[test]
    fn zip_real_prism_build() {
        let Ok(path) = std::env::var("LE_TEST_ZIP") else { return };
        let path = PathBuf::from(path);
        if !path.exists() { return; }
        let tmp = TempDir::new("zip-prism");
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        unpack(&path, &out).unwrap();
        assert!(out.join("llama-server.exe").exists(), "llama-server.exe is missing from {}", path.display());
        assert!(out.join("llama.dll").exists(), "llama.dll is missing from {}", path.display());
    }

    fn engine_with_preload() -> (App, Engine) {
        let e: Engine = toml::from_str(r#"
            id = "cake"
            name = "Cake"
            summary = "x"
            licence = "x"
            mode = "attach"
            exe = "bin/cake"
            preload = { model_name = "m", cake_args = "\"{bin}\" serve {model} --api 127.0.0.1:{port}" }
            preload_platform = { "nowhere-nothing" = { cake_args = "never" } }
        "#).unwrap();
        let home = std::env::temp_dir().join(format!("le-preload-{}", std::process::id()));
        let app = App { catalog: Catalog { engine: vec![e.clone()] }, home, lemonade: String::new(), http: reqwest::Client::new(), children: Mutex::new(HashMap::new()) };
        (app, e)
    }

    #[test]
    fn preload_uses_the_cake_program_the_user_has_not_a_fixed_path() {
        let (app, e) = engine_with_preload();
        let fake = app.dir("cake").join("bin");
        std::fs::create_dir_all(&fake).unwrap();
        let exe = if cfg!(windows) { "cake.exe" } else { "cake" };
        std::fs::write(fake.join(exe), b"").unwrap();
        let body = app.preload_body(&e).unwrap().unwrap();
        let args = body["cake_args"].as_str().unwrap();
        assert!(args.contains(&fake.join(exe).to_string_lossy().to_string()), "{args}");
        assert!(!args.contains("/home/cesarops"), "no machine-specific path may be baked in");
        assert!(args.contains("{model}") && args.contains("{port}"), "the engine fills these in itself");
        let _ = std::fs::remove_dir_all(&app.home);
    }

    #[test]
    fn preload_says_what_is_missing_when_cake_is_not_there() {
        let (app, e) = engine_with_preload();
        let m = app.preload_body(&e).unwrap_err();
        assert!(m.contains("LE_CAKE_BIN") && m.contains("not installed"), "{m}");
    }

    #[test]
    fn tar_gz_still_unpacks() {
        let tmp = TempDir::new("tar-gz");
        std::fs::write(tmp.path().join("hello.txt"), b"hello").unwrap();
        let archive = tmp.path().join("hello.tar.gz");
        let st = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("hello.txt")
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(st.success(), "tar could not create the test archive");

        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        unpack(&archive, &out).unwrap();
        assert_eq!(std::fs::read(out.join("hello.txt")).unwrap(), b"hello");
    }
}

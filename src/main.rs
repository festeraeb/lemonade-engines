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
    /// What the test sends: "chat" (default), "embeddings" (a /v1/embeddings call through Lemonade) or "vision"
    /// (a read of the engine's own /v1/models, for engines that run compiled vision graphs and cannot chat).
    #[serde(default)]
    kind: Option<String>,
    /// Set while the release the pinned files point at is not published yet; the page says so.
    #[serde(default)]
    pending: Option<String>,
    /// Heading the page lists this engine under (e.g. "Bitter Lemons").
    #[serde(default)]
    group: Option<String>,
    /// Plain-text blurb shown with the group heading (taken from the first engine of the group that has one).
    #[serde(default)]
    group_note: Option<String>,
    /// USB devices ("vid:pid", hex) of which one must be plugged in. Checked first; Linux only.
    #[serde(default)]
    requires_usb: Vec<String>,
    /// A file the user has to supply (we do not ship it): copied from `env` on install, or reported as missing.
    #[serde(default)]
    user_file: Option<UserFile>,
    /// Third-party firmware we must not redistribute: shown to the user, downloaded only after an explicit accept.
    #[serde(default)]
    firmware: Option<Firmware>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UserFile {
    /// Path inside the engine folder.
    dest: String,
    /// Environment variable that names a file to copy there.
    env: String,
    /// Said when the file is missing.
    help: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Firmware {
    /// Where the vendor published it, shown to the user.
    description: String,
    /// Download address of the archive and its sha256.
    source: String,
    sha256: String,
    /// The one file wanted from the (tar.gz) archive, its md5 and where it is saved inside the engine folder.
    member: String,
    md5: String,
    dest: String,
}

impl Engine {
    fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or("chat")
    }
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
        e.mode == "attach"
            || (e.available_here()
                && e.files_here().iter().all(|f| self.dir(&e.id).join(&f.dest).exists())
                && e.user_file.as_ref().map_or(true, |u| self.dir(&e.id).join(&u.dest).is_file()))
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


/// Plugged-in USB devices as "vid:pid" (lower-case hex), read from sysfs. Empty where that is not available.
fn usb_devices() -> Vec<String> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir("/sys/bus/usb/devices") else { return out };
    for d in rd.flatten() {
        let read = |n: &str| std::fs::read_to_string(d.path().join(n)).ok().map(|t| t.trim().to_lowercase());
        if let (Some(v), Some(p)) = (read("idVendor"), read("idProduct")) { out.push(format!("{v}:{p}")); }
    }
    out
}

/// Which of the wanted "vid:pid" devices are plugged in.
fn usb_found(want: &[String]) -> Vec<String> {
    let here = usb_devices();
    want.iter().map(|w| w.to_lowercase()).filter(|w| here.contains(w)).collect()
}

fn md5_hex(path: &Path) -> Result<String, String> {
    use md5::{Digest as _, Md5};
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Firmware {
    /// What the user is shown before anything is downloaded.
    fn plan_text(&self, name: &str) -> String {
        format!(
            "{name} needs a vendor firmware file that this program does not contain and may not hand out.\n\n{}\n\nDownload: {}\nArchive sha256: {}\nFile wanted: {} (md5 {})\n\nPress \"Download and start\" to fetch it from that address now. Both checksums are checked, and the file is kept only if they match.",
            self.description, self.source, self.sha256, self.member, self.md5
        )
    }
}

/// Download the firmware archive, check its sha256, take out the one wanted file, check its md5 and save it.
async fn get_firmware(app: &App, e: &Engine, fw: &Firmware) -> Result<(), String> {
    let dir = app.dir(&e.id);
    let dest = dir.join(&fw.dest);
    if dest.is_file() && md5_hex(&dest).map_or(false, |m| m.eq_ignore_ascii_case(&fw.md5)) { return Ok(()); }
    let work = dir.join("firmware-download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|x| x.to_string())?;
    let archive = work.join("firmware.tar.gz");
    let mut r = app.http.get(&fw.source).send().await.map_err(|x| format!("download {}: {x}", fw.source))?;
    if !r.status().is_success() { return Err(format!("download {}: HTTP {}", fw.source, r.status())); }
    let mut out = tokio::fs::File::create(&archive).await.map_err(|x| x.to_string())?;
    let mut hasher = Sha256::new();
    while let Some(chunk) = r.chunk().await.map_err(|x| format!("download {}: {x}", fw.source))? {
        hasher.update(&chunk);
        out.write_all(&chunk).await.map_err(|x| x.to_string())?;
    }
    out.flush().await.map_err(|x| x.to_string())?;
    drop(out);
    let got = format!("{:x}", hasher.finalize());
    if !got.eq_ignore_ascii_case(&fw.sha256) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("the firmware archive's sha256 is {got}, expected {}. Nothing was kept.", fw.sha256));
    }
    let st = Command::new("tar").arg("xzf").arg(&archive).arg("-C").arg(&work).arg(&fw.member).status().await.map_err(|x| x.to_string())?;
    if !st.success() { return Err(format!("could not take {} out of the archive", fw.member)); }
    let member = work.join(&fw.member);
    let got = md5_hex(&member)?;
    if !got.eq_ignore_ascii_case(&fw.md5) {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("{} has md5 {got}, expected {}. Nothing was kept.", fw.member, fw.md5));
    }
    if let Some(p) = dest.parent() { std::fs::create_dir_all(p).map_err(|x| x.to_string())?; }
    std::fs::copy(&member, &dest).map_err(|x| format!("saving {}: {x}", dest.display()))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct FirmwareReq {
    #[serde(default)]
    accept: bool,
}

async fn firmware(State(app): State<Shared>, UrlPath(id): UrlPath<String>, body: Option<Json<FirmwareReq>>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let Some(fw) = e.firmware.clone() else { return Json(json!({"ok": true, "message": "This engine needs no firmware."})).into_response() };
    let dest = app.dir(&id).join(&fw.dest);
    if dest.is_file() { return Json(json!({"ok": true, "message": "Firmware already in place."})).into_response(); }
    if !body.map(|b| b.0).unwrap_or_default().accept {
        return err(StatusCode::FORBIDDEN, "The firmware is downloaded only after you accept it.");
    }
    match get_firmware(&app, &e, &fw).await {
        Ok(()) => Json(json!({"ok": true, "message": format!("Firmware downloaded and both checksums matched ({}).", fw.md5)})).into_response(),
        Err(m) => err(StatusCode::BAD_GATEWAY, m),
    }
}

/// Look for the stick (or whatever hardware `requires_usb` names) and say plainly if it is not there.
async fn usb(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    if e.requires_usb.is_empty() { return Json(json!({"ok": true, "message": "No USB device needed."})).into_response(); }
    if !cfg!(target_os = "linux") {
        return err(StatusCode::NOT_IMPLEMENTED, "USB detection here only works on Linux, and this engine only runs there.");
    }
    let found = usb_found(&e.requires_usb);
    if found.is_empty() {
        return err(StatusCode::CONFLICT, format!("No stick found. Looked for USB device {}. Plug it in and press Set up again.", e.requires_usb.join(" or ")));
    }
    Json(json!({"ok": true, "message": format!("Found USB device {}.", found.join(", "))})).into_response()
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
    // Probe every engine at the same time. A refused connection to a stopped engine takes about 2 s on Windows, so six
    // stopped engines probed one after another made this call take 12 s and the page showed "server answering" long
    // after Stop had worked.
    let mut probes = tokio::task::JoinSet::new();
    for (i, e) in app.catalog.engine.iter().enumerate() {
        let app = app.clone();
        let id = e.id.clone();
        let base = app.base(e);
        let listed = providers.iter().any(|p| p["name"].as_str() == Some(&e.id));
        probes.spawn(async move {
            let up = match &base {
                Some(b) => app.http.get(format!("{b}/v1/models")).timeout(Duration::from_secs(2)).send().await.map(|r| r.status().is_success()).unwrap_or(false),
                None => false,
            };
            let models = if listed { app.lemonade_models(&id).await } else { vec![] };
            (i, up, models, listed)
        });
    }
    let mut probed = vec![(false, vec![], false); app.catalog.engine.len()];
    while let Some(r) = probes.join_next().await {
        if let Ok((i, up, models, listed)) = r { probed[i] = (up, models, listed); }
    }
    let mut engines = vec![];
    for (i, e) in app.catalog.engine.iter().enumerate() {
        let base = app.base(e);
        let (up, models, listed) = probed[i].clone();
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
            "usb": if e.requires_usb.is_empty() { Value::Null } else { json!(usb_found(&e.requires_usb)) },
            "firmware_present": e.firmware.as_ref().map(|f| app.dir(&e.id).join(&f.dest).is_file()),
            "user_file_present": e.user_file.as_ref().map(|f| app.dir(&e.id).join(&f.dest).is_file()),
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
    if let Some(u) = &e.user_file {
        let dst = dir.join(&u.dest);
        if !dst.is_file() {
            let from = std::env::var(&u.env).ok().filter(|v| !v.is_empty());
            match from {
                Some(p) if Path::new(&p).is_file() => {
                    if let Some(d) = dst.parent() { let _ = std::fs::create_dir_all(d); }
                    if let Err(m) = std::fs::copy(&p, &dst) { return err(StatusCode::INTERNAL_SERVER_ERROR, format!("copying {p}: {m}")); }
                }
                Some(p) => return err(StatusCode::CONFLICT, format!("{} is set to {p}, which is not a file. {}", u.env, u.help)),
                None => return err(StatusCode::CONFLICT, format!("{} Or put the file at {}.", u.help, dst.display())),
            }
        }
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
    if let Some(fw) = &e.firmware {
        if !app.dir(&id).join(&fw.dest).is_file() {
            return (StatusCode::CONFLICT, Json(json!({"ok": false, "needs_plan": true, "error": "The firmware has not been downloaded yet. Review it and accept first."}))).into_response();
        }
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
    if let Some(fw) = &e.firmware {
        if !app.dir(&id).join(&fw.dest).is_file() { return Json(json!({"ok": true, "plan": fw.plan_text(&e.name)})).into_response(); }
    }
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

/// Cosine similarity of two equal-length vectors.
fn cosine(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

const EMBED_TEST: [&str; 3] = [
    "A lighthouse guides ships at night.",
    "A beacon on the coast helps boats find their way in the dark.",
    "The tax forms are due in April.",
];

async fn test(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let models = app.lemonade_models(&id).await;
    let model = match &e.test_model {
        Some(m) if models.iter().any(|x| x == &format!("{id}.{m}")) => format!("{id}.{m}"),
        _ => match models.first() { Some(m) => m.clone(), None => return err(StatusCode::CONFLICT, "Lemonade lists no models for this engine. Register it first.") },
    };
    match e.kind() {
        "embeddings" => test_embeddings(&app, &e, &model).await,
        "vision" => test_vision(&app, &e, &models).await,
        _ => test_chat(&app, &model).await,
    }
}

/// POST the three test sentences to `{root}/embeddings` (root is "<lemonade>/api/v1" or "<engine>/v1") and return
/// (dimensions, seconds, similar, different, model, usage), or why it failed.
async fn embed_once(app: &App, root: &str, model: &str, lemonade: bool) -> Result<Value, String> {
    let t0 = Instant::now();
    let rb = app.http.post(format!("{root}/embeddings")).json(&json!({"model": model, "input": EMBED_TEST})).timeout(Duration::from_secs(300));
    let r = (if lemonade { app.lemonade_req(rb) } else { rb }).send().await.map_err(|x| x.to_string())?;
    let secs = t0.elapsed().as_secs_f32();
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let m = v["error"]["message"].as_str().map(String::from).unwrap_or_else(|| v.to_string());
        return Err(format!("HTTP {status}: {m}"));
    }
    let vecs: Vec<Vec<f64>> = v["data"].as_array().map(|a| {
        a.iter().map(|d| d["embedding"].as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()).collect()
    }).unwrap_or_default();
    if vecs.len() != EMBED_TEST.len() || vecs.iter().any(|x| x.is_empty() || x.len() != vecs[0].len()) {
        return Err(format!("expected {} vectors of the same size, got: {}", EMBED_TEST.len(), v.to_string().chars().take(300).collect::<String>()));
    }
    Ok(json!({"dimensions": vecs[0].len(), "seconds": secs, "similar": cosine(&vecs[0], &vecs[1]), "different": cosine(&vecs[0], &vecs[2]), "served_by": v["model"], "usage": v["usage"]}))
}

/// Embeddings are not chat. Ask the engine itself for the vectors of three sentences (size, time, and whether the two
/// sentences about the same thing sit closer than the unrelated one), then ask the same through Lemonade and report
/// what Lemonade does: Lemonade 2026.43.3 does not forward /embeddings to cloud providers, which is its limit, not a
/// fault of the engine, so only the direct call decides pass or fail.
async fn test_embeddings(app: &App, e: &Engine, model: &str) -> Response {
    let Some(base) = app.base(e) else { return err(StatusCode::BAD_REQUEST, "engine has no address") };
    let own_id = model.split_once('.').map(|x| x.1).unwrap_or(model);
    let direct = match embed_once(app, &format!("{base}/v1"), own_id, false).await {
        Ok(v) => v,
        Err(m) => return err(StatusCode::BAD_GATEWAY, format!("Embeddings from the engine itself failed: {m}")),
    };
    let via = embed_once(app, &format!("{}/api/v1", app.lemonade), model, true).await;
    let (via_ok, via_msg) = match &via { Ok(v) => (true, format!("{} dimensions in {:.0} ms", v["dimensions"], v["seconds"].as_f64().unwrap_or(0.0) * 1000.0)), Err(m) => (false, m.clone()) };
    Json(json!({
        "ok": true, "kind": "embeddings", "model": own_id, "lemonade_model": model, "dimensions": direct["dimensions"], "count": EMBED_TEST.len(),
        "seconds": direct["seconds"], "served_by": direct["served_by"], "usage": direct["usage"],
        "similar": direct["similar"], "different": direct["different"], "sentences": EMBED_TEST,
        "via_lemonade_ok": via_ok, "via_lemonade": via_msg,
    })).into_response()
}

/// A vision-graph engine cannot chat, so the test reads its model list straight from the engine and also shows
/// what its chat route answers (HTTP 501 is by design, not a failure).
async fn test_vision(app: &App, e: &Engine, in_lemonade: &[String]) -> Response {
    let Some(base) = app.base(e) else { return err(StatusCode::BAD_REQUEST, "engine has no address") };
    let t0 = Instant::now();
    let r = app.http.get(format!("{base}/v1/models")).timeout(Duration::from_secs(10)).send().await;
    let secs = t0.elapsed().as_secs_f32();
    let r = match r { Ok(r) => r, Err(x) => return err(StatusCode::BAD_GATEWAY, format!("{base}/v1/models: {x}")) };
    let v: Value = r.json().await.unwrap_or(Value::Null);
    let Some(m) = v["data"].as_array().and_then(|a| a.first()).cloned() else {
        return err(StatusCode::BAD_GATEWAY, format!("{base}/v1/models lists no graph: {v}"));
    };
    let chat = app.http.post(format!("{base}/v1/chat/completions")).json(&json!({"model": m["id"], "messages": [{"role": "user", "content": "hi"}]}))
        .timeout(Duration::from_secs(10)).send().await.map(|r| r.status().as_u16()).ok();
    Json(json!({
        "ok": true, "kind": "vision", "model": m["id"], "generates_text": m["generates_text"], "graph": m,
        "seconds": secs, "in_lemonade": in_lemonade, "chat_status": chat,
    })).into_response()
}

async fn test_chat(app: &App, model: &str) -> Response {
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
        .route("/api/engines/{id}/usb", post(usb))
        .route("/api/engines/{id}/firmware", post(firmware))
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
        // One folder per call: the two preload tests run in parallel and one of them creates a fake Cake program.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("le-preload-{}-{n}", std::process::id()));
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
        // The launch line joins with "/" even on Windows, so compare with one separator.
        let want = fake.join(exe).to_string_lossy().replace('\\', "/");
        assert!(args.replace('\\', "/").contains(&want), "{args}");
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
    fn new_entries_are_pinned_and_gated() {
        let c: Catalog = toml::from_str(DEFAULT_CATALOG).unwrap();
        let tl = c.engine.iter().find(|e| e.id == "ternlight").unwrap();
        assert_eq!(tl.kind(), "embeddings");
        for p in ["x86_64-linux", "x86_64-windows", "x86_64-macos", "aarch64-macos"] {
            assert!(tl.files.iter().any(|f| f.platform.as_deref() == Some(p)), "ternlight has no build for {p}");
        }
        let mv = c.engine.iter().find(|e| e.id == "movidius").unwrap();
        assert_eq!(mv.kind(), "vision");
        assert_eq!(mv.group.as_deref(), Some("Bitter Lemons"));
        assert!(mv.requires_usb.contains(&"03e7:2150".to_string()) && mv.requires_usb.contains(&"03e7:f63b".to_string()));
        let fw = mv.firmware.as_ref().expect("movidius must gate Intel's firmware");
        assert_eq!(fw.sha256, "df5261ab79697f3c564279010aebbc8fd171f57290a84f636201aee82368a3b2");
        assert_eq!(fw.md5, "0f0eb9f1a529f2ef55c09d9fae72eb95");
        assert!(mv.files.iter().all(|f| !f.source.contains("mvcmd") && !f.source.contains("NCSDK")), "Intel's firmware must not be a plain catalogue file");
        assert!(mv.files.iter().all(|f| f.sha256.as_ref().map_or(false, |h| h.len() == 64)));
        assert!(fw.plan_text("X").contains(&fw.md5) && fw.plan_text("X").contains(&fw.sha256));
    }

    #[test]
    fn md5_and_cosine_are_right() {
        let tmp = TempDir::new("md5");
        std::fs::write(tmp.path().join("a"), b"abc").unwrap();
        assert_eq!(md5_hex(&tmp.path().join("a")).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-12);
        assert_eq!(cosine(&[0.0], &[1.0]), 0.0);
    }

    #[test]
    fn missing_usb_device_is_reported_as_not_found() {
        assert!(usb_found(&["ffff:ffff".to_string()]).is_empty());
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

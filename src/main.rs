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
use tokio::{process::Command, sync::Mutex};

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
    #[serde(default)]
    get_url: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FileSpec {
    source: String,
    dest: String,
    #[serde(default)]
    sha256: Option<String>,
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

    fn installed(&self, e: &Engine) -> bool {
        e.mode == "attach" || e.files.iter().all(|f| self.dir(&e.id).join(&f.dest).exists())
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
        let registered = providers.iter().any(|p| p["name"].as_str() == Some(&e.id));
        let models = if registered { app.lemonade_models(&e.id).await } else { vec![] };
        let running_here = app.children.lock().await.contains_key(&e.id);
        engines.push(json!({
            "engine": e, "installed": app.installed(e), "up": up, "running_here": running_here,
            "registered": registered, "models": models, "base": base,
            "accepted": app.dir(&e.id).join("licence-accepted").exists(),
        }));
    }
    Json(json!({"lemonade": {"url": app.lemonade, "ok": lemonade_ok, "version": version}, "engines": engines}))
}

#[derive(Deserialize, Default)]
struct InstallReq {
    #[serde(default)]
    accept_licence: bool,
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

async fn fetch(app: &App, f: &FileSpec, dir: &Path) -> Result<(), String> {
    let dst = dir.join(&f.dest);
    if let Some(p) = dst.parent() { tokio::fs::create_dir_all(p).await.map_err(|e| e.to_string())?; }
    if let Some(path) = f.source.strip_prefix("file://") {
        copy_path(Path::new(path), &dst).await?;
    } else if f.source.starts_with("https://") {
        let r = app.http.get(&f.source).send().await.map_err(|e| format!("download {}: {e}", f.source))?;
        if !r.status().is_success() { return Err(format!("download {}: HTTP {}", f.source, r.status())); }
        let bytes = r.bytes().await.map_err(|e| e.to_string())?;
        if let Some(want) = &f.sha256 {
            let got = format!("{:x}", Sha256::digest(&bytes));
            if !got.eq_ignore_ascii_case(want) { return Err(format!("{}: sha256 {got} does not match {want}", f.source)); }
        }
        tokio::fs::write(&dst, &bytes).await.map_err(|e| e.to_string())?;
    } else {
        return Err(format!("unsupported source {}", f.source));
    }
    if f.dest.ends_with(".tar.gz") {
        let st = Command::new("tar").arg("xzf").arg(&dst).arg("-C").arg(dir).status().await.map_err(|e| e.to_string())?;
        if !st.success() { return Err(format!("unpacking {} failed", f.dest)); }
    }
    Ok(())
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
    let t0 = Instant::now();
    for f in &e.files {
        if dir.join(&f.dest).exists() { continue; }
        if let Err(m) = fetch(&app, f, &dir).await { return err(StatusCode::BAD_GATEWAY, m); }
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

async fn start(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
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
    let port = e.port.unwrap_or(0).to_string();
    let args: Vec<String> = e.args.iter().map(|a| a.replace("{port}", &port).replace("{dir}", &dir.to_string_lossy())).collect();
    let exe = dir.join(e.exe.as_deref().unwrap_or(&e.id));
    let _ = Command::new("chmod").arg("+x").arg(&exe).status().await;
    let log = match std::fs::File::create(dir.join("engine.log")) { Ok(f) => f, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()) };
    let mut cmd = Command::new(&exe);
    cmd.args(&args).current_dir(&dir).stdin(Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log).kill_on_drop(true);
    // Own process group, so stopping the engine also stops anything it started.
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn();
    let child = match child { Ok(c) => c, Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, format!("could not start {}: {e}", exe.display())) };
    app.children.lock().await.insert(id.clone(), child);
    if wait_up(&app, &base, 120).await {
        Json(json!({"ok": true, "message": format!("Running at {base}")})).into_response()
    } else {
        err(StatusCode::GATEWAY_TIMEOUT, format!("Started, but {base}/v1/models did not answer within 120 s. See {}/engine.log", dir.display()))
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
    let _ = c.kill().await;
    let _ = c.wait().await;
}

async fn stop(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let child = app.children.lock().await.remove(&id);
    match child {
        Some(c) => { stop_child(c).await; Json(json!({"ok": true, "message": "Stopped."})).into_response() }
        None => err(StatusCode::NOT_FOUND, "Not started by lemonade-engines."),
    }
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

async fn register(State(app): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = app.engine(&id).cloned() else { return err(StatusCode::NOT_FOUND, "unknown engine") };
    let Some(base) = app.base(&e) else { return err(StatusCode::BAD_REQUEST, "engine has no address") };
    if e.accept_licence && !app.dir(&id).join("licence-accepted").exists() {
        return err(StatusCode::FORBIDDEN, "Accept the licence first.");
    }
    let mut log = String::new();
    // Engines that need load options (e.g. Cake's port and command) are loaded on the engine first;
    // a request routed through Lemonade cannot pass them.
    if let Some(body) = &e.preload {
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
        let cake = c.engine.iter().find(|e| e.id == "cake").unwrap();
        assert!(cake.accept_licence, "Cake is non-commercial: it must ask first");
    }
}

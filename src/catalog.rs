//! Signed online catalogue: fetch, verify, choose, and remember which engines the user has seen.
//!
//! The published catalogue is `catalog/engines.toml` plus `catalog/engines.toml.sig` (hex of an ed25519 signature over
//! the exact file bytes). The matching PUBLIC key is compiled in below. Nothing from a catalogue that does not verify
//! is ever parsed into use, so nothing from it is downloaded or run.

use std::{collections::BTreeSet, future::Future, path::Path};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::Catalog;

/// Public key (64 hex characters) that published catalogues must be signed with. PLACEHOLDER: empty, which turns the
/// online catalogue off (the built-in one is used). The operator generates the real key with
/// `lemonade-engines --gen-key PATH` (the private key must live outside this repo, never committed) and pastes the
/// printed public key here before releasing.
pub const CATALOG_PUBLIC_KEY_HEX: &str = "ebc8cfd3b5550e425a16a3477d6e609313b407e92cda0154df7d834a879e08d7";

pub const DEFAULT_CATALOG_URL: &str = "https://raw.githubusercontent.com/festeraeb/lemonade-engines/master/catalog/engines.toml";

/// Catalogue files are small; refuse anything bigger than this.
const MAX_CATALOG_BYTES: usize = 1024 * 1024;

fn parse_public_key(hex_key: &str) -> Result<VerifyingKey, String> {
    let raw = hex::decode(hex_key.trim()).map_err(|e| format!("public key is not hex: {e}"))?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| "public key must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&raw).map_err(|e| format!("bad public key: {e}"))
}

/// Check `sig_text` (hex, whitespace allowed around it) is a valid signature of exactly `bytes` under `public_hex`.
pub fn verify(public_hex: &str, bytes: &[u8], sig_text: &str) -> Result<(), String> {
    let key = parse_public_key(public_hex)?;
    let raw = hex::decode(sig_text.trim()).map_err(|e| format!("signature is not hex: {e}"))?;
    let sig = Signature::from_slice(&raw).map_err(|e| format!("bad signature: {e}"))?;
    key.verify(bytes, &sig).map_err(|_| "signature does not match the catalogue".to_string())
}

fn signing_key_from_file(path: &Path) -> Result<SigningKey, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw = hex::decode(text.trim()).map_err(|e| format!("private key file is not hex: {e}"))?;
    let seed: [u8; 32] = raw.try_into().map_err(|_| "private key must be 32 bytes".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Sign `bytes` with the private key (hex seed) in `key_path`; returns the hex signature.
pub fn sign(key_path: &Path, bytes: &[u8]) -> Result<String, String> {
    Ok(hex::encode(signing_key_from_file(key_path)?.sign(bytes).to_bytes()))
}

/// Write a new private key (hex seed, mode 0600 on Unix) to `key_path`, refusing to overwrite; returns the public key hex.
pub fn generate_key(key_path: &Path) -> Result<String, String> {
    if key_path.exists() {
        return Err(format!("{} already exists; not overwriting", key_path.display()));
    }
    let seed: [u8; 32] = rand::random();
    let mut f = {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?
    };
    std::io::Write::write_all(&mut f, format!("{}\n", hex::encode(seed)).as_bytes()).map_err(|e| e.to_string())?;
    Ok(hex::encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes()))
}

/// Which catalogue is in use and why; served by GET /api/catalog.
#[derive(Debug, Clone)]
pub struct Active {
    pub catalog: Catalog,
    /// "remote", "builtin" or "env"
    pub source: &'static str,
    pub version: u64,
    pub published: Option<String>,
    /// True only for a remote catalogue whose signature checked out.
    pub verified: bool,
}

impl Active {
    pub fn new(catalog: Catalog, source: &'static str, verified: bool) -> Self {
        Active { version: catalog.version.unwrap_or(0), published: catalog.published.clone(), catalog, source, verified }
    }
}

/// Fetch `url` and `url.sig` through `fetch`, verify, parse. Returns the catalogue only when everything checks out.
/// `fetch` is injected so tests need no network.
pub async fn fetch_verified<F, Fut>(fetch: F, url: &str, public_hex: &str) -> Result<Catalog, String>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, String>>,
{
    if public_hex.trim().is_empty() {
        return Err("no public key compiled in".into());
    }
    let body = fetch(url.to_string()).await?;
    let sig = fetch(format!("{url}.sig")).await?;
    let sig = String::from_utf8(sig).map_err(|_| "signature is not text".to_string())?;
    verify(public_hex, &body, &sig)?;
    let text = String::from_utf8(body).map_err(|_| "catalogue is not UTF-8".to_string())?;
    let c: Catalog = toml::from_str(&text).map_err(|e| format!("verified catalogue does not parse: {e}"))?;
    if c.version.is_none() {
        return Err("catalogue has no version".into());
    }
    Ok(c)
}

/// Use `remote` only if it is strictly newer than what is in use now (this also stops an old signed file being replayed).
pub fn consider(current: &Active, remote: Catalog) -> Result<Active, String> {
    let v = remote.version.unwrap_or(0);
    if v <= current.version {
        return Err(format!("remote version {v} is not newer than {} ({})", current.version, current.source));
    }
    Ok(Active::new(remote, "remote", true))
}

/// Download with a size cap and a timeout.
pub async fn http_fetch(http: &reqwest::Client, url: String) -> Result<Vec<u8>, String> {
    let r = http.get(&url).timeout(std::time::Duration::from_secs(15)).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status()));
    }
    let b = r.bytes().await.map_err(|e| e.to_string())?;
    if b.len() > MAX_CATALOG_BYTES {
        return Err("catalogue too large".into());
    }
    Ok(b.to_vec())
}

// ---- seen engines ("New" badge) ----

pub fn seen_path(home: &Path) -> std::path::PathBuf {
    home.join("seen-engines.json")
}

/// The ids the user has seen, or None when the file does not exist yet (first run).
pub fn load_seen(home: &Path) -> Option<BTreeSet<String>> {
    let text = std::fs::read_to_string(seen_path(home)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(v.get("seen")?.as_array()?.iter().filter_map(|x| x.as_str().map(String::from)).collect())
}

pub fn save_seen(home: &Path, seen: &BTreeSet<String>) {
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(seen_path(home), serde_json::json!({"seen": seen}).to_string());
}

/// First run ever: everything in the catalogue counts as seen, so a fresh install shows no badges.
pub fn init_seen(home: &Path, ids: &[String]) {
    if load_seen(home).is_none() {
        save_seen(home, &ids.iter().cloned().collect());
    }
}

/// Ids in the catalogue that are not in `seen`.
pub fn new_ids(ids: &[String], seen: &BTreeSet<String>) -> Vec<String> {
    ids.iter().filter(|i| !seen.contains(*i)).cloned().collect()
}

pub fn mark_seen(home: &Path, id: &str) {
    let mut seen = load_seen(home).unwrap_or_default();
    if seen.insert(id.to_string()) {
        save_seen(home, &seen);
    }
}

// ---- ENGINES.md ----

fn cell(s: &str) -> String {
    s.replace('|', "/").replace('\n', " ")
}

/// Table of engines built only from catalogue fields.
pub fn engines_md(c: &Catalog) -> String {
    let mut o = String::from("# Engines\n\nGenerated from the catalogue by `lemonade-engines --print-engines-md`; do not edit by hand.\n");
    if let (Some(v), Some(p)) = (c.version, &c.published) {
        o += &format!("Catalogue version {v}, published {p}.\n");
    }
    o += "\n| Engine | Id | What it is | Licence | Needs | Modified by us | Tested |\n|---|---|---|---|---|---|---|\n";
    for e in &c.engine {
        let dash = |x: &Option<String>| x.as_deref().map(cell).unwrap_or_else(|| "-".into());
        let mut what = cell(&e.summary);
        if let Some(p) = &e.pending {
            what += &format!(" (Pending: {})", cell(p));
        }
        o += &format!("| {} | {} | {} | {} | {} | {} | {} |\n", cell(&e.name), e.id, what, cell(&e.licence), dash(&e.hardware), dash(&e.modified), dash(&e.tested));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST keypair, generated for these unit tests only. It signs nothing real; the real key is made with --gen-key.
    const TEST_SEED_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    fn test_pub() -> String {
        hex::encode(SigningKey::from_bytes(&hex::decode(TEST_SEED_HEX).unwrap().try_into().unwrap()).verifying_key().to_bytes())
    }
    fn test_sign(bytes: &[u8]) -> String {
        let sk = SigningKey::from_bytes(&hex::decode(TEST_SEED_HEX).unwrap().try_into().unwrap());
        hex::encode(sk.sign(bytes).to_bytes())
    }

    fn cat_text(version: u64) -> String {
        format!("version = {version}\npublished = \"2026-11-08\"\n[[engine]]\nid = \"a\"\nname = \"A\"\nsummary = \"s\"\nlicence = \"l\"\nmode = \"attach\"\nurl = \"http://127.0.0.1:1\"\n")
    }

    /// Fetch function serving fixed bodies by url; anything else fails like a dead network.
    fn fake(url: &'static str, body: Vec<u8>, sig: String) -> impl Fn(String) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>> {
        move |u: String| {
            let (body, sig) = (body.clone(), sig.clone());
            Box::pin(async move {
                if u == url { Ok(body) } else if u == format!("{url}.sig") { Ok(sig.into_bytes()) } else { Err("connection refused".into()) }
            })
        }
    }

    #[test]
    fn signature_ok_bad_and_tampered() {
        let body = cat_text(5).into_bytes();
        let sig = test_sign(&body);
        assert!(verify(&test_pub(), &body, &sig).is_ok());
        assert!(verify(&test_pub(), &body, &format!("  {sig}\n")).is_ok());
        // flipped signature byte
        let mut bad = hex::decode(&sig).unwrap();
        bad[0] ^= 1;
        assert!(verify(&test_pub(), &body, &hex::encode(bad)).is_err());
        // tampered file
        let mut t = body.clone();
        t.extend_from_slice(b"# x\n");
        assert!(verify(&test_pub(), &t, &sig).is_err());
        // wrong key, junk
        assert!(verify(&hex::encode([7u8; 32]), &body, &sig).is_err());
        assert!(verify(&test_pub(), &body, "zz").is_err());
        assert!(verify("", &body, &sig).is_err());
    }

    #[tokio::test]
    async fn fetch_accepts_only_verified() {
        let url = "https://example.invalid/engines.toml";
        let body = cat_text(5).into_bytes();
        let good = fetch_verified(fake(url, body.clone(), test_sign(&body)), url, &test_pub()).await.unwrap();
        assert_eq!(good.version, Some(5));
        // tampered body: signature belongs to the original
        let mut t = body.clone();
        t.extend_from_slice(b"# x\n");
        assert!(fetch_verified(fake(url, t, test_sign(&body)), url, &test_pub()).await.is_err());
        // empty placeholder key turns it off
        assert!(fetch_verified(fake(url, body.clone(), test_sign(&body)), url, "").await.is_err());
    }

    #[tokio::test]
    async fn fetch_failure_is_an_error_not_a_panic() {
        let url = "https://example.invalid/engines.toml";
        let r = fetch_verified(fake("https://other.invalid/x", vec![], String::new()), url, &test_pub()).await;
        assert_eq!(r.unwrap_err(), "connection refused");
    }

    #[test]
    fn older_or_equal_remote_is_ignored() {
        let builtin = Active::new(toml::from_str(&cat_text(3)).unwrap(), "builtin", false);
        assert!(consider(&builtin, toml::from_str(&cat_text(2)).unwrap()).is_err());
        assert!(consider(&builtin, toml::from_str(&cat_text(3)).unwrap()).is_err());
        let a = consider(&builtin, toml::from_str(&cat_text(4)).unwrap()).unwrap();
        assert_eq!((a.source, a.version, a.verified), ("remote", 4, true));
        // an older signed file cannot replace a newer remote one
        assert!(consider(&a, toml::from_str(&cat_text(4)).unwrap()).is_err());
    }

    #[test]
    fn new_engine_detection_and_first_run() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let seen: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        assert_eq!(new_ids(&ids, &seen), ["c"]);

        let home = std::env::temp_dir().join(format!("le-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(load_seen(&home).is_none());
        init_seen(&home, &ids); // fresh install: all seen, no badges
        assert!(new_ids(&ids, &load_seen(&home).unwrap()).is_empty());
        init_seen(&home, &["zzz".to_string()]); // second start must not overwrite
        assert_eq!(load_seen(&home).unwrap().len(), 3);
        // a later catalogue adds "d": it is new until marked seen
        let later: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(new_ids(&later, &load_seen(&home).unwrap()), ["d"]);
        mark_seen(&home, "d");
        assert!(new_ids(&later, &load_seen(&home).unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn key_file_roundtrip_and_no_overwrite() {
        let dir = std::env::temp_dir().join(format!("le-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let k = dir.join("k.key");
        let public = generate_key(&k).unwrap();
        assert!(generate_key(&k).is_err());
        let sig = sign(&k, b"hello").unwrap();
        assert!(verify(&public, b"hello", &sig).is_ok());
        assert!(verify(&public, b"hellp", &sig).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engines_md_uses_catalogue_fields_only() {
        let c: Catalog = toml::from_str(&cat_text(1)).unwrap();
        let md = engines_md(&c);
        assert!(md.contains("| A | a | s | l | - | - | - |"));
    }
}

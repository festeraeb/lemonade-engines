# Lemonade Engines

Add engines to the [Lemonade](https://github.com/lemonade-sdk/lemonade) server you already run.
Each engine runs as its own OpenAI-compatible server on `127.0.0.1` and joins Lemonade as a
provider, so its models show up in Lemonade's list and work in every app that talks to Lemonade.
Lemonade itself is not modified.

## Run

```sh
cargo build --release
./target/release/lemonade-engines
```

The page opens in your browser at `http://127.0.0.1:13500`. Press **Set up** on an engine: it
installs the files, starts the server, adds it to Lemonade and asks one test question, showing
each step. The individual steps are still available under "Individual steps".

| Variable | Default | Meaning |
|---|---|---|
| `LEMONADE_HOST`, `LEMONADE_PORT` | `127.0.0.1`, `13305` | Your Lemonade server |
| `LE_LISTEN` | `127.0.0.1:13500` | Address of this page |
| `LE_CATALOG` | built-in `assets/engines.toml` | Engine catalogue (TOML) |
| `LE_HOME` | `~/.local/share/lemonade-engines` | Where engines are installed |
| `LE_NO_BROWSER` | unset | Set to skip opening the browser |

## Download

Builds for Linux x86-64 (glibc 2.30 or newer), Windows x86-64 and macOS (Intel and Apple silicon)
are on the [releases page](https://github.com/festeraeb/lemonade-engines/releases); GitHub Actions
builds them from each tag. Unpack and run `lemonade-engines`. The page then opens in your browser.

## Engines

| Engine | What Set up does | Platforms |
|---|---|---|
| lemonade-burn | Downloads the server for this computer and the BitNet model (sha256 pinned), starts it, adds it to Lemonade, asks a test question | Linux, Windows, macOS |
| Strata | Runs Strata's own signed installer into the engine folder, shows the plan (model and download size) and waits for **Download and start**, then serves through `strata launch` | Linux x86-64, Apple silicon Mac |
| Ternary Bonsai 8B (GPU / CPU) | Downloads PrismML's llama.cpp build for this computer and the ternary Bonsai 8B model (both sha256 pinned), starts it, adds it to Lemonade, asks a test question | Linux, Windows |
| Extended Lemonade server, Cake | Attach to a server you already run at 127.0.0.1:13410; not packaged yet | |

Windows builds of the Bonsai engine come as `.zip` files; the installer unpacks them into the engine folder.

## Status

Tested on 2026-10-24 with Lemonade 2026.41.1 on Linux x86-64, through the page in a browser:

- lemonade-burn: installed from the release, started, added to Lemonade and answered a chat.
- Ternary Bonsai 8B (CPU only), 2026-10-25 on t440: installed (2.2 GB), started, registered as `bonsai-cpu.ternary-bonsai-8b` (catalogue id since renamed `bonsaicpu`: provider ids are letters and digits only) and answered through Lemonade in 2.0 s. The GPU entry's build was run by hand on a Radeon Pro WX 5100 (13 tok/s) but not yet through this page.
  Downloads stream to disk and are checked against the catalogue's sha256; a wrong checksum is
  rejected and leaves nothing behind.
- Strata: installed with its signed installer and showed its plan (an 84 GB model on the test
  machine). Downloading the model and serving were not run in this test, so Start and Stop for
  Strata are untested.
- The Windows and macOS builds of both programs compile and package in CI; they have not been run
  on those systems yet.

## Licence

MIT OR Apache-2.0, at your option. Engines keep their own licences; Cake (FAIR, non-commercial)
is never bundled and is fetched only after you accept its licence on the page.

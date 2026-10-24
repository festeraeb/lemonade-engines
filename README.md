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

Prebuilt Linux x86-64 binaries (glibc 2.30 or newer, built on Ubuntu 20.04) are on the
[releases page](https://github.com/festeraeb/lemonade-engines/releases). Unpack and run
`./lemonade-engines`. The page then opens in your browser.

## Status

Tested on 2026-10-24 with Lemonade 2026.41.1 and these exact binaries. The `burn` engine was
installed from its release archives, started, added to Lemonade and answered a chat through
Lemonade. Downloads stream to disk and are checked against the sha256 in the catalogue. A wrong
checksum was rejected and left nothing behind. An attached llama.cpp server was also registered
and answered from the Set up button (2026-10-23).

The `strata` and `icedlemcake` entries attach to servers you start yourself. The `cake` entry's
preload command points at a local Cake build path, so it needs editing on other machines.

## Licence

MIT OR Apache-2.0, at your option. Engines keep their own licences; Cake (FAIR, non-commercial)
is never bundled and is fetched only after you accept its licence on the page.

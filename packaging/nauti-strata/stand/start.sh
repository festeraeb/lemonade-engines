#!/bin/sh
# Lemonade Stand start for Nauti-Strata.  Usage: start.sh PORT DIR
# Writes strata-stand.json (rewritten every start) and runs PolyStrata's server on 127.0.0.1:PORT.
#   STRATA_GPU            card to use, numbered as nvidia-smi numbers them (default 0);
#                         NAUTI_STRATA_GPU is the older name, still read when STRATA_GPU is unset
#   NAUTI_STRATA_CONTEXT  context length (default 8192)
#   NAUTI_STRATA_THINKING thinking level when a request names none: none (default), low, medium or high
# NautiBurn routing scrapes are OFF by default.  They record each answer's token ids and routed experts.
# To turn them on, write a folder path into DIR/nautiburn-scrape-dir (or set NAUTIBURN_SCRAPE_DIR);
# delete the file to turn them off again.  Takes effect at the next start.
set -eu
PORT="${1:?usage: start.sh PORT DIR}"; DIR="${2:?usage: start.sh PORT DIR}"; cd "$DIR"
. ./stand/model.env
if grep -qw avx2 /proc/cpuinfo; then EXE=bin/strata-poly-avx2; else EXE=bin/strata-poly-avx; fi
GPU="${STRATA_GPU:-${NAUTI_STRATA_GPU:-0}}"
SCRAPE="${NAUTIBURN_SCRAPE_DIR:-}"
if [ -z "$SCRAPE" ] && [ -s nautiburn-scrape-dir ]; then SCRAPE="$(head -n1 nautiburn-scrape-dir)"; fi
CC="$(nvidia-smi -i "$GPU" --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -n1 | tr -d ' .' || true)"
mkdir -p logs
if [ ! -f pack/tokenizer/tokenizer.json ]; then        # first start: the tokenizer, read out of the GGUF
  venv/bin/python tools/strata_tokenizer.py --gguf "$MODEL_FILE" --out pack
fi
DIR="$DIR" EXE="$EXE" GPU="$GPU" CC="$CC" SCRAPE="$SCRAPE" PORT="$PORT" CTX="${NAUTI_STRATA_CONTEXT:-8192}" THINK="${NAUTI_STRATA_THINKING:-none}" \
MODEL_FILE="$MODEL_FILE" MODEL_NAME="$MODEL_NAME" MODEL_DISPLAY="$MODEL_DISPLAY" venv/bin/python - <<'PY'
import json, os
d = os.environ["DIR"]
args = ["--model", os.path.join(d, os.environ["MODEL_FILE"]), "--max-context", os.environ["CTX"],
        "--vram-reserve-mib", "512"]
cc = os.environ.get("CC", "")
if cc.isdigit() and int(cc) < 75:          # Pascal: CUDA graphs do not instantiate on these cards
    args.append("--no-graphs")
env = {}
if os.environ.get("SCRAPE"):
    os.makedirs(os.environ["SCRAPE"], exist_ok=True)
    env = {"NAUTIBURN_SCRAPE_DIR": os.environ["SCRAPE"], "NAUTIBURN_MODEL_ID": os.environ["MODEL_NAME"],
           "NAUTIBURN_CLASS_ID": "stand-" + os.environ["MODEL_NAME"]}
cfg = {"exe": os.path.join(d, os.environ["EXE"]), "args": args, "cwd": d,
       "tokenizer": os.path.join(d, "pack", "tokenizer"), "model_name": os.environ["MODEL_NAME"],
       "display_name": os.environ["MODEL_DISPLAY"], "log": os.path.join(d, "logs", "engine.log"),
       "lib_dirs": [os.path.join(d, "lib")], "host": "127.0.0.1", "port": int(os.environ["PORT"]),
       "gpu": [int(os.environ["GPU"])], "gpus_asked": True, "open_browser": False, "env": env,
       "reasoning_effort": os.environ["THINK"]}   # LFM2.5 thinks unless told not to; a request can still ask
with open(os.path.join(d, "strata-stand.json"), "w") as f:
    json.dump(cfg, f, indent=2)
print("NautiBurn scrapes:", ("ON -> " + env["NAUTIBURN_SCRAPE_DIR"]) if env else "off")
PY
export CUDA_DEVICE_ORDER=PCI_BUS_ID STRATA_ENGINE_READY_S="${STRATA_ENGINE_READY_S:-1800}"
exec venv/bin/python serve/server.py --engine strata --config "$DIR/strata-stand.json" --host 127.0.0.1 --port "$PORT"

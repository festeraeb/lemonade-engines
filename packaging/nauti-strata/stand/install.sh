#!/bin/sh
# Lemonade Stand install step for Nauti-Strata, run once after the files are in place:
# a Python venv for the server (start.sh reads the tokenizer out of the model file on the first start).
# Usage: install.sh DIR
set -eu
DIR="${1:?usage: install.sh DIR}"; cd "$DIR"
command -v python3 >/dev/null 2>&1 || { echo "Nauti-Strata needs python3 (3.10 or newer)"; exit 1; }
if [ ! -x venv/bin/python ]; then
  python3 -m venv venv || { rm -rf venv; echo "python3 -m venv failed: install python3-venv (Debian/Ubuntu: sudo apt install python3-venv)"; exit 1; }
fi
venv/bin/python -m pip install --disable-pip-version-check -q -r stand/requirements.txt
echo "Nauti-Strata: Python venv ready"

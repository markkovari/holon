#!/usr/bin/env bash
# Registers once (REG_TOKEN, from install.sh), keeping the registration on the home volume; then runs.
set -euo pipefail
REG=/home/runner/.registration
FILES=(.runner .credentials .credentials_rsaparams)

[ -x "$HOME/.cargo/bin/rustup" ] ||
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --default-toolchain none

cd /opt/runner
if [ -f "$REG/.runner" ]; then
  for f in "${FILES[@]}"; do cp "$REG/$f" .; done
else
  : "${REG_TOKEN:?first start needs REG_TOKEN}"
  ./config.sh --unattended --replace --url "$REPO_URL" --token "$REG_TOKEN" \
    --name "$RUNNER_NAME" --labels picur --work /home/runner/_work
  mkdir -p "$REG" && for f in "${FILES[@]}"; do cp "$f" "$REG/"; done
fi
exec ./run.sh

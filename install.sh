#!/usr/bin/env bash
# barebone-agents bootstrap installer.
#
# Default install gets you a working agent (CLI + Discord) with local-first
# memory and preferences.
#
# Usage:
#   ./install.sh                          # base install (recommended starting point)
#   ./install.sh --with-systemd           # also write a user systemd unit for ino
#   ./install.sh --non-interactive                     # skip prompts; assume defaults
#
# Designed to be idempotent — safe to re-run.

set -euo pipefail

# ---------- helpers ----------
RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLW=$'\033[0;33m'; BLD=$'\033[1m'; RST=$'\033[0m'
log()  { printf "%s==>%s %s\n" "${GRN}" "${RST}" "$*"; }
warn() { printf "%s!! %s%s\n" "${YLW}" "$*" "${RST}"; }
err()  { printf "%sxx %s%s\n" "${RED}" "$*" "${RST}" >&2; exit 1; }
ask()  { # ask "prompt" "default"
  local prompt="$1" default="${2:-}" reply
  if [[ "${NON_INTERACTIVE}" == "1" ]]; then echo "${default}"; return; fi
  read -r -p "${prompt} [${default}]: " reply
  echo "${reply:-${default}}"
}

# ---------- args ----------
WITH_SYSTEMD=0
NON_INTERACTIVE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --with-systemd)     WITH_SYSTEMD=1 ;;
    --non-interactive)  NON_INTERACTIVE=1 ;;
    -h|--help)
      sed -n '2,12p' "$0"; exit 0 ;;
    *) err "unknown flag: $1" ;;
  esac
  shift
done

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "${ROOT_DIR}"
log "barebone-agents installer — root: ${ROOT_DIR}"

# ---------- detect OS / package manager ----------
PKG=""
if   command -v apt-get >/dev/null 2>&1; then PKG="apt"
elif command -v dnf     >/dev/null 2>&1; then PKG="dnf"
elif command -v pacman  >/dev/null 2>&1; then PKG="pacman"
elif command -v brew    >/dev/null 2>&1; then PKG="brew"
fi
log "package manager: ${PKG:-none-detected}"

# ---------- system deps ----------
install_pkgs() {
  case "${PKG}" in
    apt)    sudo apt-get update -qq && sudo apt-get install -y -qq build-essential pkg-config libssl-dev ca-certificates git curl ;;
    dnf)    sudo dnf install -y -q  gcc gcc-c++ make pkg-config openssl-devel ca-certificates git curl ;;
    pacman) sudo pacman -S --needed --noconfirm base-devel pkg-config openssl ca-certificates git curl ;;
    brew)   brew install pkg-config openssl ca-certificates git curl ;;
    *)      warn "unknown package manager — install build-essential / pkg-config / openssl-dev / git / curl manually" ;;
  esac
}

if [[ -z "${SKIP_SYSTEM_DEPS:-}" ]]; then
  log "installing system deps (build-essential, pkg-config, libssl-dev, ca-certificates, git, curl)…"
  install_pkgs
else
  warn "SKIP_SYSTEM_DEPS=1 — skipping apt/dnf/pacman step"
fi

# ---------- rust toolchain ----------
if ! command -v cargo >/dev/null 2>&1; then
  log "installing rustup + stable toolchain…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
else
  log "rust already installed: $(cargo --version)"
fi

# ---------- build ----------
log "building release binary (this takes a few minutes the first time)…"
cargo build --release
log "binary: ${ROOT_DIR}/target/release/barebone-agent"

# ---------- .env scaffolding ----------
if [[ ! -f .env ]]; then
  log "creating .env from .env.template"
  cp .env.template .env
  warn "edit .env and fill in at minimum NVIDIA_API_KEY (or another provider key)"
else
  log ".env already exists — leaving it alone"
fi

# Per-agent .env (Discord token lives here)
if [[ -d agents/ino && ! -f agents/ino/.env ]]; then
  log "creating agents/ino/.env"
  cat > agents/ino/.env <<'EOF'
# Per-agent secrets. Discord bot token goes here, not in the root .env.
DISCORD_BOT_TOKEN=
EOF
fi

# ---------- runtime directories (EP-00015) ----------
# The harness creates these on demand, but pre-creating keeps the layout
# obvious and prevents the first push/draft write from hitting an unexpected
# permission error if `data/` itself doesn't exist yet.
log "scaffolding data/drafts/* (local-first artifact storage)…"
mkdir -p data/drafts/researches
mkdir -p data/drafts/knowledges/preferences
mkdir -p data/drafts/sessions
mkdir -p data/drafts/notes

# ---------- config validate ----------
log "validating config…"
if ./target/release/barebone-agent config validate; then
  log "config OK"
else
  warn "config validate reported issues — see above. Likely a missing API key."
fi

# ---------- systemd unit (optional) ----------
if [[ "${WITH_SYSTEMD}" == "1" ]]; then
  UNIT_DIR="$HOME/.config/systemd/user"
  UNIT_FILE="${UNIT_DIR}/barebone-agent-ino.service"
  mkdir -p "${UNIT_DIR}"
  log "writing user systemd unit at ${UNIT_FILE}"

  cat > "${UNIT_FILE}" <<EOF
[Unit]
Description=barebone-agent (ino)
After=network-online.target

[Service]
Type=simple
WorkingDirectory=${ROOT_DIR}
ExecStart=${ROOT_DIR}/target/release/barebone-agent run --agent ino
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal
Environment=PATH=${HOME}/.cargo/bin:${HOME}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

[Install]
WantedBy=default.target
EOF

  systemctl --user daemon-reload
  log "to start: systemctl --user enable --now barebone-agent-ino"
  log "to tail:  journalctl --user -u barebone-agent-ino -f"
  log "to keep running after logout (one-time): sudo loginctl enable-linger \$USER"
fi

# ---------- next steps ----------
echo
log "${BLD}install complete.${RST}"
echo
echo "Next steps:"
echo "  1. Edit ${BLD}.env${RST} and add at least one LLM provider key (NVIDIA_API_KEY recommended for ino)."
if [[ -d agents/ino ]]; then
  echo "  2. (Discord) Add your bot token to ${BLD}agents/ino/.env${RST}, then set ${BLD}channels.discord.enabled${RST} in ${BLD}agents/ino/agent.yml${RST}."
fi
echo "  3. Smoke test:"
echo "       ./target/release/barebone-agent run --agent ino -m 'say hi in one word'"
if [[ "${WITH_SYSTEMD}" == "1" ]]; then
  echo "  4. Start the service: ${BLD}systemctl --user enable --now barebone-agent-ino${RST}"
fi
echo
echo "${BLD}Local-first memory & preferences (EP-00015):${RST}"
echo "  - Drop preferences into ${BLD}agents/_preferences/<slug>.md${RST}; the agent will inject"
echo "    matching ones into its system prompt on each task / first conversation turn."
echo "    A starter template is at ${BLD}agents/_preferences/.template.md${RST}."
echo "  - List the pool:    ${BLD}barebone-agent prefs list${RST}"
echo "  - Promote pending:  ${BLD}barebone-agent prefs promote <slug>${RST}"
echo

#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SIDECAR_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
TAURI_BIN_DIR="$(cd "${SIDECAR_DIR}/../src-tauri" && pwd)/binaries"
TRIPLE="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || echo "x86_64-unknown-linux-gnu")"

mkdir -p "${TAURI_BIN_DIR}"

if command -v pyinstaller >/dev/null 2>&1; then
  cd "${SIDECAR_DIR}"
  pyinstaller --noconfirm --onedir --name "extractor-${TRIPLE}" app/main.py
  rm -rf "${TAURI_BIN_DIR}/extractor-${TRIPLE}"
  cp -r "dist/extractor-${TRIPLE}" "${TAURI_BIN_DIR}/extractor-${TRIPLE}"
  ln -sfn "${TAURI_BIN_DIR}/extractor-${TRIPLE}/extractor-${TRIPLE}" "${TAURI_BIN_DIR}/extractor"
else
  # Wrapper executable using the local sidecar virtualenv
  cat > "${TAURI_BIN_DIR}/extractor" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SIDECAR_DIR="$(cd "${SELF_DIR}/../../sidecar" && pwd)"
export PYTHONPATH="${SIDECAR_DIR}"
exec "${SIDECAR_DIR}/.venv/bin/python" -m app.main "$@"
EOF
  chmod +x "${TAURI_BIN_DIR}/extractor"
  cp "${TAURI_BIN_DIR}/extractor" "${TAURI_BIN_DIR}/extractor-${TRIPLE}"
fi

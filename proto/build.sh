#!/usr/bin/env bash
# Build DDAL protocol buffers
# Requires: flatc (FlatBuffers compiler)
#
# Install flatc:
#   macOS:  brew install flatbuffers
#   Linux:  apt install flatbuffers-compiler
#   cargo:  cargo install flatbuffers
#
# Usage:
#   ./proto/build.sh          # generate Rust bindings
#   ./proto/build.sh --check  # verify schema compiles without writing files

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SCHEMA="${SCRIPT_DIR}/ddal.fbs"
OUT_DIR="${SCRIPT_DIR}/../crates/daf-ddal/src/generated/"

# Ensure flatc is available
if ! command -v flatc &>/dev/null; then
  echo "error: flatc not found. Install with: brew install flatbuffers" >&2
  exit 1
fi

# Check-only mode — validates the schema without writing output
if [[ "${1:-}" == "--check" ]]; then
  flatc --binary --schema "$SCHEMA" -o /dev/null 2>/dev/null || {
    echo "Schema validation failed" >&2
    exit 1
  }
  echo "Schema OK"
  exit 0
fi

# Ensure output directory exists
mkdir -p "$OUT_DIR"

# Generate Rust code
flatc --rust -o "$OUT_DIR" "$SCHEMA"

echo "DDAL protocol generated successfully -> ${OUT_DIR}"

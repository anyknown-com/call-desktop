#!/usr/bin/env sh
# Downloads the pinned ONNX models used by crates/voice-ml into models/ and verifies their sha256.
# Idempotent: existing files with a matching hash are left alone.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/models"
mkdir -p "$DIR"

fetch() { # name url sha256
  dest="$DIR/$1"
  if [ -f "$dest" ] && [ "$(shasum -a 256 "$dest" | cut -d' ' -f1)" = "$3" ]; then
    echo "ok       $1"
    return
  fi
  echo "fetching $1"
  curl -fsSL "$2" -o "$dest.part"
  got="$(shasum -a 256 "$dest.part" | cut -d' ' -f1)"
  if [ "$got" != "$3" ]; then
    rm -f "$dest.part"
    echo "sha256 mismatch for $1: got $got, want $3" >&2
    exit 1
  fi
  mv "$dest.part" "$dest"
  echo "ok       $1"
}

# Silero VAD v5, the exact file shipped by @ricky0123/vad-web 0.0.30 (dist/silero_vad_v5.onnx).
fetch silero_vad_v5.onnx \
  "https://unpkg.com/@ricky0123/vad-web@0.0.30/dist/silero_vad_v5.onnx" \
  2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f

# 3D-Speaker CAM++ zh-en (speech_campplus_sv_zh_en_16k-common_advanced), sherpa-onnx export.
# License / model card: models/campplus/. Pin must match voice_ml::speaker::CAMPPLUS_SHA256.
fetch campplus.onnx \
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx" \
  aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2

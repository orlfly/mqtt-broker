#!/usr/bin/env bash
# Download sherpa-onnx model files used by the voice channel when
# `engine: "sherpa"` is set in config/broker.yaml.
#
# Run from the workspace root:
#   ./scripts/download_voice_models.sh [target_dir]
#
# `target_dir` defaults to $HOME/models. The script populates:
#   <target_dir>/sherpa-kws-wenetspeech/   -- KWS zipformer (Chinese, ~32 MB)
#   <target_dir>/sherpa-zh-en-streaming/   -- streaming ASR (zh+en, ~300 MB)
#   <target_dir>/sherpa-vits-zh/           -- offline TTS (Piper VITS, Mandarin)
#   <target_dir>/silero_vad_v5.onnx        -- silero VAD for endpoint detection (~2.3 MB)
#
# Custom wake words require pinyin-tokenised lines. To add one:
#   echo "你好小金" > keywords_raw.txt
#   sherpa-onnx-cli text2token \
#       --tokens <target>/sherpa-kws-wenetspeech/tokens.txt \
#       --tokens-type ppinyin keywords_raw.txt keywords.txt
# Then point voice.kws_keywords_file at the generated file or copy the
# token line into voice.kws_keywords_inline.

set -euo pipefail

TARGET_DIR="${1:-$HOME/models}"
mkdir -p "$TARGET_DIR"

RELEASE_BASE="https://github.com/k2-fsa/sherpa-onnx/releases/download"

# KWS — WenetSpeech 3.3M Chinese zipformer (~32 MB).
KWS_MODEL="sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01"
KWS_DIR="$TARGET_DIR/sherpa-kws-wenetspeech"

# Streaming Zipformer ASR (Chinese + English, ~300 MB).
ASR_MODEL="sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
ASR_DIR="$TARGET_DIR/sherpa-zh-en-streaming"

# Piper VITS Mandarin model (uses espeak-ng for grapheme-to-phoneme).
# `vits-zh-hf-data` is the name the docs used historically; the actual
# release asset is `vits-piper-zh_CN-huayan-medium` (Chinese, ~64 MB,
# includes espeak-ng-data/, tokens.txt, and a Piper ONNX).
TTS_MODEL="vits-piper-zh_CN-huayan-medium"
TTS_DIR="$TARGET_DIR/sherpa-vits-zh"

# Silero VAD (endpoint detection during user capture, ~2.3 MB).
VAD_FILE="silero_vad_v5.onnx"
VAD_PATH="$TARGET_DIR/$VAD_FILE"

download_tarball() {
    local kind="$1"
    local name="$2"
    local url="$RELEASE_BASE/$kind/$name.tar.bz2"
    local archive="$TARGET_DIR/$name.tar.bz2"
    if [ ! -d "$TARGET_DIR/$name" ]; then
        echo ">> downloading $url"
        curl -fL --retry 3 -o "$archive" "$url"
        echo ">> extracting"
        tar -xjf "$archive" -C "$TARGET_DIR"
        rm -f "$archive"
    else
        echo ">> $name already extracted, skipping"
    fi
}

download_file() {
    local url="$1"
    local dst="$2"
    if [ ! -f "$dst" ]; then
        echo ">> downloading $url"
        curl -fL --retry 3 -o "$dst" "$url"
    else
        echo ">> $dst already present, skipping"
    fi
}

download_tarball "kws-models"  "$KWS_MODEL"
download_tarball "asr-models"  "$ASR_MODEL"
download_tarball "tts-models"  "$TTS_MODEL"
download_file    "$RELEASE_BASE/asr-models/$VAD_FILE" "$VAD_PATH"

# Sanity check the expected filenames match the paths in broker.yaml.
echo
echo "Verifying expected files..."
expected=(
    "$KWS_DIR/encoder-epoch-12-avg-2-chunk-16-left-64.onnx"
    "$KWS_DIR/decoder-epoch-12-avg-2-chunk-16-left-64.onnx"
    "$KWS_DIR/joiner-epoch-12-avg-2-chunk-16-left-64.onnx"
    "$KWS_DIR/tokens.txt"
    "$ASR_DIR/encoder-epoch-99-avg-1.onnx"
    "$ASR_DIR/decoder-epoch-99-avg-1.onnx"
    "$ASR_DIR/joiner-epoch-99-avg-1.onnx"
    "$ASR_DIR/tokens.txt"
    "$TTS_DIR/zh_CN-huayan-medium.onnx"
    "$TTS_DIR/tokens.txt"
    "$TTS_DIR/espeak-ng-data"
    "$VAD_PATH"
)
missing=0
for f in "${expected[@]}"; do
    if [ ! -e "$f" ]; then
        echo "  MISSING: $f"
        missing=1
    fi
done

if [ "$missing" -ne 0 ]; then
    echo
    echo "Some files were not found. The release tarball layout may have changed;"
    echo "inspect $TARGET_DIR and update paths in config/broker.yaml accordingly."
    exit 1
fi

echo
echo "All models are at:"
echo "  $KWS_DIR"
echo "  $ASR_DIR"
echo "  $TTS_DIR"
echo "  $VAD_PATH"
echo
echo "Bundled KWS keywords (from the model): $KWS_DIR/keywords.txt"
echo "Run \`cargo run -p agent --features sherpa\` to start the voice loop."

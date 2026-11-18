#!/bin/bash
# Fuse a LoRA adapter (ft.py) into its base model, convert to GGUF and create
# the Ollama model with the tuned planner's chat template.
#   export.sh BASE_HF_DIR ADAPTER_DIR NAME OUT_DIR [QUANT=q4_K_M]
# Needs: mlx-lm, a llama.cpp checkout ($LLAMA_CPP) with llama-quantize built, ollama.
set -euo pipefail
BASE=$1; AD=$2; NAME=$3; OUT=$4; Q=${5:-q4_K_M}
PY=${PYTHON:-python3}
LLAMA_CPP=${LLAMA_CPP:?path to a llama.cpp checkout with build/bin/llama-quantize}
mkdir -p "$OUT"
[ -d "$OUT/hf" ] || "$PY" -m mlx_lm fuse --model "$BASE" --adapter-path "$AD" --save-path "$OUT/hf" --dequantize
# llama.cpp converts and quantizes (this Ollama quantizes only safetensors,
# and only to int4/int8): f16 GGUF, then $Q with llama-quantize (build:
# cmake --build build --target llama-quantize). The same GGUF serves
# llama-server.
[ -f "$OUT/f16.gguf" ] || "$PY" "$LLAMA_CPP/convert_hf_to_gguf.py" "$OUT/hf" --outtype f16 --outfile "$OUT/f16.gguf"
[ -f "$OUT/$Q.gguf" ] || "$LLAMA_CPP/build/bin/llama-quantize" "$OUT/f16.gguf" "$OUT/$Q.gguf" "$Q"
sha256sum "$OUT/$Q.gguf" 2>/dev/null || shasum -a 256 "$OUT/$Q.gguf"
# The template renders exactly ft.py's training text: ChatML, no think block.
# FROM is relative to the Modelfile, so the same file works next to the GGUF
# here and in the planner container's /models (security review F3).
cat > "$OUT/Modelfile" <<EOF
FROM ./$Q.gguf
TEMPLATE """{{- range .Messages }}<|im_start|>{{ .Role }}
{{ .Content }}<|im_end|>
{{ end }}<|im_start|>assistant
"""
PARAMETER stop <|im_end|>
PARAMETER stop <|im_start|>
PARAMETER temperature 0
EOF
ollama create "$NAME" -f "$OUT/Modelfile"
ollama show "$NAME" --modelfile | head -3

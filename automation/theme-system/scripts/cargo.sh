#!/usr/bin/env bash
# theme-system 专用 cargo 包装：与 ux-audit/scripts/cargo.sh 同一套本机环境处理。
#   1) crates.io 镜像 TUNA 不可达时，本次命令改用 rsproxy（不改全局 ~/.cargo/config.toml）。
#   2) 默认 -j 2：本机全量并行链接会 OOM，单 crate 构建足够快。
# 用法与 cargo 完全一致：
#   bash automation/theme-system/scripts/cargo.sh check -p ui-viewmodels
#   bash automation/theme-system/scripts/cargo.sh test -p app-ui --test ui_hygiene
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"

MIRROR_ARGS=()
if ! curl -sSf -m 4 -o /dev/null "https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/config.json" 2>/dev/null; then
  echo "[theme-system] TUNA 镜像不可达，本次改用 rsproxy 源" >&2
  MIRROR_ARGS=(--config 'source.tuna.registry="sparse+https://rsproxy.cn/index/"')
fi

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# --config 必须位于 `--` 之前，否则会被当成 rustc 参数。
ARGS=()
SEP=""
for arg in "$@"; do
  if [[ -z "$SEP" && "$arg" == "--" ]]; then
    ARGS+=("${MIRROR_ARGS[@]}" "--")
    SEP=1
  else
    ARGS+=("$arg")
  fi
done
if [[ -z "$SEP" ]]; then
  ARGS+=("${MIRROR_ARGS[@]}")
fi

exec cargo "${ARGS[@]}"

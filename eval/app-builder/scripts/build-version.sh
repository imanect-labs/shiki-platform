#!/usr/bin/env bash
# build-version.sh — 評価の版（GAP-30）のサーバーを、きれいな commit から別の worktree でビルドして退避する。
#
#   使い方:
#     build-version.sh [--worktree-root DIR] [--bin-dir DIR] [--versions-dir DIR] [--repo DIR] [--dry-run]
#                      <label> <commit>
#
#   手順:
#     1. <worktree-root>/abeval-<label> に `git worktree add --detach` する（既にあれば拒む）。
#     2. `git status --porcelain` が空でなければ止める（素性を確かめられない版は測らない）。
#     3. `cargo build -p shiki-api --bin shiki-server` でビルドする（CARGO_TARGET_DIR は worktree の target）。
#        CSV の集計の隔離ランナー（workspace の外・crates/tabular/runner）も、同じ commit で
#        `cargo build --locked --manifest-path crates/tabular/runner/Cargo.toml` でビルドする（target/tabular-runner。
#        版のサーバーが作業ツリーの HEAD のランナーを使うと、版の差にランナーの差が混ざるため）。
#     4. バイナリを <bin-dir>/shiki-server-<label>、ランナーを <bin-dir>/shiki-tabular-runner-<label> に写し、
#        sha256 を取る。
#     5. <versions-dir>/<label>.json に {label, commit, tree_clean, binary_sha256, runner_sha256, built_at, rustc} を書く。
#     6. worktree を消す（target も消える）。途中で失敗しても worktree は消す。
#
#   既定:
#     --worktree-root  ~/orca/workspaces/shiki-platform（/tmp・/dev/shm は tmpfs なので拒む）
#     --bin-dir        ~/.local/share/shiki-eval/bin
#     --versions-dir   このスクリプトの ../data/versions
#     --repo           このスクリプトのあるリポジトリ
#   --dry-run は何も作らずに手順だけを表示する。
#   cargo と rustc は ABEVAL_CARGO・ABEVAL_RUSTC で差し替えられる（テスト用）。
#   label は英小文字で始まり英小文字か数字で終わる、英小文字・数字・- の 21 文字まで
#   （^[a-z]([a-z0-9-]{0,19}[a-z0-9])?$。S3 のバケット・DB・store の名前に使う）。
#
#   注意: このマシンは 2 CPU・RAM 10GB。ほかのビルドや E2E と重ねない（台帳 §9.4）。
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }
say() { printf '%s\n' "$*"; }
usage() { awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; }

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORKTREE_ROOT="$HOME/orca/workspaces/shiki-platform"
BIN_DIR="$HOME/.local/share/shiki-eval/bin"
VERSIONS_DIR="$SCRIPT_DIR/../data/versions"
REPO=""
DRY_RUN=0
POS=()

need_val() {
  if [ $# -lt 2 ] || [ -z "$2" ]; then
    err "$1 には値が要ります"
    exit 2
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --worktree-root) need_val "$@"; WORKTREE_ROOT=$2; shift ;;
    --bin-dir)       need_val "$@"; BIN_DIR=$2; shift ;;
    --versions-dir)  need_val "$@"; VERSIONS_DIR=$2; shift ;;
    --repo)          need_val "$@"; REPO=$2; shift ;;
    --dry-run)       DRY_RUN=1 ;;
    -h|--help)       usage; exit 0 ;;
    --*)             err "未知の引数: $1"; exit 2 ;;
    *)               POS+=("$1") ;;
  esac
  shift
done

if [ "${#POS[@]}" -ne 2 ]; then
  err "使い方: build-version.sh [オプション] <label> <commit>（--help）"
  exit 2
fi
LABEL=${POS[0]}
COMMIT_ARG=${POS[1]}

if ! [[ "$LABEL" =~ ^[a-z]([a-z0-9-]{0,19}[a-z0-9])?$ ]]; then
  err "label は英小文字で始まり英小文字か数字で終わる、英小文字・数字・- の 21 文字までです: $LABEL"
  exit 2
fi

if [ -z "$REPO" ]; then
  REPO=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null) || { err "リポジトリが見つかりません（--repo）"; exit 2; }
fi
REPO=$(cd "$REPO" && pwd)

COMMIT=$(git -C "$REPO" rev-parse --verify --quiet "${COMMIT_ARG}^{commit}") || {
  err "commit が見つかりません: $COMMIT_ARG"
  exit 2
}

# tmpfs（/tmp・/dev/shm）にビルドを置かない（RAM 10GB のマシンで OOM になる）。
abs_path() {  # 存在しなくてもよい絶対パス（親までたどれる所で解く）
  local p=$1 tail=""
  [[ "$p" == /* ]] || p="$PWD/$p"
  while [ ! -e "$p" ]; do
    tail="/$(basename "$p")$tail"
    p=$(dirname "$p")
  done
  printf '%s%s' "$(cd "$p" && pwd -P)" "$tail"
}
WORKTREE_ROOT=$(abs_path "$WORKTREE_ROOT")
case "$WORKTREE_ROOT/" in
  /tmp/*|/dev/shm/*|/run/*) err "--worktree-root を tmpfs（$WORKTREE_ROOT）に置かないでください（ビルドは 15GB 前後）"; exit 2 ;;
esac
BIN_DIR=$(abs_path "$BIN_DIR")
VERSIONS_DIR=$(abs_path "$VERSIONS_DIR")

WT="$WORKTREE_ROOT/abeval-$LABEL"
BIN="$BIN_DIR/shiki-server-$LABEL"
RUNNER="$BIN_DIR/shiki-tabular-runner-$LABEL"
RECORD="$VERSIONS_DIR/$LABEL.json"

for p in "$WT" "$BIN" "$RUNNER" "$RECORD"; do
  if [ -e "$p" ]; then
    err "既にあります（同じ label を作り直すなら、先に消してください）: $p"
    exit 2
  fi
done

CARGO=${ABEVAL_CARGO:-cargo}
RUSTC=${ABEVAL_RUSTC:-rustc}

say "label=$LABEL commit=$COMMIT"
say "  worktree  $WT"
say "  binary    $BIN"
say "  runner    $RUNNER"
say "  record    $RECORD"
if [ "$DRY_RUN" = 1 ]; then
  say "（--dry-run: 何も作りません）"
  say "  1. git -C $REPO worktree add --detach $WT $COMMIT"
  say "  2. git -C $WT status --porcelain（空でなければ止める）"
  say "  3. (cd $WT && CARGO_TARGET_DIR=$WT/target $CARGO build -p shiki-api --bin shiki-server)"
  say "     (cd $WT && CARGO_TARGET_DIR=$WT/target/tabular-runner $CARGO build --locked --manifest-path crates/tabular/runner/Cargo.toml)"
  say "  4. install -m 755 $WT/target/debug/shiki-server $BIN・$RUNNER・sha256"
  say "  5. $RECORD を書く"
  say "  6. git -C $REPO worktree remove --force $WT"
  exit 0
fi

cleanup() {
  if [ -d "$WT" ]; then
    git -C "$REPO" worktree remove --force "$WT" >/dev/null 2>&1 || rm -rf "$WT"
  fi
  git -C "$REPO" worktree prune >/dev/null 2>&1 || true
}
trap cleanup EXIT

say "== 1. worktree を作る =="
git -C "$REPO" worktree add --detach "$WT" "$COMMIT"

say "== 2. 作業ツリーがきれいか確かめる =="
DIRTY=$(git -C "$WT" status --porcelain --untracked-files=all)
if [ -n "$DIRTY" ]; then
  err "worktree がきれいではありません（素性を確かめられないので止めます）:"
  printf '%s\n' "$DIRTY" | head -20 >&2
  exit 1
fi
HEAD_SHA=$(git -C "$WT" rev-parse HEAD)
[ "$HEAD_SHA" = "$COMMIT" ] || { err "worktree の HEAD が違います: $HEAD_SHA"; exit 1; }

say "== 3. ビルドする（sccache が効けば 7 分程度） =="
(
  cd "$WT"
  CARGO_TARGET_DIR="$WT/target" "$CARGO" build -p shiki-api --bin shiki-server
)
BUILT="$WT/target/debug/shiki-server"
[ -f "$BUILT" ] && [ -x "$BUILT" ] || { err "ビルドしたバイナリがありません: $BUILT"; exit 1; }
# CSV の集計の隔離ランナー（workspace の外のクレート・同じ commit のもの）。
RUNNER_MANIFEST="$WT/crates/tabular/runner/Cargo.toml"
[ -f "$RUNNER_MANIFEST" ] || { err "ランナーのクレートがありません: $RUNNER_MANIFEST"; exit 1; }
(
  cd "$WT"
  CARGO_TARGET_DIR="$WT/target/tabular-runner" "$CARGO" build --locked --manifest-path crates/tabular/runner/Cargo.toml
)
BUILT_RUNNER="$WT/target/tabular-runner/debug/shiki-tabular-runner"
[ -f "$BUILT_RUNNER" ] && [ -x "$BUILT_RUNNER" ] || { err "ビルドしたランナーがありません: $BUILT_RUNNER"; exit 1; }
# ビルドが作業ツリーを書き換えていないか（生成物の書き戻しなど）。
DIRTY=$(git -C "$WT" status --porcelain --untracked-files=all -- . ':(exclude)target')
if [ -n "$DIRTY" ]; then
  err "ビルドが作業ツリーを書き換えました（素性が崩れるので止めます）:"
  printf '%s\n' "$DIRTY" | head -20 >&2
  exit 1
fi
RUSTC_VERSION=$(cd "$WT" && "$RUSTC" --version)

say "== 4. バイナリを退避する =="
mkdir -p "$BIN_DIR"
install -m 755 "$BUILT" "$BIN.tmp.$$"
mv -f "$BIN.tmp.$$" "$BIN"
SHA=$(sha256sum "$BIN" | cut -d' ' -f1)
install -m 755 "$BUILT_RUNNER" "$RUNNER.tmp.$$"
mv -f "$RUNNER.tmp.$$" "$RUNNER"
RUNNER_SHA=$(sha256sum "$RUNNER" | cut -d' ' -f1)

say "== 5. 版の記録を書く =="
mkdir -p "$VERSIONS_DIR"
LABEL="$LABEL" COMMIT="$COMMIT" SHA="$SHA" RUNNER_SHA="$RUNNER_SHA" RUSTC_VERSION="$RUSTC_VERSION" python3 - "$RECORD" <<'PY'
import datetime, json, os, sys
record = {
    "label": os.environ["LABEL"],
    "commit": os.environ["COMMIT"],
    "tree_clean": True,
    "binary_sha256": os.environ["SHA"],
    "runner_sha256": os.environ["RUNNER_SHA"],
    "built_at": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "rustc": os.environ["RUSTC_VERSION"],
}
path = sys.argv[1]
with open(path + ".tmp", "w", encoding="utf-8") as f:
    json.dump(record, f, ensure_ascii=False, indent=2, sort_keys=True)
    f.write("\n")
os.replace(path + ".tmp", path)
PY

say "== 6. worktree を消す =="
cleanup
trap - EXIT
[ ! -e "$WT" ] || { err "worktree が消えていません: $WT"; exit 1; }

say ""
say "[ok] $LABEL ($COMMIT)"
say "  binary  $BIN"
say "  sha256  $SHA"
say "  runner  $RUNNER（$RUNNER_SHA）"
say "  record  $RECORD"

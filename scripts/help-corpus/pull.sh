#!/usr/bin/env bash
# ORAS 認証は呼出側で行う。タグを digest に解決してから取得する（TOCTOU 防止）。
set -euo pipefail
selector=${1:-main}
out=${2:?output directory required}
if [[ "$selector" != main && ! "$selector" =~ ^sha-[a-f0-9]{12}$ ]]; then
  echo 'expected main or sha-<12 lowercase hex>' >&2
  exit 1
fi
ref="ghcr.io/imanect-labs/shiki-help-corpus:$selector"
digest=$(oras resolve "$ref")
[[ "$digest" =~ ^sha256:[a-f0-9]{64}$ ]] || exit 1
mkdir -p "$out"
oras pull "ghcr.io/imanect-labs/shiki-help-corpus@$digest" --output "$out"
printf '%s\n' "$digest" > "$out/oci-digest"

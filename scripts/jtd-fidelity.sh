#!/usr/bin/env bash
# JTD → docx の視覚比較（トラックJTD の忠実度ハーネス・手動実行）。
#
#   scripts/jtd-fidelity.sh [出力ディレクトリ]
#
# やること:
#   1. フィクスチャの .jtd を crates/jtd で docx へ変換する
#   2. Collabora（docker）で docx を PDF 化し、各ページを PNG にする
#   3. 原本の配布 PDF も PNG にして、並べて見られる状態にする
#
# **Collabora が無ければ失敗する。** cargo test に入れずスクリプトに分けてあるのは、
# 「無いので静かにスキップ」を起こさないため（CLAUDE.md「意図したテストが実際に
# 走ったかを確認する」）。決定的な指標は crates/jtd/tests/fidelity_it.rs が見ている。
#
# 罫線位置とページ割りの一致を機械判定にするのは JTD.3 / JTD.4 の範囲。
# 本タスクの時点では**人が並べて見る**ためのものと割り切っている。
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
OUT="${1:-$ROOT/target/jtd-fidelity}"
FIXTURES="$ROOT/crates/jtd/tests/fixtures"
IMAGE="${COLLABORA_IMAGE:-collabora/code:26.04.2.1.1}"

if ! command -v docker >/dev/null 2>&1; then
  echo "❌ docker が必要です（Collabora で docx を PDF 化するため）。" >&2
  exit 1
fi
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "→ $IMAGE を取得します"
  docker pull "$IMAGE"
fi

mkdir -p "$OUT/docx" "$OUT/pdf" "$OUT/png"
chmod 777 "$OUT/pdf"

echo "→ .jtd → docx"
for jtd in "$FIXTURES"/*.jtd "$FIXTURES"/external/*.jtd; do
  [ -e "$jtd" ] || continue
  name="$(basename "$jtd" .jtd)"
  cargo run --quiet -p shiki-jtd --example jtd-dump -- --docx "$OUT/docx/$name.docx" "$jtd"
done

echo "→ docx → PDF（Collabora）"
docker run --rm -v "$OUT:/w" --entrypoint /bin/bash "$IMAGE" -c '
  set -e
  cd /tmp && cp /w/docx/*.docx .
  for f in *.docx; do
    /opt/collaboraoffice/program/soffice --headless \
      -env:UserInstallation=file:///tmp/lo --convert-to pdf --outdir /tmp/pdf "$f" >/dev/null 2>&1
  done
  cp /tmp/pdf/*.pdf /w/pdf/
'

echo "→ PDF → PNG"
python3 - "$OUT" <<'PY'
import sys, pathlib
try:
    import pypdfium2, PIL  # noqa: F401
except ImportError:
    sys.exit("❌ PNG 化には pypdfium2 と pillow が要ります: uv pip install pypdfium2 pillow")
import pypdfium2 as pdfium
out = pathlib.Path(sys.argv[1])
for pdf in sorted((out / "pdf").glob("*.pdf")):
    document = pdfium.PdfDocument(str(pdf))
    print(f"   {pdf.stem}: {len(document)} ページ")
    for index in range(min(3, len(document))):
        image = document[index].render(scale=2).to_pil()
        image.save(out / "png" / f"{pdf.stem}.p{index + 1}.png")
PY

cat <<EOF

✅ 出力: $OUT
   docx/  変換結果
   pdf/   Collabora で PDF 化したもの
   png/   各ファイルの先頭 3 ページ（2 倍解像度）

原本の配布 PDF と並べて見てください（f1 は
https://www.mhlw.go.jp/wp/kenkyu/koubo04/dl/f1.pdf）。
**この時点では表・罫線・段組・ページ割りは写りません**（JTD.3 / JTD.4 の範囲）。
見るべきは本文の欠落・文字化け・全角スペースの潰れです。
EOF

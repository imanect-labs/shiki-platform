#!/usr/bin/env bash
# JTD → docx の視覚比較（トラックJTD の忠実度ハーネス・手動実行）。
#
#   scripts/jtd-fidelity.sh [出力ディレクトリ]
#
# やること:
#   1. 原本の配布 PDF を取得して参照側に置く
#   2. フィクスチャの .jtd を crates/jtd で docx へ変換する
#   3. Collabora（docker）で docx を PDF 化し、各ページを PNG にする
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

# 再配布しないフィクスチャは取得済みでなければならない。**黙って飛ばさない。**
if ! compgen -G "$FIXTURES/external/*.jtd" >/dev/null; then
  echo "❌ $FIXTURES/external/ に .jtd がありません。" >&2
  echo "   先に scripts/fetch-jtd-fixtures.sh を実行してください。" >&2
  exit 1
fi

mkdir -p "$OUT/docx" "$OUT/pdf" "$OUT/png" "$OUT/reference"
chmod 777 "$OUT/pdf"

# 原本の配布 PDF。罫線位置とページ割りの真値で、視覚比較の参照側になる。
echo "→ 原本の配布 PDF を取得"
if ! curl -fsSL --retry 3 -A "Mozilla/5.0" \
     -o "$OUT/reference/f1.pdf" "https://www.mhlw.go.jp/wp/kenkyu/koubo04/dl/f1.pdf"; then
  echo "❌ 原本 PDF を取得できませんでした（比較対象が無いので中止します）。" >&2
  exit 1
fi
echo "   ✅ reference/f1.pdf（$(wc -c <"$OUT/reference/f1.pdf") bytes）"

echo "→ .jtd → docx"
for jtd in "$FIXTURES"/*.jtd "$FIXTURES"/external/*.jtd; do
  name="$(basename "$jtd" .jtd)"
  cargo run --quiet -p shiki-jtd --example jtd-dump -- --docx "$OUT/docx/$name.docx" "$jtd"
done

echo "→ docx → PDF（Collabora）"
docker run --rm -v "$OUT:/w" --entrypoint /bin/bash "$IMAGE" -c '
  set -e
  cd /tmp && cp /w/docx/*.docx .
  mkdir -p /tmp/pdf
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
for pdf in sorted((out / "pdf").glob("*.pdf")) + sorted((out / "reference").glob("*.pdf")):
    document = pdfium.PdfDocument(str(pdf))
    print(f"   {pdf.stem}: {len(document)} ページ")
    for index in range(min(3, len(document))):
        image = document[index].render(scale=2).to_pil()
        prefix = "reference-" if pdf.parent.name == "reference" else ""
        image.save(out / "png" / f"{prefix}{pdf.stem}.p{index + 1}.png")
PY

cat <<EOF

✅ 出力: $OUT
   docx/       変換結果
   pdf/        Collabora で PDF 化したもの
   reference/  原本の配布 PDF（比較の参照側）
   png/        各ファイルの先頭 3 ページ（2 倍解像度・reference- 接頭辞が原本）

png/f1.p1.png と png/reference-f1.p1.png を並べて見てください。
**この時点では表・罫線・段組・ページ割りは写りません**（JTD.3 / JTD.4 の範囲）。
見るべきは本文の欠落・文字化け・全角スペースの潰れです。
EOF

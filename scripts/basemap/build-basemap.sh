#!/usr/bin/env bash
# build-basemap.sh — 背景地図のデータ（OpenMapTiles 形式の PMTiles）を OSM から作る（design.md §4.7・PIT-84）。
#
#   使い方:
#     build-basemap.sh --pbf <地域の .osm.pbf> --bounds <西,南,東,北> --out <出力ディレクトリ> [--jar <planetiler.jar>]
#
#   例（東京駅のまわり。経度・緯度の 10 進度）:
#     build-basemap.sh --pbf ~/osm/kanto-latest.osm.pbf --bounds 139.70,35.65,139.83,35.72 --out ~/map-data
#
#   - 地域の .osm.pbf は Geofabrik などの OSM の抽出を各自で落とす（このスクリプトは OSM 本体を落とさない）。
#   - planetiler（https://github.com/onthegomap/planetiler）は Java 21 以上で動く。--jar を省くと
#     PLANETILER_JAR、無ければ出力ディレクトリの planetiler.jar を使う。
#   - planetiler は低ズームの海岸線・湖・国境のために Natural Earth と OSM の水域ポリゴンを落とす（--download）。
#     エアギャップ環境では、つながる環境で作った .pmtiles を運び込む。
#   - 出力は <出力ディレクトリ>/basemap.pmtiles。shiki-server の SHIKI__MAP__DATA_DIR にそのディレクトリを渡す。
#     作り直しは別名で書いてから置き換える（配信中のファイルを書きかけにしない）。
#   - 生成物（.pmtiles）はリポジトリに入れない（数百 MB〜になる・OSM の写しは配備ごとに作る）。
#   - 出典「© OpenStreetMap contributors © OpenMapTiles」は地図に常に表示される（ODbL・OpenMapTiles の条件）。
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }

PBF=""; BOUNDS=""; OUT=""; JAR="${PLANETILER_JAR:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --pbf) PBF=${2:?}; shift ;;
    --bounds) BOUNDS=${2:?}; shift ;;
    --out) OUT=${2:?}; shift ;;
    --jar) JAR=${2:?}; shift ;;
    -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) err "未知の引数: $1"; exit 2 ;;
  esac
  shift
done

[ -n "$PBF" ] && [ -n "$BOUNDS" ] && [ -n "$OUT" ] || { err "--pbf・--bounds・--out が必要です（--help）"; exit 2; }
[ -f "$PBF" ] || { err ".osm.pbf がありません: $PBF"; exit 2; }
# 西,南,東,北 の 4 つの数（範囲外の地点は背景が空白になる・PIT-84）。
if ! [[ "$BOUNDS" =~ ^-?[0-9.]+,-?[0-9.]+,-?[0-9.]+,-?[0-9.]+$ ]]; then
  err "--bounds は 西,南,東,北 の 10 進度で指定してください: $BOUNDS"; exit 2
fi
command -v java >/dev/null 2>&1 || { err "java（21 以上）が見つかりません。"; exit 2; }
mkdir -p "$OUT"
JAR=${JAR:-$OUT/planetiler.jar}
[ -f "$JAR" ] || { err "planetiler.jar がありません: $JAR（--jar か PLANETILER_JAR で指定）"; exit 2; }

# planetiler は拡張子で形式を決めるので、一時ファイルも .pmtiles で終える。
TMP_OUT="$OUT/.basemap-$$.pmtiles"
trap 'rm -f "$TMP_OUT"' EXIT
# 作業用のファイル（水域ポリゴンなどの取得物・一時データ）は出力ディレクトリの下に置く。
java "-Xmx${PLANETILER_HEAP:-4g}" -jar "$JAR" \
  --osm-path="$PBF" \
  --bounds="$BOUNDS" \
  --download \
  --download-dir="$OUT/sources" \
  --tmpdir="$OUT/tmp" \
  --output="$TMP_OUT" \
  --force
mv -f "$TMP_OUT" "$OUT/basemap.pmtiles"
rm -rf "$OUT/tmp"
echo "作成しました: $OUT/basemap.pmtiles（SHIKI__MAP__DATA_DIR=$OUT）"

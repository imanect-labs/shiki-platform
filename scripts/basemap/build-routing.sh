#!/usr/bin/env bash
# build-routing.sh — 道路沿いの道順に使う Valhalla の経路データを OSM から作る（design.md §4.7・PIT-85）。
#
#   使い方:
#     build-routing.sh --pbf <地域の .osm.pbf> --out <出力ディレクトリ> [--image <Valhalla のイメージ>]
#
#   例（背景地図と同じ .osm.pbf から作る）:
#     build-routing.sh --pbf ~/osm/kanto-latest.osm.pbf --out ~/routing-data
#
#   - Valhalla（https://github.com/valhalla/valhalla）の公式イメージで、経路データ
#     （<出力ディレクトリ>/valhalla_tiles.tar）と設定（valhalla.json）を作る。docker が要る。
#   - 背景地図（build-basemap.sh）と同じ .osm.pbf から作ると、道順の線と背景の道路がずれない。
#   - 作ったら Valhalla を起動し、shiki-server の SHIKI__MAP__ROUTING_URL にその URL を渡す:
#       docker run -d --name shiki-valhalla -p 127.0.0.1:8002:8002 -v <出力ディレクトリ>:/data \
#         <イメージ> valhalla_service /data/valhalla.json 4
#     最後の数は同時に計算する数。shiki-server も同時に 4 件までしか送らない（PIT-85）。
#       SHIKI__MAP__ROUTING_URL=http://127.0.0.1:8002
#     Valhalla は shiki-server からだけ使う（ブラウザへは公開しない）。
#   - 生成物はリポジトリに入れない（OSM の写しは配備ごとに作る）。
set -euo pipefail

err() { printf '%s\n' "$*" >&2; }

PBF=""; OUT=""
# 版を固定する（latest は中身が変わる）。上げるときはここを変える。
IMAGE="ghcr.io/valhalla/valhalla@sha256:8c6287c7a99961325f9a152c8756c2ebde9f1919f8cde4e1754dafcf10362dc1"
while [ $# -gt 0 ]; do
  case "$1" in
    --pbf) PBF=${2:?}; shift ;;
    --out) OUT=${2:?}; shift ;;
    --image) IMAGE=${2:?}; shift ;;
    -h|--help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) err "未知の引数: $1"; exit 2 ;;
  esac
  shift
done
[ -n "$PBF" ] && [ -n "$OUT" ] || { err "--pbf と --out が必要です（--help）"; exit 2; }
[ -f "$PBF" ] || { err ".osm.pbf がありません: $PBF"; exit 2; }
command -v docker >/dev/null 2>&1 || { err "docker が見つかりません。"; exit 2; }
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
cp -f "$PBF" "$OUT/region.osm.pbf"

# 作業は別名のディレクトリで行い、できたら rename で置き換える（使用中の経路データを書きかけにしない）。
docker run --rm -v "$OUT:/data" --entrypoint bash "$IMAGE" -c '
  set -euo pipefail
  rm -rf /data/.build && mkdir -p /data/.build/tiles
  # 車の道順も Yui の上限（50 地点）まで通す（Valhalla の既定は 20）。
  valhalla_build_config --mjolnir-tile-dir /data/.build/tiles \
    --mjolnir-tile-extract /data/.build/valhalla_tiles.tar \
    --mjolnir-timezone "" --mjolnir-admin "" \
    --service-limits-auto-max-locations 50 > /data/.build/valhalla.json
  valhalla_build_tiles -c /data/.build/valhalla.json /data/region.osm.pbf
  valhalla_build_extract -c /data/.build/valhalla.json -v --overwrite
  # 配信時は tar だけを読む。新しい tar は別名で作ってから rename する（配信中の Valhalla が開いている
  # 古い tar の中身は書き換わらない。読み直すには Valhalla を再起動する）。
  sed -e "s#/data/.build/valhalla_tiles.tar#/data/valhalla_tiles.tar#" \
      -e "s#/data/.build/tiles#/data/tiles-unused#" /data/.build/valhalla.json > /data/.build/valhalla.json.new
  mv -f /data/.build/valhalla_tiles.tar /data/valhalla_tiles.tar
  mv -f /data/.build/valhalla.json.new /data/valhalla.json
  rm -rf /data/.build /data/region.osm.pbf
'
echo "作成しました: $OUT/valhalla_tiles.tar と $OUT/valhalla.json"

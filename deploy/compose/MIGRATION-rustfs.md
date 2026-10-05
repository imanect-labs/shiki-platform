# 既存 compose 環境を MinIO から RustFS へ移す

**ボリュームを使い回してはいけない。** RustFS のオンディスク形式は MinIO と
非互換で、`minio-data` を `rustfs-data` としてマウントしても既存オブジェクトは
一切見えない。移行は必ず S3 API 経由でコピーする。

対象は「既に MinIO でデータを持っている compose 環境」だけ。新規構築では不要。

## 事前に確認すること

- **イメージも作り直すこと。** `SHIKI__STORAGE__BACKEND` の値が `minio` →
  `s3` に変わっている。旧バイナリのイメージが残った状態で compose だけ更新して
  `docker compose up -d`（`--build` 無し）すると
  `unknown variant: found `s3`, expected `minio` or `gcs`` で起動ループする。
  逆方向（新バイナリが旧 `minio` を読む）は serde alias で通る。
- **資格情報。** `.env` に `MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD` を書いて
  いる場合はそのまま引き継がれる（compose が
  `RUSTFS_ACCESS_KEY:-${MINIO_ROOT_USER:-minioadmin}` で受けている）。
  この機会に `RUSTFS_ACCESS_KEY` / `RUSTFS_SECRET_KEY` へ書き換えてよい。
- **MinIO のイメージは upstream から消えている。** 手元にキャッシュが残っている
  うちに移行すること。`docker image prune -a` を打つと復旧手段が無くなる。

## 手順

### 0. 書き込み元を止める

**先に止めないとコピーが壊れる。** `docker compose up <サービス>` は指定した
サービスとその依存だけを起動し、**既に動いている `shiki-server` は止めない。**
コピー中にオブジェクトが更新されると、手順 2 の検証は「同じキーが両側にある」
ことしか見ないので、**RustFS 側に古い内容が残ったまま failed=0 で通る。**
そのまま手順 4 で旧ボリュームを消すと、その更新は失われる。

```bash
cd deploy/compose
# MinIO へ書き込みうるものを全部止める。
docker compose stop shiki-server ingestion-worker
# 確認（これらが up のままなら先へ進まない）
docker compose ps shiki-server ingestion-worker
```

Collabora も WOPI 経由で `shiki-server` に書かせるが、`shiki-server` が止まって
いれば書けない。再開は手順 3。

### 1. 旧 MinIO と新 RustFS を同時に上げる

MinIO は旧定義のまま別ポートで残し、RustFS を本来のポートに上げる。

```bash
cd deploy/compose
cat > /tmp/migrate.yml <<'EOF'
services:
  # 旧 MinIO を読み出し専用の移行元として一時的に残す。
  minio-old:
    image: minio/minio:RELEASE.2025-04-22T22-12-26Z   # 手元のキャッシュを使う
    command: server /data --console-address ":9001"
    environment:
      MINIO_ROOT_USER: ${MINIO_ROOT_USER:-minioadmin}
      MINIO_ROOT_PASSWORD: ${MINIO_ROOT_PASSWORD:-minioadmin}
    volumes:
      - minio-data:/data          # 既存ボリュームをそのまま読む
    ports: ["127.0.0.1:19000:9000"]
volumes:
  minio-data:
    external: true
    name: <プロジェクト名>_minio-data   # docker volume ls で確認する
EOF
docker compose -f docker-compose.yml -f /tmp/migrate.yml up -d minio-old rustfs
```

### 2. オブジェクトをコピーする

`boto3` を使う（リポジトリの依存ではないので使い捨ての venv で入れる）。

```bash
python3 -m venv /tmp/s3v && /tmp/s3v/bin/pip -q install boto3
```

```python
# /tmp/copy-objects.py
# 冪等。同じ ETag のキーはスキップするので、途中で落ちてもそのまま再実行できる。
import os, sys, boto3
from botocore.config import Config

def c(ep, ak, sk):
    return boto3.client("s3", endpoint_url=ep, aws_access_key_id=ak, aws_secret_access_key=sk,
        region_name="us-east-1",
        config=Config(s3={"addressing_style": "path"}, signature_version="s3v4",
                      retries={"max_attempts": 5, "mode": "standard"}))

B  = os.environ.get("BUCKET", "shiki-blobs")
AK = os.environ.get("ACCESS_KEY", "minioadmin")
SK = os.environ.get("SECRET_KEY", "minioadmin")
src = c(os.environ["SRC_ENDPOINT"], AK, SK)   # 旧 MinIO
dst = c(os.environ["DST_ENDPOINT"], AK, SK)   # 新 RustFS

try: dst.head_bucket(Bucket=B)
except Exception: dst.create_bucket(Bucket=B)

existing = {}
for p in dst.get_paginator("list_objects_v2").paginate(Bucket=B):
    for o in p.get("Contents", []): existing[o["Key"]] = o["ETag"]

copied = skipped = failed = 0
for p in src.get_paginator("list_objects_v2").paginate(Bucket=B):
    for o in p.get("Contents", []):
        if existing.get(o["Key"]) == o["ETag"]:
            skipped += 1; continue
        try:
            body = src.get_object(Bucket=B, Key=o["Key"])["Body"].read()
            dst.put_object(Bucket=B, Key=o["Key"], Body=body); copied += 1
        except Exception as e:
            failed += 1; print(f"FAIL {o['Key']}: {e}", file=sys.stderr)

# **キーと ETag の両方を突き合わせる。** 件数だけ見ると、
#   - ページングの取りこぼし
#   - コピー後に更新されたキー（キーは両側にあるが内容が違う）
# のどちらも検出できない。手順 0 で書き込みを止めていれば後者は起きないが、
# 止め忘れをここで捕まえる。
def inventory(cl):
    inv = {}
    for p in cl.get_paginator("list_objects_v2").paginate(Bucket=B):
        for o in p.get("Contents", []): inv[o["Key"]] = o["ETag"]
    return inv
si, di = inventory(src), inventory(dst)
missing = sorted(set(si) - set(di))
mismatch = sorted(k for k in set(si) & set(di) if si[k] != di[k])
print(f"copied={copied} skipped={skipped} failed={failed} / src={len(si)} dst={len(di)}")
if missing:  print(f"dst に無いキー {len(missing)} 件: {missing[:5]}", file=sys.stderr)
if mismatch: print(f"ETag 不一致 {len(mismatch)} 件（コピー中に更新された疑い）: {mismatch[:5]}", file=sys.stderr)
sys.exit(1 if (failed or missing or mismatch) else 0)
```

**資格情報を渡すこと。** 既定は `minioadmin` なので、`.env` で
`MINIO_ROOT_USER` / `MINIO_ROOT_PASSWORD`（または `RUSTFS_*`）を変えている環境で
省略すると認証エラーで落ちる。実値は compose から取る。

```bash
cd deploy/compose
AK=$(docker compose exec -T rustfs printenv RUSTFS_ACCESS_KEY | tr -d '\r\n')
SK=$(docker compose exec -T rustfs printenv RUSTFS_SECRET_KEY | tr -d '\r\n')
cd ../..

SRC_ENDPOINT=http://127.0.0.1:19000 DST_ENDPOINT=http://127.0.0.1:9000 \
  ACCESS_KEY="$AK" SECRET_KEY="$SK" \
  /tmp/s3v/bin/python /tmp/copy-objects.py
```

> 旧 MinIO と新 RustFS で資格情報が違う場合は、スクリプトの `src` / `dst` を
> 別々のキーで作るよう手で直す（上の例は両者が同じ前提）。

**`failed=0` かつ「全キーの ETag が一致」になるまで次へ進まない。**
件数一致だけでは、コピー後に更新されたキーを見逃す。

### 3. 切り替えて確認する

```bash
docker compose -f docker-compose.yml -f /tmp/migrate.yml rm -sf minio-old
# 手順 0 で止めたものを再開する。--build を忘れないこと（BACKEND が s3 に変わっている）。
docker compose up -d --build shiki-server ingestion-worker
```

アプリから確認する（最低限この 3 つ）。

1. 既存ファイルの**ダウンロード**ができる（presigned GET が旧データに当たる）
2. 新規**アップロード**ができる（presigned PUT ＋ バケット CORS）
3. 既存ファイルの**版履歴**が見える（blob の content-addressing が保たれている）

### 4. 旧ボリュームを消す

**上の確認が全部通ってから。** 消すと戻せない。

```bash
docker volume rm <プロジェクト名>_minio-data
```

## 失敗時

| 症状 | 原因 | 対処 |
| --- | --- | --- |
| `unknown variant: found `s3`` で起動ループ | 旧バイナリのイメージが残っている | `docker compose up -d --build shiki-server` |
| アップロードだけ CORS で失敗 | `SHIKI__STORAGE__S3__CORS_ALLOWED_ORIGINS` 未設定 | compose の既定 `[*]` が効いているか `docker compose config` で確認 |
| ダウンロードが 404 | コピー前に切り替えた | 手順 1〜2 をやり直す（旧ボリュームを消していなければ復旧できる） |
| S3 認証エラー | `.env` の資格情報が旧 MinIO と食い違う | `docker compose exec rustfs printenv RUSTFS_ACCESS_KEY` で実値を確認 |

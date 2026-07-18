# Collabora Online 自前ビルド（Task 11.5）

Office 文書（docx/xlsx/pptx）のブラウザ編集に使う Collabora Online を、
**ソースから自前ビルド**するための Dockerfile 一式。

## なぜソースビルドか（human 確定判断）

- 公式 **CODE** バイナリ/イメージには **10 文書・20 接続**の制限が焼き込まれ、制限解除は
  商用サブスクリプション前提。オンプレ配布のたびに商用契約を結ぶ運用は取れない。
- Collabora Online（coolwsd）と Collabora Office core のソースは **MPLv2**。
  自前ビルドなら機能制限・契約なしで配布物へ同梱できる（エアギャップ可）。
- 代償: セキュリティ更新への追従・ビルドの供給網管理を自分で負う（下記手順）。

改変はしない（**フォークではなく無改変ビルド**）。`vendor/` のフォーク所有物とは違い
[fork-policy](../../../docs/sandbox/fork-policy.md) の対象外だが、ピン・追従・検証の規律は
同じ精神で運用する（PIT-43）。

## ライセンス（MPLv2 の配布義務）

- MPLv2 はファイル単位のコピーレフト。**無改変ビルドの配布**では「対応ソースの入手先の明示」で足りる。
- 本イメージは `/etc/collabora/CORE_COMMIT` / `/etc/collabora/ONLINE_COMMIT` に
  ビルド元コミットを焼き込む。入手先は `office-manifest.env` 記載の上流リポジトリ。
- 万一パッチを当てる場合は MPLv2 §3.2 に従い当該ファイルのソース提供が必要になる
  （その時点で fork-policy の対象に昇格させ、`vendor/` へ移す）。

## ピンと検証（PIT-33/PIT-43）

`office-manifest.env` が正: リリースタグ＋**peel 済みコミット SHA** の二重ピン。
Dockerfile はクローン直後に `git rev-parse HEAD` が期待 SHA と一致しなければ失敗する
（上流でのタグ付け替えを検知）。実行時ダウンロードは一切ない。

## ビルド（CI が正・数時間級）

PR CI では走らない（`docker-image.yml` から除外）。`office-image.yml` が

- `deploy/docker/collabora/**` 変更の push（main）
- 手動 `workflow_dispatch`
- 週次スケジュール（上流セキュリティタグの取り込み漏れ検知・ビルド腐敗検知）

でビルドし、GHCR へ `ghcr.io/<owner>/shiki-collabora:<ONLINE_REF>` として push する。

ローカルでフルビルドする場合（RAM 16GB+・ディスク 100GB+・数時間）:

```bash
set -a; source deploy/docker/collabora/office-manifest.env; set +a
docker build deploy/docker/collabora \
  --build-arg CORE_REPO --build-arg CORE_REF --build-arg CORE_COMMIT \
  --build-arg ONLINE_REPO --build-arg ONLINE_REF --build-arg ONLINE_COMMIT \
  -t shiki-collabora:local
```

## バージョン追従

1. 上流のリリースタグを確認（core/online は同一トレイン cp-XX.YY.x を選ぶ）:

   ```bash
   git ls-remote https://github.com/CollaboraOnline/online.git 'refs/tags/cp-*'
   git ls-remote https://git.libreoffice.org/core 'refs/tags/cp-*'
   ```

2. `office-manifest.env` の REF と COMMIT（`^{}` の peel 済み SHA）を更新して PR。
3. マージ後 `office-image.yml` が新イメージを push → compose の `COLLABORA_IMAGE` を更新。

## 開発・CI の暫定フォールバック

フルビルドは重いため、**開発と CI に限り** 公式 CODE イメージのピン利用を許す
（compose の `COLLABORA_IMAGE` で差し替え。CODE の接続数制限は dev では実害なし）:

```bash
COLLABORA_IMAGE=collabora/code:26.04.2.1.1 docker compose --profile office up -d
```

**配布物（オンプレ顧客へ渡すもの）は必ず自前ビルドイメージを使う**（ライセンス妥協なし）。

## 実行時の注意

- coolwsd の jail 生成のため `coolforkit` にファイル capability
  （`cap_fowner,cap_chown,cap_mknod,cap_sys_chroot`）を付けている。いずれも Docker 既定の
  cap 集合内だが、**`no-new-privileges` を付けると file caps が獲得できず起動しない**
  （compose の collabora サービスに security_opt を足さないこと）。
- TLS はリバースプロキシ終端（コンテナ内は平文 9980・`ssl.enable=false`）。
- WOPI ホスト許可（`storage.wopi.host`）と `server_name` は compose 側 command で注入する。

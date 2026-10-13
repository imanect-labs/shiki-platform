# Collabora Online 自前ビルド（Task 11.5）

Office 文書（docx/xlsx/pptx）のブラウザ編集に使う Collabora Online を、
**ソースから自前ビルド**するための Dockerfile 一式。

## なぜソースビルドか（human 確定判断）

- 公式 **CODE** バイナリ/イメージは Collabora が本番利用を想定していない（[CODE](https://www.collaboraonline.com/code/)）。
  無償で本番に使うなら「MPLv2 ソースから**自前でビルドし、自前のブランドで出し、自前でサポートする**」ことが
  条件（[Collabora の回答](https://forum.collaboraonline.com/t/code-binary-license/179)）。
  なお「10 文書・20 接続」の上限が掛かるのは `home_mode` を有効にしたときだけで、現行の CODE の既定は無制限。
- Collabora Online（coolwsd）と Collabora Office core のソースは **MPLv2**。
  自前ビルドなら機能制限・契約なしで配布物へ同梱できる（エアギャップ可）。自前ビルドの上限は
  configure の既定どおり 9999 接続・9999 文書（`--with-welcome-url` を付けていないので `home_mode` も効かない）。
- 代償: セキュリティ更新への追従・ビルドの供給網管理を自分で負う（下記手順）。

改変はしない（**フォークではなく無改変ビルド**）。`vendor/` のフォーク所有物とは違い
[fork-policy](../../../docs/sandbox/fork-policy.md) の対象外だが、ピン・追従・検証の規律は
同じ精神で運用する（PIT-43）。

## ブランド（Shiki Office・#549）

Collabora の[商標ポリシー](https://www.collaboraonline.com/trademark-policy/)は、Collabora が配布した
無改変バイナリ以外では Collabora の名称・ロゴを外すことを求める。自前ビルドは **Shiki Office**
（ベンダー: Imanect Labs）として出す。**ソースは無改変のまま**、configure の引数と設定だけで行う。

| 何を | どこで |
| --- | --- |
| core のベンダー・ロゴ | `--with-vendor` / `--with-branding`（このディレクトリの `branding/core/` を `/src/branding` へコピーして渡す。7 ファイル。欠けると LibreOffice のロゴで埋まる） |
| online の製品名（WOPI の capabilities 等） | `--with-app-name` |
| About の著作権表示（`Copyright © <年>, <vendor>.`） | online の `--with-vendor="the Collabora Online and LibreOffice contributors"`。**著作権表示なので自社名にしない**（無改変の上流コードの著作権者は上流の貢献者。未指定だとビルドユーザー名 root が出る） |
| Collabora のヘルプへのリンク | `--without-help-url` |
| UI の製品名・リンク・ロゴ | `coolwsd.xml` の `user_interface.brandProductName` / `brandProductURL`（`https://www.imanect.co.jp/`）/ `logoURL=none` をイメージに焼き込む |
| Collabora のフォーラム・Issue へ飛ぶヘルプ項目 | `branding/online/branding.css`（coolwsd が未ブランドのビルドでも読み込む） |
| Collabora Office の有償 EULA（`EULA.odt` / `EULA_en-US.rtf`） | core-build で削除（MPL のライセンス表示ではない。LICENSE / NOTICE は残す） |

ビルド時の検査（製品名・ベンダー・`coolwsd.xml`）と smoke の検査（capabilities・エディタのページ・
`branding.css`・同梱物）が、どれかが崩れたら止める。

**core（文書エンジン）の内部の製品名は "Collabora Office" のまま**（human 判断）。Collabora 版の core は
インストーラの設定（`openoffice.lst.in`）に製品名を直書きしており、`--with-product-name` で変えると
ooinstall が落ちる。変えるにはソースの改変が要る。利用者の目に入るのは About の
「This version of Shiki Office is powered by」に並ぶエンジンの版（"Collabora Office 25.04"）と
上流のコミットへのリンクだけで、これは出所の事実の記載にあたる。ダイアログのタイトル中の名前は
online が Shiki Office に置き換える。

ほかに残っている Collabora の文字列は、Zotero 連携や更新通知など普段は出ない画面のメッセージだけ。
これもソースを書き換えないと消せない。

開発・CI のフォールバック（公式 CODE）は Collabora のブランドのまま。配布物には使わないこと。

## ライセンス（MPLv2 の配布義務）

- MPLv2 はファイル単位のコピーレフト。**無改変ビルドの配布**では「対応ソースの入手先の明示」
  （§3.2(a)）で足りる。ソース中のライセンス表示は消さない（§3.4）。
- core は `--enable-mpl-subset` でビルドしている（MPL より制約の強いライセンスの部品があると configure が止まる）。
- 万一パッチを当てる場合は MPLv2 §3.2 に従い当該ファイルのソース提供が必要になる
  （その時点で fork-policy の対象に昇格させ、`vendor/` へ移す）。

### イメージに入っている告知（`/usr/share/doc/shiki-office/`）

| ファイル | 中身 |
| --- | --- |
| `NOTICE` | 構成要素ごとのライセンスとソースの入手先（リポジトリ・タグ・コミット）、ライセンス文書の場所、`-sources` イメージの案内 |
| `core-external-tarballs.sha256` | core が同梱する外部ライブラリ（ICU・NSS・OpenSSL・curl・cairo・フォント等）のソース tarball と sha256 |
| `ubuntu-source-packages.txt` | イメージ内の Ubuntu パッケージのソースパッケージ名と版 |
| `poco-LICENSE` | Poco の BSL-1.0 本文 |
| `ubuntu-snapshot.txt` | それらを入れたときの Ubuntu archive の時刻（`snapshot.ubuntu.com` でその時点の archive を引ける） |

Poco は既定（`POCO_UNBUNDLED=OFF`）で PCRE2・zlib・Expat・utf8proc を自分のソースから取り込んでビルドする。
それらの著作権表示とライセンス本文は、同じ上流の Ubuntu パッケージの `/usr/share/doc/<pkg>/copyright` を
NOTICE から参照している（utf8proc のためにランタイムへ `libutf8proc3` を入れている）。

ライセンス文書そのものは `/opt/collaboraoffice/LICENSE.html`・`/usr/share/doc/coolwsd/THIRDPARTYLICENSES`・
`/usr/share/doc/*/copyright` にある。

### ソースの保全（`-sources` イメージ）とタグ

`office-image.yml` は smoke の後、`--target sources` で**対応するソース一式**を固めたイメージを先に push し、
それが成功してから本体を push する（対応ソースの無い本体を公開しないため）。上流のリポジトリやミラーが
消えても提供できるようにするため。

| タグ | 中身 | 上書き |
| --- | --- | --- |
| `<ONLINE_REF>-<BUILD_ID>` | 本体（このビルド） | されない |
| `<ONLINE_REF>-<BUILD_ID>-sources` | 上の本体に対応するソース一式。本体の NOTICE はこれを案内する | されない |
| `<ONLINE_REF>` / `latest` / `<ONLINE_REF>-sources` | 最新のビルド | 週次の再ビルド等で上書きされる |

`BUILD_ID` は `b<run_id>-<run_attempt>`。**配布・デプロイでは不変タグ（`<ONLINE_REF>-<BUILD_ID>`）を指す**
（上書きされるタグだと、ノードに残った古いイメージが使われたり、ソースとの対応が崩れたりする）。
`-sources` の中身は NOTICE の構成要素と 1 対 1:

```
core-<CORE_REF>.tar.gz      core（submodule 込み・git の追跡対象のみ）
core-external/              core が取得した外部ライブラリの tarball
online-<ONLINE_REF>.tar.gz  online
poco.tar.gz                 Poco（PCRE2 / zlib / Expat / utf8proc の同梱ソースを含む）
ubuntu/                     Ubuntu パッケージのソース（.dsc / orig / debian、イメージ内の版そのもの。
                            ランタイムが apt-get update した時刻のスナップショットから取る）
doc/                        NOTICE ほか（上の表）
```

**オンプレ顧客への納品物には、本体と、同じ `BUILD_ID` の `-sources` イメージを添える**（NOTICE がそう案内している。
Ubuntu の GPL パッケージのソース提供義務もこれで満たす）。

取り出し方:

```bash
docker create --name src ghcr.io/<owner>/shiki-collabora:<ONLINE_REF>-<BUILD_ID>-sources none
docker cp src:/ ./shiki-collabora-sources && docker rm src
```

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
docker build deploy/docker/collabora --target runtime \
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
（compose の `COLLABORA_IMAGE` で差し替え。CODE は Collabora のブランドのままだが dev では実害なし）:

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

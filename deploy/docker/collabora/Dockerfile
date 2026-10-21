# Collabora Online（coolwsd＋Collabora Office core）の**ソース自前ビルド**（Task 11.5・PIT-43）。
#
# なぜソースビルドか（human 確定判断・docs/design.md §4.8）:
#   公式 CODE バイナリは Collabora が本番利用を想定しておらず、無償で本番に使う条件は
#   「MPLv2 ソースから自前でビルドし、自前のブランドで出し、自前でサポートする」こと。
#   自前ビルドなら制限・契約なしでオンプレ同梱できる。Shiki Office としてブランドする（#549・README.md）。
#   代償としてサプライチェーンと鮮度の責任を自分で持つ（PIT-43・README.md の追従手順参照）。
#
# 取得物は office-manifest.env の「リリースタグ＋peel 済みコミット SHA」で二重にピンする
# （実行時ダウンロードなし・PIT-33 と同型）。クローン後に HEAD が期待コミットと一致しなければ
# 即失敗する（タグ差し替え攻撃の検知）。
#
# ビルドは数時間級のため PR CI では走らせず（docker-image.yml から除外）、
# .github/workflows/office-image.yml（manifest 変更/タグ/手動トリガ）でビルドして
# レジストリへ push し、compose はそのイメージを参照する。

# ---- Stage 1: engine（Collabora Office core / LOKit）のビルド ----
#
# **26.04 から core と online は 1 つのリポジトリ（monorepo）になった。** core は online の
# `engine/` にあり、online の一部（gbuild で建てる C++）は engine の**ビルドツリー**を使う
# （上流の README-gbuild.md）。インストール済みの /opt/collaboraoffice だけでは建たないので、
# engine と online を同じステージ系列で建てる（online-build は FROM core-build）。
# Poco も engine の外部ライブラリとして静的に建つようになり、別ステージは要らない。
#
# 取得元は online.mirror（gerrit.collaboraoffice.com の公開ミラー）。online.git には
# 26.04 のタグが無く、core の gerrit（git.libreoffice.org）にも cp-26.04 のタグは無い。
FROM ubuntu:24.04 AS core-build
ARG ONLINE_REPO=https://github.com/CollaboraOnline/online.mirror.git
ARG ONLINE_REF=cp-26.04.5-1
ARG ONLINE_COMMIT=22ce52fb5173821dd90782638afcbfb6b6d7cb40
ARG TRANSLATIONS_REPO=https://gerrit.collaboraoffice.com/translations
ARG TRANSLATIONS_REF=cp-26.04.5-1
ARG TRANSLATIONS_COMMIT=ed43817bd4972e81feb228a0ad06a8aa783d3f55
ARG NODE_URL=https://nodejs.org/dist/v22.23.4/node-v22.23.4-linux-x64.tar.xz
ARG NODE_SHA256=1805bcd35e6815559bd2eb97db438d3fdb0eb3a5e6a977b7c1badca76337e9ce
ENV DEBIAN_FRONTEND=noninteractive
# engine と online の両方のビルド依存（online は同じツリーで後から建てる）。
# **fakeroot / unzip / gettext**: engine の configure が要求する（欠けると数秒で落ちる・25.04 で実測）。
# **python3-lxml / python3-polib**: online の configure が import して確かめる。
# **cpio / file / fontconfig / fonts-dejavu-core**: online の make の中で coolwsd-systemplate-setup が
#   走り（jail の雛形）、欠けると `cpio: command not found` / `cannot stat '/usr//share/fonts'` で落ちる。
# **rsync**: browser/Makefile.am の install-data-hook が使う。
# **meson / ninja-build**: 26.04 の engine は harfbuzz を meson で建てる（configure:
#   "you need to have meson and ninja in order to build harfbuzz"）。
# **libpoco-dev は入れない**（Poco は engine が建てる）。
RUN apt-get update && apt-get install -y --no-install-recommends \
    git build-essential autoconf automake libtool pkg-config python3 python3-dev \
    python3-polib python3-lxml \
    zip unzip ccache flex bison gperf nasm libx11-dev libxml2-dev libxml2-utils libxslt1-dev \
    libcups2-dev libfontconfig1-dev libxrandr-dev libgl-dev libglu1-mesa-dev \
    libkrb5-dev libnss3-dev libpam0g-dev libssl-dev libcairo2-dev \
    libavahi-client-dev libdbus-glib-1-dev ca-certificates curl fakeroot gettext xz-utils \
    libcap-dev libzstd-dev libpng-dev zlib1g-dev libcppunit-dev libpcre2-dev \
    cpio file fontconfig fonts-dejavu-core rsync meson ninja-build \
    && rm -rf /var/lib/apt/lists/*
# **Node は 20 以上が要る**（online の configure: "This node version is old, upgrade to >= 20.0.0"）。
# Ubuntu 24.04 の nodejs は 18 なので、公式の tarball を sha256 で固定して入れる。
RUN curl -fsSL -o /tmp/node.tar.xz "$NODE_URL" \
    && echo "$NODE_SHA256  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
    && rm /tmp/node.tar.xz \
    && node --version
# **core は root でビルドできない。** LibreOffice の Makefile に
#   check-if-root: Building LibreOffice as root is a very bad idea, use a regular user.
# という明示の拒否があり、make が即 Error 1 になる（実測）。online も同じユーザーで建てる。
RUN useradd -m build \
    && mkdir -p /src /opt/collaboraoffice /inst \
    && chown -R build:build /src /opt/collaboraoffice /inst
USER build
WORKDIR /src
RUN git clone --depth 1 --branch "$ONLINE_REF" "$ONLINE_REPO" online \
    && cd online \
    && test "$(git rev-parse HEAD)" = "$ONLINE_COMMIT" \
    && git rev-parse HEAD > /src/ONLINE_COMMIT && cat /src/ONLINE_COMMIT \
    && test -x engine/autogen.sh
# engine の翻訳（monorepo の外）。無いと configure は警告 1 行で英語だけに落とす。
RUN git clone --depth 1 --branch "$TRANSLATIONS_REF" "$TRANSLATIONS_REPO" online/engine/translations \
    && cd online/engine/translations \
    && test "$(git rev-parse HEAD)" = "$TRANSLATIONS_COMMIT" \
    && test -f README
# LOKit 向けの構成（GUI 依存を切る）。--with-distro が依存/機能セットを固定する
# （QuickJS の JS UNO＝executescript も CPLinuxCommon の --enable-quickjs で入る）。
#
# **configure と make をレイヤで分ける。** 一体にすると、数時間走った末に落ちたのか
# configure の数秒で落ちたのかがログの末尾からは読めない。
#
# **--disable-epm --without-package-format**: CPLinux-LOKit の既定は deb / rpm を作るが、
# ここでは instdir をそのまま使う（上流の docker/from-source/build.sh と同じ）。
#
# **Shiki Office としてブランドする（#549）。** distro 設定より後ろの引数が勝つ。
#   --with-vendor: versionrc の Vendor と About の「supplied by」に出る
#   --with-branding: 7 ファイルすべてを用意する。欠けた分は既定（LibreOffice のロゴ）で埋まる
# **--with-product-name は渡さない**（エンジンの内部名は "Collabora Office" のまま・human 判断・README）。
COPY --chown=build:build branding/core /src/branding
RUN cd online/engine && ./autogen.sh --with-distro=CPLinux-LOKit --disable-symbols \
    --disable-epm --without-package-format \
    --with-vendor="Imanect Labs" --with-branding=/src/branding \
    && grep -Eq '^WITH_LANG_LIST=.*\bja\b' config_host_lang.mk \
    || { echo "engine の言語に ja が入っていない（翻訳を取り込めていない）"; exit 1; }
RUN cd online/engine && make -j"$(nproc)"
# instdir をランタイムの置き場所へ写す（上流の build.sh と同じ）。make とは別レイヤにする
# （ここで落ちても make の結果がキャッシュに残る）。
# - **EULA は Collabora Office の有償サブスクリプション契約書**で、MPL のライセンス表示
#   （LICENSE / NOTICE。§3.4 で消してはならないもの）ではない。自前ビルドの配布物に残さない（#549）。
# - ベンダー（versionrc の Vendor）が入ったことを確かめる。上流の既定は "Collabora"。
# - engine が make 中に取得した外部ライブラリ（Poco を含む）の tarball を sha256 付きで記録する。
#   ランタイムの告知（NOTICE）と、ソース保全イメージ（sources ステージ）の両方が使う。
# - Poco は engine の中で静的に建つ。告知に添える BSL-1.0 の本文を展開済みのツリーから持ち出す。
RUN cd online/engine \
    && cp -a instdir/. /opt/collaboraoffice/ \
    && test -f /opt/collaboraoffice/program/versionrc \
    && grep -q '^Vendor=Imanect Labs' /opt/collaboraoffice/program/versionrc \
    && rm -f /opt/collaboraoffice/EULA.odt /opt/collaboraoffice/EULA_en-US.rtf \
    && test -f /opt/collaboraoffice/LICENSE.html \
    && test -f include/LibreOfficeKit/LibreOfficeKit.h -o -d include/COKit \
    && (cd external/tarballs && find . -maxdepth 1 -type f ! -name '.*' -printf '%f\n' \
        | sort | xargs -r sha256sum) > /src/CORE_EXTERNAL_TARBALLS.sha256 \
    && test -s /src/CORE_EXTERNAL_TARBALLS.sha256 \
    && grep -q 'poco' /src/CORE_EXTERNAL_TARBALLS.sha256 \
    && cp workdir/UnpackedTarball/poco/LICENSE /src/poco-LICENSE
# **ソース保全用に monorepo のソース一式を固める（#549）。** git の追跡対象だけを入れ、
# workdir/ や instdir/ の成果物は混ぜない。submodule があれば make の後に揃っているので込みで入れる。
# --transform の flags=r: 既定（rsh）だとシンボリックリンクのリンク先にも接頭辞が付き、
# 展開すると壊れたリンクになる。
RUN mkdir -p /src/sources \
    && cd online \
    && git ls-files --recurse-submodules -z \
       | tar --null -T - --transform "flags=r;s,^,online-$ONLINE_REF/," -czf /src/sources/online-$ONLINE_REF.tar.gz \
    && tar tzf /src/sources/online-$ONLINE_REF.tar.gz "online-$ONLINE_REF/engine/configure.ac" >/dev/null \
    && cd engine/translations \
    && git ls-files -z \
       | tar --null -T - --transform "flags=r;s,^,translations-$TRANSLATIONS_REF/," -czf /src/sources/translations-$TRANSLATIONS_REF.tar.gz

# ---- Stage 2: coolwsd（Collabora Online 本体）のビルド ----
# engine のビルドツリーの上で建てる（上の説明）。
FROM core-build AS online-build
# ブランド（#549）:
#   --with-app-name: 既定は "Collabora Online Development Edition"。WOPI の
#     /hosting/capabilities の productName 等に出る
#   --with-vendor: About の「Copyright © <年>, <vendor>.」に出る（Control.AboutDialog.ts）。
#     **著作権表示なので Imanect Labs にしない**（無改変の上流コードの著作権者は上流の貢献者）。
#     上流の cool.html.m4 は引用符なしで埋め込むので空白で切れる。branding.js で正す（runtime で検査）。
#   オンラインヘルプ: 26.04 は既定で無効（--with-help-url を渡したときだけ外へ飛ぶ）。
# --with-lokit-path: engine のヘッダ（ツリーの include/）。--with-lo-path: 実行時の engine の場所。
# --disable-tests: C++ の単体テストを建てない（上流の build.sh と同じ）。
RUN cd online && ./autogen.sh \
    && ./configure --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
         --with-lokit-path=/src/online/engine/include \
         --with-lo-path=/opt/collaboraoffice \
         --with-app-name="Shiki Office" \
         --with-vendor="the Collabora Online and LibreOffice contributors" \
         --disable-tests --enable-silent-rules
RUN cd online \
    && make -j"$(nproc)" \
    && make install DESTDIR=/inst \
    && test -x /inst/usr/bin/coolwsd

# ---- Stage 3: ランタイム ----
# **名前付きステージなので、ビルドでは `--target runtime` を指定する。** 後ろに
# ソース保全用のステージ（sources）が続くため、無指定だとそちらが建つ。
FROM ubuntu:24.04 AS runtime
ARG ONLINE_REPO=https://github.com/CollaboraOnline/online.mirror.git
ARG ONLINE_REF=cp-26.04.5-1
ARG ONLINE_COMMIT=22ce52fb5173821dd90782638afcbfb6b6d7cb40
ARG TRANSLATIONS_REPO=https://gerrit.collaboraoffice.com/translations
ARG TRANSLATIONS_REF=cp-26.04.5-1
ARG TRANSLATIONS_COMMIT=ed43817bd4972e81feb228a0ad06a8aa783d3f55
ENV DEBIAN_FRONTEND=noninteractive
# ランタイム依存は dev パッケージで引き込む（noble の t64 リネーム等、実ライブラリの
# パッケージ名差異に依存しないため。ヘッダ分のサイズ増は堅牢性とのトレード）。
# 日本語フォント（Noto CJK）はオンプレ・エアギャップで文書の見た目を保つため同梱する。
#
# cpio / tzdata / fontconfig: 下の coolwsd-systemplate-setup が使う（online-build と同じ理由）。
# このスクリプトは cpio が無くても**exit 0 で終わる**ので、ビルドは通るのに
# jail の雛形が空になり、coolwsd が起動直後に落ちる。
#   /usr/bin/coolwsd-systemplate-setup: line 88: cpio: command not found
#   cp: cannot stat '/etc/timezone': No such file or directory
# tzdata は /etc/timezone と /etc/localtime を、fontconfig は /etc/fonts と
# /var/cache/fontconfig を供給する（いずれも雛形へコピーされる）。
#
# 2 行目は **core-build でリンクに使った -dev と同じ並び**（ビルド専用ツールは除く）。
# 欠けると coolforkit が LOKit を読めず、子プロセスが 1 つも上がらない。
#   Failed to load .../libmergedlo.so: libgssapi_krb5.so.2: cannot open shared object file
#   FTL  No child process could be created. Exiting.   （exit 70）
# core-build の apt を変えたらここも揃えること（漏れは下の ldd 検査で止まる）。
#
# **upgrade してから入れる（#549）。** ベースイメージに入っている古い版のままだと、
# sources ステージがその版のソースを deb-src から取れない（索引には各ポケットの
# 最新版しか載らない）。セキュリティ修正の取り込みにもなる。
#
# **Ubuntu スナップショット（snapshot.ubuntu.com）から入れ、その時刻を記録する（#549）。**
# このレイヤはビルドキャッシュから再利用されるので、後からライブの deb-src を引くと版がずれる。
# また、ライブの archive から入れて後でスナップショットを引く形だと、スナップショット側の
# 反映の遅れで版が見つからないことがある。入れる側も取る側も同じ時刻のスナップショットを
# 使えば、原理的に一致する（sources ステージは ubuntu-snapshot.txt の時刻で取る）。
# 時刻は CI が 1 回だけ決めて UBUNTU_SNAPSHOT で渡す。runtime（load）・sources・push は
# 別々のビルドなので、間でキャッシュが消えてこの RUN がやり直しになったときに、`date` だと
# 本体と -sources で時刻（＝版）がずれうる。未指定（手元のビルド）なら今の時刻を使う。
# ca-certificates だけは、スナップショットに https で繋ぐために先にライブから入れる
# （その後の upgrade でスナップショットの版に揃う。揃わない場合は sources 側でライブから取る）。
#
# libutf8proc3: Poco が同梱している utf8proc の著作権表示とライセンス本文
# （/usr/share/doc/libutf8proc3/copyright）を NOTICE から参照するため。
# PCRE2 / zlib / expat の分は既に入っているパッケージのものを参照する。
ARG UBUNTU_SNAPSHOT=
RUN mkdir -p /usr/share/doc/shiki-office \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && snap="${UBUNTU_SNAPSHOT:-$(date -u +%Y%m%dT%H%M%SZ)}" \
    && echo "$snap" | grep -Eqx '[0-9]{8}T[0-9]{6}Z' \
    && echo "$snap" > /usr/share/doc/shiki-office/ubuntu-snapshot.txt \
    && echo "APT::Snapshot \"$snap\";" > /etc/apt/apt.conf.d/50shiki-snapshot \
    && rm -rf /var/lib/apt/lists/* && apt-get update \
    && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends \
    libssl-dev libpng-dev libzstd-dev libpam0g-dev libcap2-bin libpcre2-8-0 \
    libx11-dev libxml2-dev libxslt1-dev libcups2-dev libfontconfig1-dev libxrandr-dev \
    libgl-dev libglu1-mesa-dev libkrb5-dev libnss3-dev libcairo2-dev \
    libavahi-client-dev libdbus-glib-1-dev \
    cpio tzdata fontconfig libutf8proc3 \
    fonts-noto-cjk fonts-noto-cjk-extra ca-certificates \
    && rm -f /etc/apt/apt.conf.d/50shiki-snapshot \
    && rm -rf /var/lib/apt/lists/*
# **このイメージに入っている Ubuntu パッケージのソースパッケージと版を記録する（#549）。**
# GPL / LGPL のパッケージ（bash・coreutils・libc 等）を含むイメージを配布すると、
# そのソースの提供義務を負う。sources ステージはこの一覧どおりにソースを取得する。
# apt はこの上の 1 回しか走らせないこと（後から足すと一覧から漏れる）。
RUN dpkg-query -W -f='${source:Package} ${source:Version}\n' | sort -u \
       > /usr/share/doc/shiki-office/ubuntu-source-packages.txt \
    && test -s /usr/share/doc/shiki-office/ubuntu-source-packages.txt
# Poco は engine の中で静的にリンクされている（26.04 から・別に載せる共有ライブラリは無い）。
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
COPY --from=online-build /inst/ /
COPY --from=core-build /src/ONLINE_COMMIT /etc/collabora/ONLINE_COMMIT
COPY --from=core-build /src/CORE_EXTERNAL_TARBALLS.sha256 /usr/share/doc/shiki-office/core-external-tarballs.sha256
# **第三者ソフトウェアの告知（#549・MPL-2.0 §3.2(a)）。** 受け取った人がソースの入手先を
# 分かるようにする。コミット SHA だけでは足りないので、リポジトリ・タグ・SHA を並べる。
COPY --from=core-build /src/poco-LICENSE /usr/share/doc/shiki-office/poco-LICENSE
COPY NOTICE.in /usr/share/doc/shiki-office/NOTICE.in
# BUILD_ID: office-image.yml がビルドごとに付ける不変タグの一部。NOTICE はこのビルドに
# 対応する -sources イメージ（<ONLINE_REF>-<BUILD_ID>-sources）を案内する。
# ここで初めて宣言するのは、毎回変わる値でキャッシュを外すのをこれより後ろに限るため。
ARG BUILD_ID=local
RUN cd /usr/share/doc/shiki-office \
    && for d in libpcre2-8-0 zlib1g libexpat1 libutf8proc3; do \
         test -s "/usr/share/doc/$d/copyright" || { echo "/usr/share/doc/$d/copyright が無い"; exit 1; }; \
       done \
    && sed -e "s|@ONLINE_REPO@|$ONLINE_REPO|g" \
           -e "s|@TRANSLATIONS_REPO@|$TRANSLATIONS_REPO|g" -e "s|@TRANSLATIONS_REF@|$TRANSLATIONS_REF|g" \
           -e "s|@TRANSLATIONS_COMMIT@|$TRANSLATIONS_COMMIT|g" \
           -e "s|@ONLINE_REF@|$ONLINE_REF|g" -e "s|@ONLINE_COMMIT@|$ONLINE_COMMIT|g" \
           -e "s|@BUILD_ID@|$BUILD_ID|g" \
           NOTICE.in > NOTICE \
    && rm NOTICE.in \
    && if grep -n '@[A-Z_]*@' NOTICE; then echo "NOTICE に未置換のプレースホルダが残っている"; exit 1; fi \
    && test "$(cat /etc/collabora/ONLINE_COMMIT)" = "$ONLINE_COMMIT"
# **ブランドの上書き（#549）。** ソースは無改変のまま、未ブランドのビルドが読み込む
# branding.css / branding.js を置く（中身の説明は各ファイルの冒頭）。
COPY branding/online/branding.css branding/online/branding.js /usr/share/coolwsd/browser/dist/
# branding.js で入れ直す vendor が、configure に渡した --with-vendor と同じであること
# （cool.html には引用符なしで全文が入っているので、そこから取る）。
# 26.04 の cool.html は値を引用符で囲んで埋め込む（上流で空白の切れが直った）。引用符を外して比べる。
RUN v="$(sed -n 's/^ *data-vendor = //p' /usr/share/coolwsd/browser/dist/cool.html | tr -d '"')" \
    && test -n "$v" \
    && grep -qF "window.vendor = '$v';" /usr/share/coolwsd/browser/dist/branding.js \
    || { echo "branding.js の vendor が --with-vendor と一致しない: $v"; exit 1; }
# **共有ライブラリの解決漏れをビルド時に全部出して止める。** 実行時に初めて
# dlopen で 1 つずつ見つかる形だと、1 件ごとに CI を 1 往復（40 分）することになる。
RUN missing="$( { for f in /opt/collaboraoffice/program/*.so* \
                       /opt/collaboraoffice/program/soffice.bin \
                       /usr/bin/coolwsd /usr/bin/coolforkit*; do \
                    ldd "$f" 2>/dev/null; \
                  done; } | grep 'not found' | sort -u)" \
    && if [ -n "$missing" ]; then echo "解決できない共有ライブラリ:"; echo "$missing"; exit 1; fi
# **製品名・リンク・ロゴを coolwsd.xml の既定に焼き込む（#549）。** 起動引数（--o:）で
# 渡すと、command を丸ごと置き換える compose / K8s の側でも毎回書く必要が出る。
#   brandProductName: 空だと UI に "Collabora Online Development Edition (unbranded)" が出る
#   brandProductURL: 空だと About のリンクが collaboraonline.github.io に戻る（空にはできない）
#   logoURL=none: ロゴを出さない
RUN coolconfig set user_interface.brandProductName "Shiki Office" \
    && coolconfig set user_interface.brandProductURL "https://www.imanect.co.jp/" \
    && coolconfig set user_interface.logoURL none \
    && grep -q '>Shiki Office</brandProductName>' /etc/coolwsd/coolwsd.xml \
    && grep -q '>https://www.imanect.co.jp/</brandProductURL>' /etc/coolwsd/coolwsd.xml \
    && grep -q '>none</logoURL>' /etc/coolwsd/coolwsd.xml \
    && if grep -n 'help.collaboraoffice.com' /etc/coolwsd/coolwsd.xml; then \
         echo "help_url が Collabora のヘルプを指している（--without-help-url が効いていない）"; exit 1; fi
# **日本語フォントの別名（#725）。** 日本語の docx が指定する 游ゴシック・MS 明朝 などを、同梱の
# Noto CJK の日本語版へ寄せる（中身の説明はファイルの冒頭）。systemplate が /etc/fonts を写すので、
# その前に置く。ゴシックと明朝が取り違わないことをここで確かめる。
COPY fonts/65-shiki-ja-aliases.conf /etc/fonts/conf.d/65-shiki-ja-aliases.conf
RUN fc-cache -f >/dev/null \
    && fc-match -f '%{family[0]}\n' '游ゴシック' | grep -qx 'Noto Sans CJK JP' \
    && fc-match -f '%{family[0]}\n' 'MS 明朝' | grep -qx 'Noto Serif CJK JP' \
    || { echo "日本語フォントの別名が効いていない"; exit 1; }
# 非 root 実行（cool ユーザー）＋ systemplate/jail の初期化＋forkit へのファイル capability 付与
# （jail の chroot に必要。Docker 既定 cap 集合（CHOWN/FOWNER/MKNOD/SYS_CHROOT）内で動く。
#  compose 側で no-new-privileges を付けないこと — file caps の獲得が塞がれ起動不能になる）。
# systemplate-setup は失敗しても exit 0 なので、雛形の中身をここで確かめて止める。
# （26.04 の systemplate-setup は /etc/localtime を写さなくなったので検査しない。）
RUN useradd -m -d /opt/cool cool \
    && mkdir -p /etc/coolwsd /var/cache/coolwsd /opt/cool/child-roots \
    && coolwsd-systemplate-setup /opt/cool/systemplate /opt/collaboraoffice \
    && for f in lib64/ld-linux-x86-64.so.2 etc/ld.so.cache etc/fonts usr/share/fonts; do \
         test -e "/opt/cool/systemplate/$f" || { echo "systemplate に $f が無い"; exit 1; }; \
       done \
    && for b in /usr/bin/coolforkit /usr/bin/coolforkit-caps /usr/bin/coolmount; do \
         [ -x "$b" ] && setcap cap_fowner,cap_chown,cap_mknod,cap_sys_chroot=ep "$b" || true; \
       done \
    && chown -R cool:cool /opt/cool /etc/coolwsd /var/cache/coolwsd
USER cool
EXPOSE 9980
# WOPI ホスト許可・server_name 等の環境依存値は compose/K8s 側の command 追記で注入する。
# TLS はリバースプロキシ終端が前提（コンテナ内は平文 9980）。
#
# file_server_root_path: **必須。** make install は discovery.xml と browser/dist を
# /usr/share/coolwsd に置くが、coolwsd.xml の既定（空）だと実行ファイルの場所
# （/usr/bin/）を探して起動直後に落ちる。upstream の coolwsd.service と公式 Docker の
# 起動スクリプトも同じ値を渡している。
#   Processing discovery.xml from /usr/bin//discovery.xml
#   FTL  Exception: File not found   （exit 70）
CMD ["coolwsd", "--o:sys_template_path=/opt/cool/systemplate", \
     "--o:child_root_path=/opt/cool/child-roots", \
     "--o:lo_template_path=/opt/collaboraoffice", \
     "--o:file_server_root_path=/usr/share/coolwsd", \
     "--o:ssl.enable=false", "--o:ssl.termination=false"]

# ---- Stage 4: ソース保全（#549） ----
#
# 配布するイメージに入っているものの**対応するソース一式**を 1 つのイメージに固める。
# office-image.yml が `--target sources` で建てて `<ONLINE_REF>-sources` として push する。
# 上流のリポジトリやミラーが消えても、顧客から求められたときに提供できるようにするため。
#   - MPL-2.0（core / online）: §3.2(a) のソース入手手段
#   - GPL / LGPL（Ubuntu パッケージ）: 配布物に添えるソース
# 中身は NOTICE の「1. 構成要素」と 1 対 1 に対応する。
FROM ubuntu:24.04 AS ubuntu-sources
ENV DEBIAN_FRONTEND=noninteractive
# ca-certificates: snapshot.ubuntu.com は https のみ。
RUN apt-get update && apt-get install -y --no-install-recommends dpkg-dev ca-certificates
COPY --from=runtime /usr/share/doc/shiki-office/ubuntu-source-packages.txt /ubuntu-source-packages.txt
COPY --from=runtime /usr/share/doc/shiki-office/ubuntu-snapshot.txt /ubuntu-snapshot.txt
# ランタイムに入っている**版そのもの**のソースを取る（別の版で代用しない）。
# ランタイムが apt-get update した時刻のスナップショットから取る（ランタイムのレイヤが
# キャッシュから来ても版がずれない）。deb-src は noble の DEB822 形式の Types 行に足す。
# 取れなかったものは名前を全部出して止める。数百件あるので 8 並列で取る（1 件ずつだと 30 分を超える）。
WORKDIR /out/ubuntu
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources \
    && grep -q '^Types: deb deb-src$' /etc/apt/sources.list.d/ubuntu.sources \
    && snap="$(cat /ubuntu-snapshot.txt)" \
    && apt-get update --snapshot "$snap" \
    && : > /failed.txt \
    && sed 's/ /=/' /ubuntu-source-packages.txt \
       | xargs -P 8 -n 1 sh -c 'apt-get source --snapshot "$0" --download-only "$1" >/dev/null 2>&1 \
                               || echo "$1" >> /failed.txt' "$snap" \
    && failed="$(sort /failed.txt | tr '\n' ' ')" \
    && if [ -n "$failed" ]; then \
         echo "snapshot $snap で取れなかったものをライブの索引で取り直す:$failed"; \
         apt-get update; retry="$failed"; failed=""; \
         for pv in $retry; do \
           apt-get source --download-only "$pv" >/dev/null 2>&1 || failed="$failed $pv"; \
         done; \
       fi \
    && if [ -n "$failed" ]; then echo "ソースを取得できなかったパッケージ:$failed"; exit 1; fi \
    && ls | wc -l

FROM scratch AS sources
COPY --from=core-build /src/sources/ /
COPY --from=core-build /src/online/engine/external/tarballs/ /core-external/
COPY --from=ubuntu-sources /out/ubuntu/ /ubuntu/
COPY --from=runtime /usr/share/doc/shiki-office/ /doc/

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

# ---- Stage 1: Collabora Office core（LOKit）のビルド ----
# core は LibreOffice の gerrit が正（GitHub の CollaboraOnline org に core は無い）。
FROM ubuntu:24.04 AS core-build
ARG CORE_REPO=https://git.libreoffice.org/core
ARG CORE_REF=cp-25.04.9-4
ARG CORE_COMMIT=bbf180856f1919e69a04fdf6175ff4f11f52e1db
ENV DEBIAN_FRONTEND=noninteractive
# **fakeroot / unzip / gettext を足している。** いずれも configure の検査に
# 必要で、欠けると数秒で落ちる（2026-09 以降この workflow が全失敗していた）。
# 実測で 1 つずつ潰した順:
#   configure: error: --with-package-format='deb rpm' requires fakeroot.
#   configure: error: unzip is required
#   configure: error: msgfmt not found. Install GNU gettext, ...
RUN apt-get update && apt-get install -y --no-install-recommends \
    git build-essential autoconf automake libtool pkg-config python3 python3-dev \
    zip unzip ccache flex bison gperf nasm libx11-dev libxml2-dev libxml2-utils libxslt1-dev \
    libcups2-dev libfontconfig1-dev libxrandr-dev libgl-dev libglu1-mesa-dev \
    libkrb5-dev libnss3-dev libpam0g-dev libssl-dev libcairo2-dev \
    libavahi-client-dev libdbus-glib-1-dev ca-certificates curl fakeroot gettext \
    && rm -rf /var/lib/apt/lists/*
# **core は root でビルドできない。** LibreOffice の Makefile に
#   check-if-root: Building LibreOffice as root is a very bad idea, use a regular user.
# という明示の拒否があり、make が即 Error 1 になる（実測）。
# 書き込む先（/src と /opt/collaboraoffice）を先に用意して所有者を移し、
# 以降を非 root で走らせる。
RUN useradd -m build \
    && mkdir -p /src /opt/collaboraoffice \
    && chown -R build:build /src /opt/collaboraoffice
USER build
WORKDIR /src
RUN git clone --depth 1 --branch "$CORE_REF" "$CORE_REPO" core \
    && cd core \
    && test "$(git rev-parse HEAD)" = "$CORE_COMMIT" \
    && git rev-parse HEAD > /src/CORE_COMMIT && cat /src/CORE_COMMIT
# LOKit 向け最小構成（GUI 依存を切る）。--with-distro が依存/機能セットを固定する。
#
# **configure と make をレイヤで分ける。** 一体にすると、数時間走った末に
# 落ちたのか configure の数秒で落ちたのかがログの末尾からは読めない
# （実際、fakeroot 欠けで 5 分で落ちていたのを「タイムアウトだろう」と
#  誤って見積もっていた）。分けておけばどちらかが一目で分かる。
#
# **--with-package-format を上書きする。** CPLinux-LOKit の既定は 'deb rpm' だが、
# ここでは `make cmd cmd='ooinstall ...'` しか使わず deb/rpm は作らない。
# 既定のままだと rpm コマンドまで要求され（configure: error: not found）、
# 使わない経路のためにビルド依存が増え続ける。archive なら tar/gzip で足りる。
#
# **Shiki Office としてブランドする（#549）。** CPLinux-LOKit は vendor=Collabora と
# Collabora のロゴ（brand_cp）を設定するが、Collabora の商標ポリシーは自前ビルドの配布で
# Collabora の名称・ロゴを外すことを求める。distro 設定より後ろの引数が勝つので
# （--with-package-format と同じ）、ここで上書きする。
#   --with-vendor: versionrc の Vendor と About の「supplied by」に出る
#   --with-branding: 7 ファイルすべてを用意する。欠けた分は既定（LibreOffice のロゴ）で埋まる
#
# **--with-product-name は渡さない。** Collabora 版の core はインストーラの設定
# （instsetoo_native/util/openoffice.lst.in）に製品の節 `CollaboraOffice` と
# PRODUCTNAME "Collabora Office" を直書きしている。製品名を変えると ooinstall が
# `-p ShikiOffice`（空白を除いた製品名）を渡し、その節が無いので 3 時間の make の後に落ちる。
# 変えるにはソースの改変が要るので、エンジンの内部名は "Collabora Office" のまま残す（human 判断）。
# 利用者に見える製品名は online 側（--with-app-name と coolwsd.xml）で Shiki Office にしており、
# ダイアログのタイトル中の "Collabora Office" も online が brandProductName に置き換える
# （browser/src/control/Control.LokDialog.js）。残るのは About の「powered by」に並ぶ
# エンジンの版の表記（出所の事実の記載）だけ。
COPY --chown=build:build branding/core /src/branding
RUN cd core && ./autogen.sh --with-distro=CPLinux-LOKit --disable-symbols \
    --with-package-format=archive \
    --with-vendor="Imanect Labs" --with-branding=/src/branding
RUN cd core && make -j"$(nproc)"
#
# **ooinstall は make とは別レイヤにする。** ここは約 3 時間の make が終わった
# 後にしか到達しないので、同じレイヤだと 1 回失敗するたびに 3 時間やり直しになる
# （実際に 2 回やり直した）。分けておけば make の結果がキャッシュに残る。
#
# **WITH_LANG_LIST を明示的に渡す。** ooinstall は $ENV{WITH_LANG_LIST} を
# `-l` の値に使うが、この変数は config_host_lang.mk に export 無しで定義され、
# Makefile.in は config_host.mk しか include しない。つまり環境には出てこない。
# 空のまま渡すと `-l -p CollaboraOffice` となって `-l` が `-p` を食い、
#   ERROR: unknown parameter: CollaboraOffice
# で落ちる（upstream の定義を追って切り分けた）。
#
# **ooinstall はパスつきで呼ぶ。** `bin/` は PATH に無いので裸の `ooinstall` は
#   /bin/sh: 1: ooinstall: not found   make: *** [Makefile:261: cmd] Error 127
# になる。**2 時間 48 分の make が完走した直後にこれで落ちた。**
#
# **LOKit のヘッダは ooinstall が入れない**（-simple インストールは実行時の
# program/ と share/ だけ）。coolwsd 側の configure は
#   AC_CHECK_HEADERS([LibreOfficeKit/LibreOfficeKit.h], ...,
#     [AC_MSG_ERROR([header LibreOfficeKit/LibreOfficeKit.h not found ...])])
# で止まるので、ソースツリーから持ち出して次のステージへ渡す。
RUN cd core \
    && LANGS="$(sed -n 's/^WITH_LANG_LIST=//p' config_host_lang.mk)" \
    && if [ -z "$LANGS" ]; then echo "WITH_LANG_LIST が取れない" >&2; exit 1; fi \
    && echo "languages: $LANGS" \
    && WITH_LANG_LIST="$LANGS" make cmd cmd='./solenv/bin/ooinstall /opt/collaboraoffice' \
    && cp -a include /opt/collaboraoffice/include \
    && test -f /opt/collaboraoffice/program/versionrc \
    && grep -q '^Vendor=Imanect Labs' /opt/collaboraoffice/program/versionrc \
    && rm -f /opt/collaboraoffice/EULA.odt /opt/collaboraoffice/EULA_en-US.rtf \
    && test -f /opt/collaboraoffice/LICENSE.html \
    && (cd external/tarballs && find . -maxdepth 1 -type f ! -name '.*' -printf '%f\n' \
        | sort | xargs -r sha256sum) > /src/CORE_EXTERNAL_TARBALLS.sha256 \
    && test -s /src/CORE_EXTERNAL_TARBALLS.sha256
# 上の RUN の後半（#549）:
# - **EULA は Collabora Office の有償サブスクリプション契約書**で、MPL のライセンス表示
#   （LICENSE / NOTICE。§3.4 で消してはならないもの）ではない。自前ビルドの配布物に
#   残すと契約関係があるように読めるので除く。後のステージで消すと下のレイヤに残るため、ここで消す。
# - ベンダー（versionrc の Vendor）が入ったことを確かめる。上流の既定は "Collabora"（実測）。
# - core が make 中に取得した外部ライブラリの tarball を sha256 付きで記録する。
#   ランタイムの告知（NOTICE）と、ソース保全イメージ（sources ステージ）の両方が使う。
#
# **ソース保全用に core のソース一式を固める（#549）。** submodule（translations /
# dictionaries / helpcontent2）は make の中で取得されるので、make の後に作る。
# git の追跡対象だけを入れ、workdir/ や instdir/ の成果物は混ぜない。
# --transform の flags=r: 既定（rsh）だとシンボリックリンクのリンク先にも接頭辞が付き、
# 展開すると core-<ref>/core-<ref>/... を指す壊れたリンクになる（.vsconfig 等）。
RUN mkdir -p /src/sources \
    && cd core \
    && git ls-files --recurse-submodules -z \
       | tar --null -T - --transform "flags=r;s,^,core-$CORE_REF/," -czf /src/sources/core-$CORE_REF.tar.gz \
    && tar tzf /src/sources/core-$CORE_REF.tar.gz "core-$CORE_REF/configure.ac" >/dev/null

# ---- Stage 1-b: Poco（coolwsd の依存）のビルド ----
#
# **distro から取れない。** coolwsd の configure は
#   #if POCO_VERSION < 0x010C0000 → "The POCO version is too old, 1.12.0 or newer required"
# を要求するが、Ubuntu 24.04 の libpoco-dev は **1.11.0**（実測）。
# 1.13.0 は configure が「known buggy poco 1.13.0」と判定する版なので避ける。
#
# バージョンは office-manifest.env が正。tarball なので sha256 で直接固定する。
FROM ubuntu:24.04 AS poco-build
ARG POCO_URL=https://github.com/pocoproject/poco/archive/refs/tags/poco-1.14.2-release.tar.gz
ARG POCO_SHA256=47394ea7ddb7b0a40e1a5be896f8f5dc77cfdc4f561d2e7131ecf582df5a0c3a
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake ca-certificates curl libssl-dev zlib1g-dev libpcre2-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl -fsSL -o poco.tar.gz "$POCO_URL" \
    && echo "$POCO_SHA256  poco.tar.gz" | sha256sum -c - \
    && tar xf poco.tar.gz && mv poco-* poco \
    && printf '%s\n%s\n' "$POCO_URL" "$POCO_SHA256" > /src/POCO_SOURCE
# coolwsd が使うのは Foundation / Net / NetSSL / Crypto / Util / XML / JSON。
# それ以外（Data・MongoDB・Redis・Zip 等）は切って時間とサイズを節約する。
RUN cmake -S poco -B build \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX=/opt/poco \
      -DBUILD_SHARED_LIBS=ON \
      -DENABLE_NETSSL=ON -DENABLE_CRYPTO=ON -DENABLE_UTIL=ON \
      -DENABLE_XML=ON -DENABLE_JSON=ON \
      -DENABLE_DATA=OFF -DENABLE_DATA_SQLITE=OFF -DENABLE_MONGODB=OFF \
      -DENABLE_REDIS=OFF -DENABLE_ZIP=OFF -DENABLE_PAGECOMPILER=OFF \
      -DENABLE_PAGECOMPILER_FILE2PAGE=OFF -DENABLE_ENCODINGS=OFF \
      -DENABLE_TESTS=OFF -DENABLE_SAMPLES=OFF \
    && cmake --build build -j"$(nproc)" \
    && cmake --install build \
    && test -f /opt/poco/include/Poco/Version.h

# ---- Stage 2: coolwsd（Collabora Online 本体）のビルド ----
FROM ubuntu:24.04 AS online-build
ARG ONLINE_REPO=https://github.com/CollaboraOnline/online.git
ARG ONLINE_REF=cp-25.04.9-5
ARG ONLINE_COMMIT=53f91c25641febb11d77702380530775a923890e
ENV DEBIAN_FRONTEND=noninteractive
# **zlib と cppunit も要る。** coolwsd の configure.ac は Linux では
#   PKG_CHECK_MODULES([ZLIB], [zlib])
#   PKG_CHECK_MODULES([CPPUNIT], [cppunit])
# を無条件で通すので、欠けると configure が止まる（upstream の configure.ac で確認）。
# **libpoco-dev は入れない**（1.11.0 で古い。poco-build ステージの 1.14.2 を使う）。
#
# python3-lxml / python3-polib: configure が `import` して確かめる。
# cpio / file / fontconfig とフォント: **make の中で coolwsd-systemplate-setup が
# 走る**（jail の雛形を作る）。欠けると
#   ./coolwsd-systemplate-setup: line 88: cpio: command not found
#   cp: cannot stat '/usr//share/fonts': No such file or directory
#   make[2]: *** [Makefile:4370: all-local] Error 127
# で落ちる。同スクリプトが使う外部コマンドは
#   cp cpio dirname file find grep ldd ln mkdir rm sed sort touch xargs
# で、このうち base イメージに無いのが cpio と file（実測で切り分け）。
#
# rsync: browser/Makefile.am の install-data-hook が使う。
#   /bin/bash: line 1: rsync: command not found
#   make[5]: *** [Makefile:1784: install-data-hook] Error 127
# Makefile.am を走査して外部コマンドを突き合わせ、足りないのはこれだけと確認した。
RUN apt-get update && apt-get install -y --no-install-recommends \
    git build-essential autoconf automake libtool pkg-config \
    python3 python3-polib python3-lxml \
    libcap-dev libzstd-dev libpam0g-dev libssl-dev libpng-dev \
    zlib1g-dev libcppunit-dev libpcre2-dev \
    cpio file fontconfig fonts-dejavu-core rsync \
    nodejs npm ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
# **ローダにも登録する。** make の途中で coolforkit-ns を実行するので、
# ヘッダとリンク時だけでなく実行時にも Poco が見えている必要がある
#   ./coolforkit-ns: error while loading shared libraries: libPocoXML.so.112
COPY --from=poco-build /opt/poco /opt/poco
RUN echo /opt/poco/lib > /etc/ld.so.conf.d/poco.conf && ldconfig \
    && ldconfig -p | grep -q libPocoXML
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
WORKDIR /src
RUN git clone --depth 1 --branch "$ONLINE_REF" "$ONLINE_REPO" online \
    && cd online \
    && test "$(git rev-parse HEAD)" = "$ONLINE_COMMIT" \
    && git rev-parse HEAD | tee /ONLINE_COMMIT \
    && mkdir -p /src/sources \
    && git archive --format=tar.gz --prefix="online-$ONLINE_REF/" HEAD \
       > /src/sources/online-$ONLINE_REF.tar.gz
# setcap は configure 時ではなくランタイムステージで明示的に付与する（DESTDIR install と
# レイヤ分離のため）。
# configure と make を分ける（core 側と同じ理由）。
#
# ブランド（#549）:
#   --with-app-name: 既定は "Collabora Online Development Edition"。WOPI の
#     /hosting/capabilities の productName 等に出る
#   --without-help-url: 既定は help.collaboraoffice.com。空にするとヘルプボタンが消える
#     （coolwsd.xml の help_url の既定値もこれになる）
#   --with-vendor: About の「Copyright © <年>, <vendor>.」にそのまま出る（Control.AboutDialog.ts）。
#     **著作権表示なので Imanect Labs にしない**（無改変の上流コードの著作権者は上流の貢献者）。
#     未指定だとビルドしたユーザー名（root）が入る。
RUN cd online && ./autogen.sh \
    && ./configure --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
         --with-lokit-path=/opt/collaboraoffice/include \
         --with-lo-path=/opt/collaboraoffice \
         --with-poco-includes=/opt/poco/include \
         --with-poco-libs=/opt/poco/lib \
         --with-app-name="Shiki Office" \
         --with-vendor="the Collabora Online and LibreOffice contributors" \
         --without-help-url \
         --disable-setcap --enable-silent-rules
RUN cd online \
    && make -j"$(nproc)" \
    && make install DESTDIR=/inst

# ---- Stage 3: ランタイム ----
# **名前付きステージなので、ビルドでは `--target runtime` を指定する。** 後ろに
# ソース保全用のステージ（sources）が続くため、無指定だとそちらが建つ。
FROM ubuntu:24.04 AS runtime
ARG CORE_REPO=https://git.libreoffice.org/core
ARG CORE_REF=cp-25.04.9-4
ARG CORE_COMMIT=bbf180856f1919e69a04fdf6175ff4f11f52e1db
ARG ONLINE_REPO=https://github.com/CollaboraOnline/online.git
ARG ONLINE_REF=cp-25.04.9-5
ARG ONLINE_COMMIT=53f91c25641febb11d77702380530775a923890e
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
# **apt-get update の時刻を記録する（#549）。** このレイヤはビルドキャッシュから
# 再利用されるので、後から deb-src を引くと版がずれる。sources ステージはこの時刻の
# Ubuntu スナップショット（snapshot.ubuntu.com）からソースを取る。記録とインストールが
# 同じレイヤにあるので、キャッシュに当たっても対応は崩れない。
#
# libutf8proc3: Poco が同梱している utf8proc の著作権表示とライセンス本文
# （/usr/share/doc/libutf8proc3/copyright）を NOTICE から参照するため。
# PCRE2 / zlib / expat の分は既に入っているパッケージのものを参照する。
RUN mkdir -p /usr/share/doc/shiki-office \
    && apt-get update \
    && date -u +%Y%m%dT%H%M%SZ > /usr/share/doc/shiki-office/ubuntu-snapshot.txt \
    && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends \
    libssl-dev libpng-dev libzstd-dev libpam0g-dev libcap2-bin libpcre2-8-0 \
    libx11-dev libxml2-dev libxslt1-dev libcups2-dev libfontconfig1-dev libxrandr-dev \
    libgl-dev libglu1-mesa-dev libkrb5-dev libnss3-dev libcairo2-dev \
    libavahi-client-dev libdbus-glib-1-dev \
    cpio tzdata fontconfig libutf8proc3 \
    fonts-noto-cjk fonts-noto-cjk-extra ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# **このイメージに入っている Ubuntu パッケージのソースパッケージと版を記録する（#549）。**
# GPL / LGPL のパッケージ（bash・coreutils・libc 等）を含むイメージを配布すると、
# そのソースの提供義務を負う。sources ステージはこの一覧どおりにソースを取得する。
# apt はこの上の 1 回しか走らせないこと（後から足すと一覧から漏れる）。
RUN dpkg-query -W -f='${source:Package} ${source:Version}\n' | sort -u \
       > /usr/share/doc/shiki-office/ubuntu-source-packages.txt \
    && test -s /usr/share/doc/shiki-office/ubuntu-source-packages.txt
# **ビルドに使ったのと同じ Poco を載せる。** distro の 1.11.0 を入れると
# coolwsd がリンクしたシンボルと食い違う。
COPY --from=poco-build /opt/poco/lib /opt/poco/lib
RUN echo /opt/poco/lib > /etc/ld.so.conf.d/poco.conf && ldconfig
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
COPY --from=online-build /inst/ /
COPY --from=core-build /src/CORE_COMMIT /etc/collabora/CORE_COMMIT
COPY --from=online-build /ONLINE_COMMIT /etc/collabora/ONLINE_COMMIT
COPY --from=core-build /src/CORE_EXTERNAL_TARBALLS.sha256 /usr/share/doc/shiki-office/core-external-tarballs.sha256
# **第三者ソフトウェアの告知（#549・MPL-2.0 §3.2(a)）。** 受け取った人がソースの入手先を
# 分かるようにする。コミット SHA だけでは足りないので、リポジトリ・タグ・SHA を並べる。
COPY --from=poco-build /src/POCO_SOURCE /usr/share/doc/shiki-office/POCO_SOURCE
COPY NOTICE.in /usr/share/doc/shiki-office/NOTICE.in
RUN cd /usr/share/doc/shiki-office \
    && POCO_URL="$(sed -n 1p POCO_SOURCE)" && POCO_SHA256="$(sed -n 2p POCO_SOURCE)" && rm POCO_SOURCE \
    && for d in libpcre2-8-0 zlib1g libexpat1 libutf8proc3; do \
         test -s "/usr/share/doc/$d/copyright" || { echo "/usr/share/doc/$d/copyright が無い"; exit 1; }; \
       done \
    && sed -e "s|@CORE_REPO@|$CORE_REPO|g" -e "s|@CORE_REF@|$CORE_REF|g" \
           -e "s|@CORE_COMMIT@|$CORE_COMMIT|g" -e "s|@ONLINE_REPO@|$ONLINE_REPO|g" \
           -e "s|@ONLINE_REF@|$ONLINE_REF|g" -e "s|@ONLINE_COMMIT@|$ONLINE_COMMIT|g" \
           -e "s|@POCO_URL@|$POCO_URL|g" -e "s|@POCO_SHA256@|$POCO_SHA256|g" \
           NOTICE.in > NOTICE \
    && rm NOTICE.in \
    && if grep -n '@[A-Z_]*@' NOTICE; then echo "NOTICE に未置換のプレースホルダが残っている"; exit 1; fi \
    && test "$(cat /etc/collabora/CORE_COMMIT)" = "$CORE_COMMIT" \
    && test "$(cat /etc/collabora/ONLINE_COMMIT)" = "$ONLINE_COMMIT"
# **ブランドの上書き（#549）。** ソースは無改変のまま、未ブランドのビルドが読み込む
# branding.css / branding.js を置く（中身の説明は各ファイルの冒頭）。
COPY branding/online/branding.css branding/online/branding.js /usr/share/coolwsd/browser/dist/
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
# 非 root 実行（cool ユーザー）＋ systemplate/jail の初期化＋forkit へのファイル capability 付与
# （jail の chroot に必要。Docker 既定 cap 集合（CHOWN/FOWNER/MKNOD/SYS_CHROOT）内で動く。
#  compose 側で no-new-privileges を付けないこと — file caps の獲得が塞がれ起動不能になる）。
# systemplate-setup は失敗しても exit 0 なので、雛形の中身をここで確かめて止める。
RUN useradd -m -d /opt/cool cool \
    && mkdir -p /etc/coolwsd /var/cache/coolwsd /opt/cool/child-roots \
    && coolwsd-systemplate-setup /opt/cool/systemplate /opt/collaboraoffice \
    && for f in lib64/ld-linux-x86-64.so.2 etc/ld.so.cache etc/localtime etc/fonts usr/share/fonts; do \
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
# 取れなかったものは名前を全部出して止める。
WORKDIR /out/ubuntu
RUN sed -i 's/^Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/ubuntu.sources \
    && grep -q '^Types: deb deb-src$' /etc/apt/sources.list.d/ubuntu.sources \
    && snap="$(cat /ubuntu-snapshot.txt)" \
    && apt-get update --snapshot "$snap" \
    && failed="" \
    && while read -r pkg ver; do \
         apt-get source --snapshot "$snap" --download-only "$pkg=$ver" >/dev/null 2>&1 \
           || failed="$failed $pkg=$ver"; \
       done < /ubuntu-source-packages.txt \
    && if [ -n "$failed" ]; then echo "ソースを取得できなかったパッケージ（snapshot $snap）:$failed"; exit 1; fi \
    && ls | wc -l

FROM scratch AS sources
COPY --from=core-build /src/sources/ /
COPY --from=core-build /src/core/external/tarballs/ /core-external/
COPY --from=online-build /src/sources/ /
COPY --from=poco-build /src/poco.tar.gz /poco.tar.gz
COPY --from=ubuntu-sources /out/ubuntu/ /ubuntu/
COPY --from=runtime /usr/share/doc/shiki-office/ /doc/

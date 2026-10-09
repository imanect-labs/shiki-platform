# Collabora Online（coolwsd＋Collabora Office core）の**ソース自前ビルド**（Task 11.5・PIT-43）。
#
# なぜソースビルドか（human 確定判断・docs/design.md §4.8）:
#   公式 CODE バイナリには 10 文書/20 接続の制限が焼き込まれ、本番利用は商用契約が前提になる。
#   ソースは MPLv2 であり、自前ビルドなら制限・契約なしでオンプレ同梱できる。
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
RUN cd core && ./autogen.sh --with-distro=CPLinux-LOKit --disable-symbols \
    --with-package-format=archive
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
RUN cd core \
    && LANGS="$(sed -n 's/^WITH_LANG_LIST=//p' config_host_lang.mk)" \
    && if [ -z "$LANGS" ]; then echo "WITH_LANG_LIST が取れない" >&2; exit 1; fi \
    && echo "languages: $LANGS" \
    && WITH_LANG_LIST="$LANGS" make cmd cmd='./solenv/bin/ooinstall /opt/collaboraoffice' \
    && cp -a include /opt/collaboraoffice/include \
    && test -f /opt/collaboraoffice/program/versionrc

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
    && tar xf poco.tar.gz && mv poco-* poco
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
    && git rev-parse HEAD | tee /ONLINE_COMMIT
# setcap は configure 時ではなくランタイムステージで明示的に付与する（DESTDIR install と
# レイヤ分離のため）。
# configure と make を分ける（core 側と同じ理由）。
RUN cd online && ./autogen.sh \
    && ./configure --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
         --with-lokit-path=/opt/collaboraoffice/include \
         --with-lo-path=/opt/collaboraoffice \
         --with-poco-includes=/opt/poco/include \
         --with-poco-libs=/opt/poco/lib \
         --disable-setcap --enable-silent-rules
RUN cd online \
    && make -j"$(nproc)" \
    && make install DESTDIR=/inst

# ---- Stage 3: ランタイム ----
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
# ランタイム依存は dev パッケージで引き込む（noble の t64 リネーム等、実ライブラリの
# パッケージ名差異に依存しないため。ヘッダ分のサイズ増は堅牢性とのトレード）。
# 日本語フォント（Noto CJK）はオンプレ・エアギャップで文書の見た目を保つため同梱する。
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl-dev libpng-dev libzstd-dev libpam0g-dev libcap2-bin libpcre2-8-0 \
    fonts-noto-cjk fonts-noto-cjk-extra ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# **ビルドに使ったのと同じ Poco を載せる。** distro の 1.11.0 を入れると
# coolwsd がリンクしたシンボルと食い違う。
COPY --from=poco-build /opt/poco/lib /opt/poco/lib
RUN echo /opt/poco/lib > /etc/ld.so.conf.d/poco.conf && ldconfig
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
COPY --from=online-build /inst/ /
COPY --from=core-build /src/CORE_COMMIT /etc/collabora/CORE_COMMIT
COPY --from=online-build /ONLINE_COMMIT /etc/collabora/ONLINE_COMMIT
# 非 root 実行（cool ユーザー）＋ systemplate/jail の初期化＋forkit へのファイル capability 付与
# （jail の chroot に必要。Docker 既定 cap 集合（CHOWN/FOWNER/MKNOD/SYS_CHROOT）内で動く。
#  compose 側で no-new-privileges を付けないこと — file caps の獲得が塞がれ起動不能になる）。
RUN useradd -m -d /opt/cool cool \
    && mkdir -p /etc/coolwsd /var/cache/coolwsd /opt/cool/child-roots \
    && coolwsd-systemplate-setup /opt/cool/systemplate /opt/collaboraoffice \
    && for b in /usr/bin/coolforkit /usr/bin/coolforkit-caps /usr/bin/coolmount; do \
         [ -x "$b" ] && setcap cap_fowner,cap_chown,cap_mknod,cap_sys_chroot=ep "$b" || true; \
       done \
    && chown -R cool:cool /opt/cool /etc/coolwsd /var/cache/coolwsd
USER cool
EXPOSE 9980
# WOPI ホスト許可・server_name 等の環境依存値は compose/K8s 側の command 追記で注入する。
# TLS はリバースプロキシ終端が前提（コンテナ内は平文 9980）。
CMD ["coolwsd", "--o:sys_template_path=/opt/cool/systemplate", \
     "--o:child_root_path=/opt/cool/child-roots", \
     "--o:lo_template_path=/opt/collaboraoffice", \
     "--o:ssl.enable=false", "--o:ssl.termination=false"]

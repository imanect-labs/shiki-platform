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
RUN apt-get update && apt-get install -y --no-install-recommends \
    git build-essential autoconf automake libtool pkg-config python3 python3-dev \
    zip ccache flex bison gperf nasm libx11-dev libxml2-dev libxml2-utils libxslt1-dev \
    libcups2-dev libfontconfig1-dev libxrandr-dev libgl-dev libglu1-mesa-dev \
    libkrb5-dev libnss3-dev libpam0g-dev libssl-dev libcairo2-dev \
    libavahi-client-dev libdbus-glib-1-dev ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN git clone --depth 1 --branch "$CORE_REF" "$CORE_REPO" core \
    && cd core \
    && test "$(git rev-parse HEAD)" = "$CORE_COMMIT" \
    && git rev-parse HEAD | tee /CORE_COMMIT
# LOKit 向け最小構成（GUI 依存を切る）。--with-distro が依存/機能セットを固定する。
RUN cd core \
    && ./autogen.sh --with-distro=CPLinux-LOKit --disable-symbols \
    && make -j"$(nproc)" \
    && mkdir -p /opt/collaboraoffice \
    && make cmd cmd='ooinstall /opt/collaboraoffice'

# ---- Stage 2: coolwsd（Collabora Online 本体）のビルド ----
FROM ubuntu:24.04 AS online-build
ARG ONLINE_REPO=https://github.com/CollaboraOnline/online.git
ARG ONLINE_REF=cp-25.04.9-5
ARG ONLINE_COMMIT=53f91c25641febb11d77702380530775a923890e
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git build-essential autoconf automake libtool pkg-config python3 python3-polib \
    libpoco-dev libcap-dev libzstd-dev libpam0g-dev libssl-dev libpng-dev \
    nodejs npm ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
WORKDIR /src
RUN git clone --depth 1 --branch "$ONLINE_REF" "$ONLINE_REPO" online \
    && cd online \
    && test "$(git rev-parse HEAD)" = "$ONLINE_COMMIT" \
    && git rev-parse HEAD | tee /ONLINE_COMMIT
# setcap は configure 時ではなくランタイムステージで明示的に付与する（DESTDIR install と
# レイヤ分離のため）。
RUN cd online && ./autogen.sh \
    && ./configure --prefix=/usr --sysconfdir=/etc --localstatedir=/var \
         --with-lokit-path=/opt/collaboraoffice/include \
         --with-lo-path=/opt/collaboraoffice \
         --disable-setcap --enable-silent-rules \
    && make -j"$(nproc)" \
    && make install DESTDIR=/inst

# ---- Stage 3: ランタイム ----
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
# ランタイム依存は dev パッケージで引き込む（noble の t64 リネーム等、実ライブラリの
# パッケージ名差異に依存しないため。ヘッダ分のサイズ増は堅牢性とのトレード）。
# 日本語フォント（Noto CJK）はオンプレ・エアギャップで文書の見た目を保つため同梱する。
RUN apt-get update && apt-get install -y --no-install-recommends \
    libpoco-dev libssl-dev libpng-dev libzstd-dev libpam0g-dev libcap2-bin \
    fonts-noto-cjk fonts-noto-cjk-extra ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=core-build /opt/collaboraoffice /opt/collaboraoffice
COPY --from=online-build /inst/ /
COPY --from=core-build /CORE_COMMIT /etc/collabora/CORE_COMMIT
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

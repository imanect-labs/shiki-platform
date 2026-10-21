/*
 * Shiki Office のブランド上書き（#549）。branding.css と対で置く。
 * 製品名・リンク・ロゴは coolwsd.xml の user_interface.* で設定している。
 *
 * About の著作権表示（Copyright © <年>, <vendor>.）の vendor を正す。
 * 上流の cool.html.m4 は `data-vendor = VENDOR` と引用符なしで埋め込むため、空白を含む
 * --with-vendor は最初の単語で切れ、「Copyright © 2026, the.」になる。
 * このファイルは global.js（window.vendor を data-vendor から読む）の後に読み込まれ、
 * About は開くたびに window.vendor を読むので、ここで入れ直せば効く。
 * 値は Dockerfile の online の --with-vendor と同じにする（ビルド時に一致を検査している）。
 */
window.vendor = 'the Collabora Online and LibreOffice contributors';

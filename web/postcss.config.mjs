// Tailwind v4 は PostCSS プラグイン経由で読み込む（v3 の `tailwindcss` エントリは使わない）。
// 設定は CSS 側（globals.css の @theme / @custom-variant）に集約し、tailwind.config は持たない。
const config = {
  plugins: {
    "@tailwindcss/postcss": {},
  },
};

export default config;

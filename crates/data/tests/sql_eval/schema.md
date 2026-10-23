## テーブル

- `customers`（顧客）
  - `name`: テキスト（顧客名）
  - `city`: 選択肢（東京 / 大阪 / 福岡 / 名古屋 / 札幌）
  - `tier`: 選択肢（会員ランク: gold / silver / bronze）
  - `signup_date`: 日付（登録日・'YYYY-MM-DD'）
  - `orders`: リンク（この顧客を参照している `orders` の一覧。`orders.customer` の逆向き）
- `products`（商品）
  - `name`: テキスト（商品名）
  - `category`: 選択肢（文具 / 家電 / 家具 / 食品）
  - `price`: 数値（価格・円）
  - `lines`: リンク（この商品を参照している `order_lines` の一覧。`order_lines.product` の逆向き）
- `orders`（注文）
  - `order_no`: テキスト（注文番号・例 'O-0001'）
  - `customer`: リンク → `customers`（多対一・注文した顧客）
  - `status`: 選択肢（受付 / 出荷済 / キャンセル）
  - `amount`: 数値（金額・円）
  - `ordered_on`: 日付（受付日・'YYYY-MM-DD'）
  - `channel`: 選択肢（web / 店舗 / 電話）
  - `lines`: リンク（この注文の `order_lines` の一覧。`order_lines.order_ref` の逆向き）
- `order_lines`（注文明細）
  - `order_ref`: リンク → `orders`（多対一・明細が属する注文）
  - `product`: リンク → `products`（多対一・明細の商品）
  - `qty`: 数値（数量）
  - `unit_price`: 数値（単価・円）

どのテーブルにも擬似列 `_id`（レコード ID）がある。リンク列の値は参照先の `_id`。

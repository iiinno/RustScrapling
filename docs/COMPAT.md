# COMPAT.md: 互換モードと既定モードの違い、本家との対応

## 1. 段階1の照合方法

- 互換モード（libxml2 解析＋自前 CSS/XPath）: oracle の正解データ `fixtures/stage1.json`（本家の parser 試験由来の3文書、CSS 59件・XPath 29件）と 100% 一致した。エラーになる式（`:hidden`、`[attr>=...]`、`4 ayo`、XPath の `count()` のようなスカラ結果）も一致に含む。
- 既定モード（html5ever 解析＋同じ選択エンジン）: 同じ88件を実行し、30件で結果の記述が異なった。内訳は以下の2分類のみで、**選択される要素の集合（タグ・属性・内容で識別）が変わる差は無かった**。

## 2. 既定モードと互換モードの違い（分類済み）

### クラスT: table の `tbody` 補完

| 違い | 互換モード（libxml2） | 既定モード（html5ever） |
|---|---|---|
| `tbody` の補完 | `<table><tr>` に `tbody` を挿入しない | HTML5 仕様どおり `tbody` を挿入する |

影響: 要素パス（`/html/body/div/table/tr[1]` 対 `/html/body/div/table/tbody/tr[1]`）と、パスで要素を識別する adaptive の指紋。選択結果の要素そのものは変わらない。根拠: 2026-11-02 実測。本家 Scrapling（lxml）でも `css('tbody')` は0件、`css('tr')` は1件。段階1の差分30件のうち5件がこのクラス（complex 文書の table 系）。

### クラスW: 空白のみのテキストノードの有無

libxml2 の HTML 解析器は、位置によって空白のみのテキストノードの一部を作らない。html5ever（とブラウザ）は全て保持する。具体的に観測した欠落位置:

- 要素の最初の子になる空白テキスト（例: `<article>` 直後、子要素の前の空白）。互換モードでは `article` の `text` が null になり、`text_content` が子要素の文字から始まる。
- 親要素の終了タグ直前の空白テキストの一部（例: `<table>` の後、`</div>` の前）。
- `body` の前後や文書末尾（`</script>` の後など）の空白。

影響: `text`、`tail`、`text_content` の文字列と、`::text` の結果件数（complex 文書で 4件対5件）。空白以外の文字を含むテキストノードには差が無い。段階1の差分30件のうち25件がこのクラス（general 文書の article・section・main・`*` など、空白を含む記述子を持つ要素）。

### 差が無かったもの

- 属性の名前・値・順序、要素の親子関係（tbody を除く）、テキスト検索や数値述語を含む選択の成否は、88件すべてで両モード一致した。

## 3. CLI コマンドと本家の対応

Plan 7.1節の表が正本。実装が進んだコマンドからここへ実装状況を追記する。

## 4. find_by_regex の正規表現の差

`find_by_regex` は Rust の `regex` クレートで実装している。Python の `re` との主な差:

- 先読み・後読み（`(?=...)`、`(?<=...)` 等）と後方参照（`\1`、名前付き後方参照）は使えない。該当パターンはコンパイルエラーになる（本家では動作する）。
- 名前付きグループは `(?<name>...)` と `(?P<name>...)` の両形式を受け付ける。
- フラグ記法（`(?i)` 等）、文字クラス、量指定子、アンカー等の通常の記法は共通。マッチ判定は部分マッチ（`re.search` 相当）で本家と一致させている。

照合範囲: `fixtures/stage2_find.json` の regex ケース全件が本家と一致。上記の未対応構文を使うパターンだけが差になる。

## 5. storage の element_data の型（TEXT / BLOB）

本家は orjson の bytes をそのまま SQLite へ書くため、`element_data` 列の実データは BLOB 型になる（列定義は TEXT でも SQLite の型親和性で値は BLOB のまま保持される）。

- RustScrapling の保存: 本家に合わせて JSON をバイト列で束縛し、BLOB として保存する。本家がそのまま読める（`oracle/verify_stage2.py` で JSON バイト同一を確認済み）。
- RustScrapling の読取: BLOB と TEXT の両方を受ける。初期実装は BLOB のみを読もうとして、TEXT で保存された（=かつての Rust 自身が書いた）DB の読み戻しに失敗した。CLI スモークで検出し、保存=BLOB・読取=両対応に統一した。回帰テスト: `storage::tests::rust_written_db_roundtrips`。

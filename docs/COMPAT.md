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

- `rustscrapling network search [--url-contains TEXT] [--limit 1..100]` は、起動中のdaemonが完了した取得のメタデータ（URL、method、status、tier）を、認証付きIPCを通じて検索する。最新128件の揮発性履歴で、URLのquery/fragment/認証情報とヘッダー・本文は返さない。**Python Scraplingの `session.network.search` が対象とするブラウザ内部通信までは取得しない**ため、現状は部分実装。
- `rustscrapling session open/close/list/save-state/load-state` はコマンドとして受理するが、現行daemonがブラウザセッションを保持しないため `error.kind=unsupported` で失敗する。CLIのプロセス終了後に有効なセッションIDを捏造しない。永続MCPサーバ内のブラウザセッションは別実装。

### MCPブラウザ通信と操作（Python v0.5との差、`1a74ab5`時点）

- **CLI daemonの `network search` とMCPの `browser_network_requests` は別のデータ源**。前者は上位Fetch完了のメタデータのみ、後者は同じMCP browser sessionのCDPサブリクエスト（最新128件・session-local単調ID・body合計1MiB）と、`browser_network_request(part="response_body")`によるXHR/Fetchテキスト本文。本家の上限（1000件・20MiB・1件1MiB）とは異なる。URLとJSON機密キーは保守的に伏せるが、アプリ固有情報の完全な秘匿は保証しない。
- `browser_actions`は本家の12種すべてを実装。selectorはPlaywright構文のうちCSS（open shadow rootを貫通、ただし境界をまたぐ子孫結合子は不可）、`xpath=`/`//`、`text=`（引用符で完全一致・無しで部分一致・`/re/`）、`role=`/`internal:role=`（accessible nameは簡易計算）、`internal:text=`/`internal:label=`/`internal:testid=`、`id=`、`data-testid=`、`nth=`、`visible=`、`>>`連結、`aria-ref=`。Playwright CSS拡張（`:has-text()`、`:nth-match()`等）、`frame_locator`相当、iframe内要素は未対応で、未知engineは`Unknown engine`エラー。
- browser_actionsのエラーは本家と同じ`Action N (type) failed: ...`文言を`error.message`に入れ、`error.kind`（`action_failed`/`timeout`/`invalid_arguments`）を付けたJSONで返す（本家はプレーンテキストのToolError）。timeoutは`Timeout {ms}ms exceeded; <最後の待機理由>`で、Playwrightのcall logは返さない。
- ARIA snapshotと`aria-ref=`、およびselector評価は、本家が実行時に使うpatchright-core 1.64.3の注入スクリプトをvendorし分離worldで実行するため本家と同一（fixture差分0）。ただしiframe内容のsnapshotへの統合と、iframe内refへの操作は未対応。
- MCPのコンテンツ出力（markdown/html/text）は本家`Convertor._extract_content`互換（body無害化＋markdownify移植）で、`content`配列末尾の空文字列も本家どおり。Rust独自の記事抽出（`rustscrapling-extract::extract`）はCLI等の追加機能として残し、MCP互換出力を上書きしない（Plan 5.9）。
- `tools/list`は本家live出力と完全一致。引数検証はJSON Schemaに対し厳格で、本家（Pydantic lax）が型変換で受理する値（例：`"5"`→整数）や未知の追加引数は`invalid_arguments`として拒否する。
- 入力はCDP `Input`ドメインの信頼イベント。要素判定は分離worldで実行し、`browser_actions`はmain worldの`Runtime.enable`を送らない。応答のないJavaScript dialogは自動でdismissする（Playwright既定と同じ）。

### MCPブラウザ取得オプション（後継セッション）

- 実装済み：`locale`（Emulation＋Accept-Language）、`timezone_id`、`useragent`（未指定のheadlessは`HeadlessChrome`→`Chrome`）、`cookies`（Playwright形式）、`extra_headers`（Refererはnavigation referrerへ移す）、`google_search`（既定で`https://www.google.com/`、`referrerPolicy=unsafeUrl`）、`blocked_domains`（サブドメイン含む）、`disable_resources`（Font/Image/Media/Ping/TextTrack/WebSocket/CSPViolationReport/Stylesheet）、`block_webrtc`/`allow_webgl`/`hide_canvas`（本家と同じChromeフラグ）、`pierce_shadow`（本家`SHADOW_SNAPSHOT_JS`）、`proxy`辞書の`server`、`network_idle`/`wait_selector`（one-shotも）、viewport 1920×1080。
- 未対応（typed error）：認証付きブラウザproxy（username/password）、空でない`additional_args`、`solve_cloudflare=true`。`real_chrome`は値に関わらずシステムChrome（`executable_path`/`SCRAPLING_EXECUTABLE_PATH`/既定パス）を使う。`load_dom=false`でもloadイベントまで待つ（より厳しい待機）。本家の常時広告ブロック（`block_ads=True`）とHTTPSエラー無視は未実装。

### MCP HTTP取得（`make_request` / `session_make_request`）

- 本家（curl_cffi `FetcherSession`）準拠：`retries`は初回を含む試行回数（None/1未満は1回）、通信エラー時のみ全メソッドで再試行し`retry_delay`秒待つ（HTTP 4xx/5xxでは再試行しない）。`follow_redirects`は`"safe"`（既定：リダイレクト先を名前解決し、loopback/private/link-local/CGNAT/ULA等なら`blocked`エラー）・`true`・`false`、`max_redirects`（-1は無制限だが安全上100で打ち切り）。`auth`/`proxy_auth`（Basic、不完全な辞書は本家と同じ文言で拒否）、`params`のリスト値は複数キー、`cookies`は既存`Cookie`ヘッダーへ連結、`json`優先・`data`辞書はform、GETではbody無視、`stealthy_headers`は未指定時にGoogle Referer、`verify=false`は明示時のみTLS検証無効化、`http3=true`はHTTP/3のみ（本家V3ONLY相当）、`impersonate`はプロファイル名/リスト（ランダム選択）/null。
- HTTP sessionはセッション共通のcookie jarを保持（Set-Cookieが次のリクエストに送られる）。リクエストごとの設定差に対応するため、session内でも要求ごとにクライアントを構築する（接続再利用は限定的、性能はG-05で計測予定）。
- 差分：impersonateの指紋はwreqのプロファイル（curl_cffiのものとは別実装）。`impersonate=null`かつ`stealthy_headers=true`で本家が生成するbrowserforgeヘッダーは未実装（Refererのみ付与）。クロスオリジンへのリダイレクトでは`Authorization`/`Cookie`を転送しない（安全側の差）。

### Spider rich Request（S-03部分、`7752546`統合）

- `SpiderRequest`はmethod/body/headers/priorityを持ち、URLだけでなくmethod・正規化header・bodyを含むdedupe keyで重複排除する（priorityはkeyに含めない）。旧`[url, depth]`形式checkpointは読み書き互換。
- `crawldir`使用時にbody/headerを含むRequestを保存するには`persist_sensitive_requests = true`の明示が必要（平文・所有者限定ファイル）。cookies/session/metadata/callback/errbackは未実装。

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

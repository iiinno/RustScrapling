# DECISIONS.md: 判断の記録

Plan.md の「判断済みの事項」は変えていない。ここには段階0の下調べ結果と、計画にない判断点への対処を記録する。

## D-001 crates.io への到達（2026-11-02、段階0）

- `https://index.crates.io/config.json` は 200。個別クレートのインデックス（`index.crates.io/se/rd/serde`）も 200。
- `https://static.crates.io/crates/serde/serde-1.0.219.crate` の実ファイル取得も 200（78,983 バイト、gzip として検証）。
- 先行実測の「crates.io から 403」は、`static.crates.io` ルートへの HEAD と `storage.googleapis.com` ルートへの HEAD に対する 403 であり、ルート一覧が禁止されているだけだった。実ファイルの取得は通る。
- `cargo add` / `cargo fetch` / `cargo build`（外部クレート取得を含む）が実際に通ることを段階0の試行クレート群で確認済み。cargo は使える。全体を止める理由はなかった。

## D-002 Rust ツールチェーン（2026-11-02、段階0）

- rustup（minimal profile）で stable を導入。`rustc 1.99.1 (58cf8f9c3 2026-10-13)`、`cargo 1.99.1`。
- ビルド設定は Plan 4章どおり: `CARGO_TARGET_DIR=/home/hatch/rustscrapling-target`、`TMPDIR=/home/hatch/tmp`、`CARGO_BUILD_JOBS=2`、root `Cargo.toml` に `[profile.dev] debug = "line-tables-only"` と `[profile.release] lto = true`。
- 再現用に `scripts/env.sh` へ同じ値を置いた。
- 空のワークスペース（Plan 8章の11クレート）で `cargo build` が通ることを確認（11クレート全てコンパイル、約1.3秒）。

## D-003 Chrome for Testing（2026-11-02、段階0）

- Stable 156.0.8078.80（linux64）を `/home/hatch/chrome-for-testing/chrome-linux64/chrome` に導入。不足共有ライブラリなし（ldd で not found 0件）。
- headless は `file://` ページの `--dump-dom` で動作確認。
- headed は `xvfb-run` 下で起動し、CDP の `/json/version` が `Chrome/156.0.8078.80` を返し、UA に Headless が付かないこと（`Chrome/156.0.0.0` 表記）で確認した。証跡は計測時の `/home/hatch/tmp/cdp-version.json`。
- 注意: Chrome は環境変数のプロキシを読まない。プロキシなしで外部サイトへ出すと `ERR_TUNNEL_CONNECTION_FAILED` になった。段階4では `--proxy-server` の明示指定と、認証付きプロキシへの対応（CDP の Fetch ドメインでの認証応答）を実装条件に加える。

## D-004 libxml2 のビルド組み込み（2026-11-02、段階0）

- システムに libxml2 の開発部品はない（pkg-config に libxml-2.0 なし、`/usr/include/libxml2` なし）。
- lxml 6.1.3 が同梱するのと同じ libxml2 2.14.6 を公式ソース（download.gnome.org）から静的ビルドし、`/home/hatch/libxml2` に導入した（`--enable-static --disable-shared --without-python --without-lzma --without-zlib --without-iconv --without-icu --without-threads`、make -j2）。
- Rust の `libxml` クレート 0.3.23 は、環境変数 `LIBXML2=/home/hatch/libxml2/lib/libxml2.a` を渡すと pkg-config と bindgen（libclang）を使わずに静的リンクできることを試行で確認した。生成バイナリの ldd に libxml2 の共有リンクは現れない。
- 試行では HTML パースと XPath（`//span[@class="price"]`、`//td[text()="Cell 1"]`）が正しく動作した。
- 決定: 互換モードの解析器と XPath はこの経路（libxml クレート＋静的 libxml2 2.14.6）を使う。本家 lxml と同じエンジン・同じ版数のため、互換モードの DOM と XPath の一致はエンジン差では崩れない。

## D-005 XPath 既存クレートの試行（2026-11-02、段階0）

本家の試験から抜いた代表5式（属性の数値比較、`number(translate(substring-after(...)))`、変数 `$cell_text`、`count(ancestor::*)`、不正な式 `4 ayo` のエラー）を、XML 文書に対して試した。

| クレート | 版 | 結果 | 問題 |
|---|---|---|---|
| sxd-xpath（sxd-document と併用） | 0.4.2 | 5式すべて期待どおり | 評価対象が sxd-document の文書型に固定され、結果のノードも同型。Plan 8章のアリーナ型 DOM ではそのまま使えない |
| xmloxide | 0.5.0 | 5式すべて期待どおり | 同上で、xmloxide 自身の `Document`/`NodeId` 木に固定される |
| libxml2 の XPath（libxml クレート経由） | libxml2 2.14.6 | 試行2式とも正解 | 互換モード専用（D-004）。既定モードのアリーナ DOM では使えない |

- 「本家の試験を全て通せる」ことは段階0では未確認である。5式の通過は必要条件であって十分条件ではない。
- 決定: Plan 5章の既定どおり、既定モードの XPath 1.0 はアリーナ型 DOM 上に独自実装する。互換モードは libxml2 の XPath を使う（D-004）。段階1で oracle の正解データ一式（本家の parser 試験由来の全 XPath 式）に対して sxd-xpath / xmloxide をアダプタ経由で再評価し、全件通過するものがあれば Plan 5章の但し書きどおり採用に切り替えてここに追記する。

## D-006 wreq の HTTP/3 対応（2026-11-02、段階0）

- wreq 0.16.3 のソースを確認した。`HttpVersionPref` は Http1 / Http2 / All のみで、QUIC の実装（quinn、h3）への依存はない。TLS 設定型に ALPN の `h3` 定数はあるが、HTTP/3 の接続経路は存在しない。よって wreq は HTTP/3 に対応していない。
- 対処（Plan 段階0の指定どおり補う案を書く）: 段階3で `--http3` 指定時のみ quinn + h3（h3-quinn）による取得経路を別実装し、失敗時は HTTP/2 へ落とす。本家の Fetcher の `http3` 引数自体は引数互換のため受け付ける。
- この環境は UDP が通らずプロキシ経由のみのため、実サイトでの HTTP/3 確認はできない（Plan 13章どおり）。単体試験はローカルの h3 サーバで段階3に行う。

## D-007 chromiumoxide と Runtime.enable（2026-11-02、段階0）

- chromiumoxide 0.9.1 のソースを確認した。`FrameManager::init_commands()` がページ初期化コマンド列に `Runtime.enable` を無条件で含み、`Page::enable_runtime` のドキュメントにも「Activated by default」とある。つまり標準の Page 経路では、Runtime.enable を送らずに要素を操作することはできない。
- 決定: `rustscrapling-browser` のステルス経路では chromiumoxide の Page / FrameManager 抽象を使わず、CDP のトランスポート（chromiumoxide_cdp の型と接続）を使って Target / Page / DOM ドメインを直接送る。Runtime は `browser_evaluate` のように明示的に必要な操作のときだけ有効化する。これは patchright が Runtime.enable を避ける方針と揃う。通常（非ステルス）の DynamicFetcher 相当の経路では Page 抽象を使ってよい。
- 段階4の終わりに成功率で Plan 5.1 の代替案（patchright ドライバ子プロセス）へ切り替えるかどうかを判断し、ここに追記する。

## D-008 段階0の試行物の置き場所（2026-11-02）

- 試行プロジェクト（trial-libxml、trial-xpath-sxd、trial-xmloxide、trial-net）は再現可能性のためリポジトリ内の `stage0-trials/` に保存する。各プロジェクトは独立した `[workspace]` を持ち、本体のワークスペースには含めない。
- ビルド時の環境は `scripts/env.sh` を source すること。

## D-005 追記（2026-11-02、段階1の結果）

- 段階1で CSS・XPath の両方をアリーナ DOM 上に独自実装し、oracle の正解データ88件（CSS 59 + XPath 29、エラー症例含む）と互換モードで 100% 一致した。互換モードでも libxml2 の XPath は使っていない（使うのは libxml2 の HTML パーサだけ）。D-005 の「互換モードは libxml2 の XPath を使う」は実装結果に合わせて取り消す。
- sxd-xpath / xmloxide のアダプタ経由の再評価は、独自実装が先に全件通過したため実施しなかった。Plan 5章の但し書きは「独自実装の前に既存クレートを試す」ことが目的で、試行自体は段階0で完了している。

## D-009 trial-net の実行結果（2026-11-02、段階0の追補）

- chromiumoxide: Chrome for Testing 156 を起動し、CDP 経由でページのタイトルと DOM を取得できた（ローカルページ）。D-007 の方針（トランスポート層を直接使う）は実装可能と確認した。
- wreq: プロキシ経由で example.com への CONNECT と TLS ハンドシェイク開始までは進んだが、BoringSSL の証明書検証が CERTIFICATE_VERIFY_FAILED で失敗した。この環境の出口プロキシは中間者プロキシで、システムの CA バンドル（/etc/ssl/certs/ca-certificates.crt）にはその CA が入っているが、wreq の既定の検証ストアには入っていない。
- 対処: 段階3の HTTP クライアントは、起動時にシステム CA バンドルを読み込んで検証ストアへ追加する（環境変数で差し替え可能にする）。証明書検証の無効化は既定にしない。

## D-010 ブラウザ起動引数を本家と揃える／`--password-store=basic`（2026-11-03、後継セッション）

- 問題：Chrome155/156でlocalhost遷移が止まる事象は、テスト専用プロファイルに`--password-store=basic`を付けると解消していた（HANDOFF/旧RESULTS）。Plan B-14は「一般ユーザープロファイルへ自動適用しない」ことを求める。
- 調査：固定SHAの本家は`scrapling/engines/constants.py`の`DEFAULT_ARGS`に`--password-store=basic`を含め、`STEALTH_ARGS`（`--disable-blink-features=AutomationControlled`等54個）と共に**すべての起動**へ付与している。viewport/screenは1920×1080。
- 決定：RustScraplingが自ら起動するChromeには`UPSTREAM_LAUNCH_ARGS`（本家`DEFAULT_ARGS`+`STEALTH_ARGS`）を付ける。RustScraplingは常に一時プロファイル（起動ごとに新規作成・終了時削除）を使い、利用者の永続プロファイルを開かないため、basic password storeが既存の資格情報保存を弱めることはない。`cdp_url`で外部ブラウザに接続する場合は本家同様に起動設定を適用しない。headless時のUAは`HeadlessChrome`を`Chrome`に置換（本家は生成UA）。
- 互換影響：本家と同じフラグ構成になり、取得成功率比較（G-04）の前提が揃う。HTTPSエラー無視（本家`ignore_https_errors=True`）は未採用で、TLS検証は維持（差分としてCOMPATに記録、G-04で影響を測定してから再判断）。
- 戻し方：`rustscrapling-browser::UPSTREAM_LAUNCH_ARGS`の付与を外す。

## D-011 Playwrightの起動スイッチも本家同様に付与し、機能フラグを統合（2026-11-03、後継セッション）

- 問題：Chrome 156のheaded再利用試験で、遷移直後の`click`が**エラー無しで効かない**ラウンドが約1/150発生した（textboxは成功）。
- 調査：本家のブラウザプロセスはpatchright-core 1.64の`chromiumSwitches()`（`--disable-features=…,PaintHolding,…`、`--enable-features=CDPScreenshotNewSurface`、`--disable-field-trial-config`等）を**Scraplingの引数より前に**受け取る。PaintHolding（遷移直後に旧フレームを保持し入力を新ページへ届けない）はPlaywrightが自動操作の安定性のため無効化しているが、D-010の`UPSTREAM_LAUNCH_ARGS`はScrapling側の引数のみでこれを欠いていた。Playwrightは`chromiumSandbox`未指定時`--no-sandbox`も付ける。
- 決定：`upstream_launch_args(headless)`＝Playwright switches＋headless追加分＋`UPSTREAM_LAUNCH_ARGS`をMCPとCLI（fetch tier/shell）の自前起動に付与。Chromeは同名スイッチの最後の値のみ使うため、`--enable-features`/`--disable-features`/`--disable-blink-features`は起動時に和集合の1本へ統合（`merge_feature_switches`）。`--no-sandbox`は**付与しない**（サンドボックス維持、本家との差分）。
- 証拠：変更後、Chrome 155/156でheaded・headless各150ラウンド（計600）取りこぼし0件。153/155/156で実機E2Eマトリクス全PASS（`scripts/chrome_e2e_matrix.sh`）。
- 互換影響：本家の実プロセス引数に近づく。Chromeが重複スイッチをどう扱うかに依存せず、Playwright/Scrapling双方の機能指定が有効になる（本家は最後の`--disable-features`のみ有効になっている可能性がある＝差分として記録）。
- 戻し方：`rustscrapling_browser::upstream_launch_args`を`UPSTREAM_LAUNCH_ARGS`に戻し、`merge_feature_switches`呼び出しを外す。

## D-012 外部CDP接続はloopbackか明示allowlistのみ（2026-11-03、後継セッション）

- 問題：`cdp_url`（MCP `browser_open`/CLI `--cdp-url`/shell）は任意のホストへのDevTools接続を受け付けていた。Plan 7.1は「CDPの外部接続は許可されたendpointのみ」を要求する。MCPではAIエージェントが取得本文に誘導されて接続先を選ぶ可能性がある。
- 決定：`BrowserClient::connect`で`ws`/`wss`のみ、URL内資格情報・空白/制御文字を拒否。ホストがloopback（127/8、::1、localhost）でなければ`RUSTSCRAPLING_CDP_ALLOWED_HOSTS`（カンマ区切り、完全一致）に含まれる必要がある。拒否は`invalid_argument`系typed error。
- 互換影響：本家は任意の`cdp_url`を受け付ける。リモートブラウザを使う利用者は環境変数で明示許可する（COMPATに記録）。
- 戻し方：`check_cdp_endpoint`呼び出しを外す。

## D-013 `main_content_only`で`hidden`属性の要素も除去（2026-11-03、後継セッション）

- 問題：G-07評価（`fixtures/extraction`、`bench/eval_extraction.py`）で、本家`Convertor._sanitize_for_ai`は`aria-hidden`・CSS非表示・`<template>`・hidden `<slot>`は除くが、HTML`hidden`属性の一般要素（`<p hidden>`）を残すため、**非表示のプロンプト注入文がAI向け本文に混入**することを確認（全8ページ）。Rust移植も本家どおり混入していた。
- 決定：`main_content_only=True`のときに限り、`hidden`属性（`until-found`を含む）を持つ要素を除去する。全文モード（`main_content_only=False`）は本家どおり。Plan 7.1/E-03（非表示要素・隠し指示の除外）を優先した安全側の差分。
- 証拠：本家Convertorオラクル240/240は一致のまま（fixtureの`hidden`は`slot`のみ）。G-07評価でRust移植の混入0・保持率100%・全ページ文字数≤本家。
- 互換影響：`hidden`属性付き要素を含むページで、MCPの`main_content_only`出力が本家より短くなる（COMPATに記録）。
- 戻し方：`compat.rs::hidden_or_noise`の`hidden_attribute`条件を外す。

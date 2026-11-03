# RESULTS.md: 測定と検証の結果

## 2026-11-03 後継セッション: Issue #1修正・2ブランチ統合・browser_actions全12種

検証環境：claude.ai cloud の隔離Linuxコンテナ（Ubuntu 24.04系、4 vCPU、15GiB RAM、Rust 1.99.1、libxml2 2.9.14、Google Chrome for Testing **153.0.8010.12**、Python 3.13.18）。VPS・Windows/WSLは使用していない。Chrome 155/156ではないため **G-03の合格根拠にはならない**。

| 対象SHA | コマンド | 結果 |
| --- | --- | --- |
| `7752546`（m0+spider統合直後） | `cargo test --workspace --lib --tests --locked` | **FAIL**: `rustscrapling-mcp/tests/runtime.rs::browser_get_and_session_if_chrome_is_available` が `net::ERR_CONNECTION_REFUSED`（GitHub Actions run 5と同一症状をローカル再現） |
| `8bd7146`（Issue #1修正） | `cargo fmt --all -- --check` / `cargo test --workspace --lib --tests --locked --no-fail-fast` / `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0 / **127 PASS・0 FAIL・3 ignored・0 filtered** / exit 0 |
| `8bd7146` | `RUSTSCRAPLING_CHROME_FOR_TESTING=/usr/bin/google-chrome RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX=1 cargo test -p rustscrapling-browser --tests -- --ignored` | 実Chrome 3/3 PASS |
| `8bd7146` | `cargo build -p rustscrapling-cli --locked` + `python3 tests/e2e_local.py target/debug/rustscrapling` | exit 0 / CLI E2E 6/6 PASS |
| `8bd7146` | `python3 -m unittest discover -s tests -p 'test_m0*.py'` / `inventory_upstream.py --check` / `audit_parity.py --check` | 37 tests OK（2 skip）/ exit 0 / exit 0 |
| `8bd7146` | live runtime schema oracle（隔離venv、mcp 2.3.0、pydantic 2.14.2） | 16/16ツール、差分**348件**（コミット済みスナップショットと`differences`完全一致）。exit 1は設計どおり |
| `1a74ab5`（全12アクション） | 全workspaceテスト | **130 PASS・0 FAIL・4 ignored（実Chrome opt-in）** exit 0、fmt exit 0 |
| `1a74ab5` | 実Chrome opt-in（上記env） | browser 4/4 PASS（新規 `all_actions_chrome_e2e` を含む） |

### Issue #1 の根本原因

`net::ERR_CONNECTION_REFUSED` はブラウザ起動・鍵管理の問題ではなく**テストfixtureの欠陥**。MCP runtimeテストのHTTPサーバは`accept()`を2回だけ行う実装で、Chromeが`GET /page`の後に`GET /favicon.ico`を別接続で要求したため2回目の枠を消費しサーバスレッドが終了した。続く`browser_fetch`の遷移は待受ソケットが閉じており接続拒否になった（受信ログ：`GET /page` → `GET /favicon.ico`）。修正（`8bd7146`）はテスト終了まで全接続を処理し、`/page`以外へ404を返し、`/page`が2回以上配信されたことを検証する。製品Launcherに`--password-store=basic`は追加していない。修正後同テストを5回連続PASS。

### 統合したブランチ

- `feat/m0-runtime-schema`（`e00b0b0`+`f0275ce`）をmerge commit `4aaaab9`で統合。M0系37テストOK、Linuxで本家live `tools/list`との比較を再現（348件はJSON表現差を含む構造差分の数であり、互換バグ件数ではない）。
- `feat/spider-rich-request-s03`（`ce77df3`+`98707e6`）をmerge commit `7752546`で統合。Spider 39テストを含む全workspace回帰PASS（`8bd7146`）。S-03の残り（cookies/session/metadata/callback/errback、CLI経由rich request、checkpoint暗号化、priority frontierの計算量）は未達のまま。
- `handoff/local-docs-wip`（`7d89ce1`）は未レビュー資料として扱い、merge していない。内容を現行コードと照合し、正確な記述（MCP network本文の上限、textbox clear=trueの実装）は本書・COMPATへ書き直して反映し、Spider GET-only記述等の古くなった記述は採用しない。VPS上の過去ログ（`c6434bb`/`4d1ed34`）は本セッションで再検証できないため証跡として転記しない。

### browser_actions 全12種（`1a74ab5`）

- 実装：`rustscrapling-browser`の新`actions`モジュール（Page.createIsolatedWorldの補助world内でPlaywright互換selector/actionabilityを評価し、入力はCDP `Input`ドメインで送信）。MCP `browser_actions`はこれに接続。
- 本家との突合：同じChrome 153で本家Scrapling v0.5（patchright 1.64.4）を隔離venvから実行し、`Control+A`のevent.key=`A`、未フォーカスinputへの`clear=false`入力がcaret先頭（`Xold`）、strict mode違反・`Input of type "checkbox" cannot be filled`・`Unknown key: "Foo"`・option欠落時timeoutの文言、`click_count=2`で`event.detail=2`を確認しRust実装へ反映。
- テスト：`crates/rustscrapling-browser/tests/all_actions_chrome_e2e.rs`（全12種＋shadow DOM、xpath/text/role、ラベルretarget、dialog accept/prompt/自動dismiss、遮蔽/disabled/未知engineの失敗、slowly）、`crates/rustscrapling-mcp/tests/actions_runtime.rs`（本家`_validate_actions`の文言、セッション前検証、実Chromeでのツール経由チェーン）。`Runtime.enable`が送られないことも検証。
- 限界：iframe内要素（`aria-ref`のframe ref含む）、CSSのshadow境界をまたぐ子孫結合子、Playwright CSS拡張（`:has-text()`等）、`role=`のaccessible name計算の完全互換、macOS編集コマンドは未対応または近似。ARIA snapshot（`aria-ref`の発行元）は未実装のため、`aria-ref=`は現状どのrefにも一致しない。

## 2026-11-03 CDP通信本文・LinkExtractor補完後のVPS統合回帰

- 統合ブランチ `integration/plan-v2-recovered` の `22a7d60`（Spider `8c2ca06` とBrowser `22a7d60` を含む）で、VPS Ubuntu 24.04・Rust1.99.1の `cargo test --workspace --lib --tests --locked --offline -- --skip local_browser_navigation_reuse_external_connection_and_shadow_dom --skip browser_get_and_session_if_chrome_is_available` が **104件PASS、0件FAIL、Chrome HTTP実機2件filtered out、Chrome data URL実機1件ignored、終了コード0**。`systemd-run --user --scope`、`MemoryMax=3000M`、`MemorySwapMax=0`、Cargo2ジョブ、`nice -n 12`、420秒timeoutで実行。ログはVPSの `/tmp/rustscrapling-22a7d60-tests.log`。
- 同HEADで `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` が **終了コード0**（`MemoryMax=2800M`、スワップ追加なし、360秒timeout）、`cargo fmt --all --check` も終了コード0。ClippyログはVPSの `/tmp/rustscrapling-22a7d60-clippy.log`。
- Spiderは宣言的な `link_allow` / `link_deny` URL正規表現を追加（各64件、1件1024バイト、コンパイルサイズ制限）。大量の対象外リンクを先に除外してfrontier枠を守り、seedには適用しない。Spider単体27件PASS。ただしPythonのLinkExtractor全オプションやカスタムparse/Request callbackの互換を意味しない。
- Browserは同一CDP page session内の`Network.responseReceived`/`loadingFinished`をrequestIdで関連付け、XHR/FetchのJSON/テキスト応答本文を有界取得する。128履歴・64KiB/件・合計1MiB・2秒上限、binary/base64/大型bodyの除外、URL/レスポンスヘッダ/JSON機密値の秘匿を実装。CDP mockの追加テストを含むworkspace回帰はPASS。ただしChrome実機のlocalhost HTTP遷移問題は継続中で、ブラウザ通信本文の実サイトE2EとMCP経由でのbody参照は**未達**。

## 2026-11-03 先行Rust移植を参照したStage6補完・VPS実証

- 比較した公開Rust移植: `Liohtml/RUSTScrapling`の`2dd9a5eb0627fdb7b5c0aae7b5fc4dc9e8cc092c`（robots.rs/throttle.rs/cache.rs）、`chrisabruce/scrapling-rs`の`0d61c3e88d95473d77d90d3e374ffded79de07da`。Python本家の固定仕様はPlan.md記載`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`。採用対象とライセンスを`docs/UPSTREAM_PORTS.md`、`NOTICE`、`licenses/RUSTScrapling-MIT.txt`に記録。
- `a45f41d`: robots.txtの複数User-agentグループ、Crawl-delay、Request-rateを実装。静的`delay_ms`とAutoThrottleの下限として扱う。異常値(NaN/負数/24h超)は拒否、同一originの並列要求を時間間隔で制御。robots取得失敗は既存のfail-closed方針を継続。
- `a2fd050`: 開発モードのレスポンスキャッシュを実装。成功した非チャレンジの8MiB以下の本文をURLハッシュ名で保存（Unixの専用ディレクトリ0700、ファイル0600）。次回同一ページのネットワーク再取得を抑えつつ抽出を再実行。開発モードでは本番のチェックポイントを読み書きしない。robots.txtはキャッシュせず毎回取得する。
- `51e3d6c`/`5d0c1a6`/`8541fca`: レスポンスヘッダをSpiderへ渡し、HTTP 429/503の`Retry-After`を秒数形式およびHTTP-date形式で処理。並列待機中の要求も最新のバックオフ期限を再評価する。HTTP-date解析には既存Cargo.lock内のchronoを採用。
- VPS Ubuntu Rust1.99.1、`MemoryMax=3000M`、`MemorySwapMax=0`、Cargo2ジョブ、nice低優先度で、`cargo test --workspace --lib --tests --locked --offline -- --skip local_browser_navigation_reuse_external_connection_and_shadow_dom --skip browser_get_and_session_if_chrome_is_available` は**95件PASS/0件FAIL、Chrome HTTP実機2件を明示除外、data URL実機1件ignored、終了コード0**（`51e3d6c`時点）。`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`も同コミットで終了コード0。その後のHTTP-date対応`8541fca`ではSpider25/25 PASS（全workspace/clippy追試は別途）。
- この結果はローカルモック/localhostの機能テスト。実サイトでのrobots遵守・成功率や開発キャッシュの実運用耐久、Python本家と同条件の比較が完了したわけではない。

## 2026-11-03 AutoThrottle統合後のVPS全体回帰

- コミット`e1bec53`でVPS Ubuntu 24.04・Rust1.99.1の`cargo test --workspace --lib --tests --locked --offline -- --skip local_browser_navigation_reuse_external_connection_and_shadow_dom --skip browser_get_and_session_if_chrome_is_available`を実行。**87件PASS、0件FAIL、2件filtered out、Chrome data URLの実機試験1件ignored、終了コード0**。`MemoryMax=3000M`、`MemorySwapMax=0`、`CARGO_BUILD_JOBS=2`、`nice -n 12`を使用。
- `cargo clippy -p rustscrapling-spider --all-targets --locked --offline -- -D warnings`と`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`はどちらも最新統合版で**終了コード0**。全workspace Clippyには約3分58秒かかったが、VPS上で`MemoryMax=2800M`、`MemorySwapMax=0`、2 jobsの上限を維持した。HTTPを使用するChrome実機E2Eの安定通過は依然未達。

## 2026-11-03 Stage6 オプトインAutoThrottle追加

- 統合コミット`05cda33`で`CrawlSpec.auto_throttle`を導入（既定false）。有効化した場合のみホスト別の動的な取得開始間隔を設定し、HTTP 408/429/502/503/504や取得失敗で上限付きバックオフ、200〜399の応答では実測応答時間の半分または既定の最小間隔に向けて平滑化する。`delay_ms`指定値を下回らず、`delay_ms=0`のAutoThrottle時は最小100ms。実行中リクエストはキャンセルせず、次回以降の取得開始時刻に反映する。`Retry-After`解析・分散ジョブ間の共有レート制限は未実装。
- 旧CrawlSpec/チェックポイントの互換性を保つため`auto_throttle=false`はJSON出力から省略する。VPS Ubuntu・Rust1.99.1・Cargo offlineで`cargo test -p rustscrapling-spider --locked --offline` **17/17 PASS**。429後に同一ホストの次回開始が延期される試験、バックオフ/緩和、旧specハッシュを変えない試験を含む。これはモックFetchによる機能試験であり、外部サーバー負荷の低減や取得成功率を実測したものではない。
- VPS Chrome155で`RUSTSCRAPLING_CHROME_FOR_TESTING=/usr/bin/google-chrome`、明示`--ignored`、テスト専用サンドボックス解除を指定した`chrome_data_e2e`が**1/1 PASS（約0.93秒）**。DOM/PNG/明示evaluateとRuntime非自動有効化を検証した。HTTPを使うlocalhost Chrome実機E2Eは依然未達。

## 2026-11-03 VPSでのCLI性能・Daemon feature・Chrome for Testing追加追試

- 現行統合版をVPS（Ubuntu/Linux、Rust1.99.1、Debugビルド）で`cargo build -p rustscrapling-cli --locked --offline`した後、`python3 tests/e2e_local.py target/debug/rustscrapling`は6/6 PASS。`python3 scripts/stress_local.py target/debug/rustscrapling`は固定localhost150ページ×並列8で**0.261秒、最大RSS24.46MiB、チェックポイント再開0.011秒**（1回実測）。WSL測定値0.877秒と異なる環境の値を性能改善倍率として扱わない。
- VPSで`cargo test -p rustscrapling-daemon --features fetch-integration --locked --offline`を実行し、local E2E **7/7 PASS**。SQLiteによる再起動復元、304、token境界、認証IPC・状態ファイル保護を含む。
- GoogleのChrome for Testing公式配布から156.0.8078.80のLinux64ビルドをVPSの専用`~/tools/rustscrapling-chrome-156`へ導入（既存ブラウザ設定は変更せず）。`RUSTSCRAPLING_CHROME_FOR_TESTING`を明示して実機localhost HTTP E2Eを走らせても、Chrome 155と同様に`Page.navigate`がタイムアウト。対照的にHTTPを使わない`data:` URLでの実CDP E2E（`cargo test -p rustscrapling-browser --test chrome_data_e2e -- --ignored`）は**1/1 PASS（約1.28秒）**し、DOM取得・PNG screenshot・明示Runtime.evaluateと通常操作のRuntime非有効化を確認した。`chrome_data_e2e`は通常テストで明示ignoreされるオプトイン実機試験。VPS localhost HTTPのネットワーク異常は未解決。

## 2026-11-03 VPS移管後の取得漏れ修正・チェックポイント保護・実機検証

- 負荷の高いRustビルド/試験をWindows PCからUbuntu VPS `inouemasaki-ai` の `~/Project/RustScrapling` に移管。VPSでRust 1.95.0を1.99.1に更新した。Tailscale SSHによるGit同期を用い、ビルド/試験はVPSの `systemd-run --user --scope` で `MemoryMax=2600M`（全体試験は3000～3200M）、`MemorySwapMax=0`、`CARGO_BUILD_JOBS=2`、低優先度 `nice -n 12` を指定。PCのWSLで追加ビルドは実施していない。
- クローラ: 先頭に大量の対象外ドメイン/既訪問リンクがあるページで、`max_pages`分だけ先にリンクを切り捨てると有効リンクが消える不具合を修正。初期seedの重複/フラグメント違いによるページ上限の誤消費も修正。未処理URLに秘密のクエリが含まれる可能性を踏まえ、Unixチェックポイントをmode 0600、新規専用ディレクトリをmode 0700で作成し、旧state権限移行・symlink拒否・排他作成の一時ファイルを導入。`cargo test -p rustscrapling-spider --locked --offline` 14件PASS。
- 全ワークスペース: VPS Ubuntu Rust1.99.1で `cargo test --workspace --lib --tests --locked --offline -- --skip local_browser_navigation_reuse_external_connection_and_shadow_dom --skip browser_get_and_session_if_chrome_is_available` は**83件PASS、終了コード0**。これにはdaemonのクロスプロセスCLI E2EとMCP mock CDP E2Eを含む。Chrome実機依存2件は明示的に除外しており、全テスト無条件PASSの意味ではない。
- VPSのGoogle Chrome 155を使ったlocalhost実機E2Eは失敗。CDPで`Target.attachToTarget`/`Page.enable`/`Network.enable`は成功するが、`Page.navigate`が断続的に20秒タイムアウト。ローカルHTTPサーバのヘッダ読み取りを完結させる修正とテスト用プロキシ無効化で一度はページ遷移、DOM/Shadow DOM取得、明示evaluateまで成功したが、Chrome終了時プロファイル削除と子プロセスの競合が発生。削除は有界リトライに修正。再実行で遷移タイムアウトが再現したため、Chrome実機E2Eの安定通過は**未検証・未達**。サンドボックス無効化は明示指定したlocalhostテストに限定し、通常のブラウザ起動の安全性を弱めていない。CDPトレースは環境変数指定時にコマンド名/IDだけを出す（URLや資格情報は出さない）。
- 切り分け: 同じVPSのChrome155をRustScrapling/CDPを介さず、`--headless=new --dump-dom http://127.0.0.1:18763/`（専用一時プロファイル、プロキシ無効）で起動しても17秒でタイムアウト。Python HTTPサーバはcurlのHEADには応答するがChromeからのHTTP要求本文は到達しなかった。Chromeの`about:blank`と`data:`ページは単体で正常表示した。Chrome netlog上はloopback TCP接続済みだがURL requestが実際のHTTP送信まで進んでいない。VPS Chrome/ホスト環境側の問題も強く疑われるが原因確定ではない。ChromeのLocal Network Accessチェック無効化を試してもタイムアウトは改善しなかった。環境を変えた実Chrome E2Eと外部サイト実測が必要。
- CDPで応答がない場合、従来の`Page.navigate`コマンドは呼び出し側`navigation_timeout`(標準15秒)に先立ち固定20秒待つ可能性があった。コマンド応答とページロードを**同一のnavigation deadline**に束ね、無応答CDPモックで150ms期限のテストを追加。VPS `cargo test -p rustscrapling-browser --test navigation_timeout --locked --offline` は1/1 PASS（0.16秒）。`cargo fmt --all --check`および`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`はVPSで終了コード0。
- 上記CDP期限修正とCLI strict Clippy修正（`bd79dd4`）を含むVPS最終検査: `cargo test --workspace --lib --tests --locked --offline -- --skip local_browser_navigation_reuse_external_connection_and_shadow_dom --skip browser_get_and_session_if_chrome_is_available` が**84 PASS、2件明示除外、終了コード0**。`cargo fmt --all --check`と`cargo clippy --workspace --all-targets --locked --offline -- -D warnings`はともに終了コード0。VPSでは2ジョブ、3GB前後のcgroup上限・スワップ追加割当なし・nice低優先度を維持。実Chromeの2件を含めた全件成功と混同しない。
- VPS側でこの版の約200実サイト比較・24時間耐久試験・Chrome実機でのlocalStorage往復の通過確認はしていない。旧WSLの150ページ負荷測定をVPSの新しい速度結果として扱わない。

## 2026-11-03 継続開発・追加機能の統合再検証

- 統合ブランチ `integration/plan-v2-recovered` に HTTP入力検査、ExtractのURL credential秘匿、crawler大量リンク上限/HTML base URL、daemon CLI再起動E2E、BrowserState v2(現行トップレベルoriginのlocalStorage) を統合。WSL2 Ubuntu 24.04 / Rust 1.99.1 / Debug test / `CARGO_BUILD_JOBS=2`で `cargo test --workspace --lib --tests --locked --offline` の**終了コード0**を確認。ビルド時に一度見つかったbrowser stateファイル書込関数の型推論エラーを修正してから再実行した結果。
- `cargo test -p rustscrapling-extract --locked --offline` で6件PASS、`cargo test -p rustscrapling-spider --locked --offline` で11件PASS。HttpClientで認証情報を含むURLを拒否、retries>10を拒否するテストを追加。クローラは2,000リンクを含むページでもmax_pages=3の訪問数・キューを守るテスト、およびHTML `<base href>` の相対URL解決試験を追加。
- daemon CLIは別プロセスから start/get/auto/stop と再起動後SQLite復元/304を試験。MCP mock CDPはcookie/localStorage保存・復元(v1/v2)、別origin拒否とPNG/JPEG撮影を確認。これらはlocalhost/CDPモックの検証であり、外部サイトやChrome実機成功率の証明ではない。
- `cargo test -p rustscrapling-cli --test daemon_commands --locked --offline` は4/4 PASS、`cargo test -p rustscrapling-mcp --test mock_cdp --locked --offline` は1/1 PASS、`cargo test -p rustscrapling-http --test http_local --locked --offline` は8/8 PASS。前者は実CLIプロセス/localhostサーバを用い、後者はモックCDPとlocalhost HTTPサーバを使う。
- 最新のDebug CLIを `cargo build -p rustscrapling-cli --locked --offline` でビルド後、`python3 tests/e2e_local.py /root/rustscrapling-target/debug/rustscrapling` は6/6 PASS。`python3 scripts/stress_local.py /root/rustscrapling-target/debug/rustscrapling` はlocalhost固定150ページ、並列8、**0.877秒、最大RSS24.82MiB、チェックポイント再開0.039秒**。Windows 11のWSL2 Ubuntu 24.04、Rust 1.99.1、他の並行作業がありうる環境での1回測定。本家との同条件比較や外部サイトの成功率ではない。
- 大規模サイト約200件の本家との同条件成功率比較、HTTP3外部接続の定期検証、24時間安定性、実Chromeでの新しいlocalStorage機能E2Eは未実施。旧値0.79秒と新値0.877秒を速度改善/悪化の証明として比較しない。

## 2026-11-03 引き継ぎ統合後ワークスペース再検証

- `integration/plan-v2-recovered` にクローラ修正、SQLite永続化、MCP Cookie state・CDP screenshotとmock CDP試験を統合した後、WSL2 Ubuntu 24.04（Rust 1.99.1、`CARGO_TARGET_DIR=/root/rustscrapling-target`、`CARGO_BUILD_JOBS=2`）で `cargo test --workspace --lib --tests --locked --offline` を実行し **終了コード0** を確認。既存テストと追加テストを通過した。
- この再検証でChrome実機依存テストはChrome実行ファイル不在のため内部スキップ。新規MCPのmock CDP E2Eは別途1件PASS。24時間試験や本家との同条件比較は行っていない。

## 2026-11-03 MCPブラウザ状態とCDP画像の追加検証

- `feat/mcp-browser-state` を統合。Windows上のWSL2 Ubuntu 24.04で `cargo test -p rustscrapling-browser -p rustscrapling-mcp --tests` はPASS、追加の `cargo test -p rustscrapling-mcp --test mock_cdp` は1/1 PASS。モックCDPとMCPを接続し、PNG/JPEGスクリーンショットのbase64返送、full_page clip・qualityパラメータ、Cookieの保存→値変更→復元→再保存を検証した。
- 新機能の実Chrome E2EはWSL環境にChrome実行ファイルがなく内部スキップされたため未実施。Windows native の初回依存ビルドはBoringSSLのNASM不足により完了していない。これらを成功として計上しない。
- ブラウザ状態はCookieのみ（最大4MiB、専用非公開ディレクトリ、単純ファイル名、上書き禁止）、localStorageとIndexedDBは対象外。スクリーンショットはPNG/JPEG、32MP/8MiB上限。selector/ref指定の撮影も未実装。

## 2026-11-03 Daemon SQLite永続化

- Windows 11ネイティブ（Rust 1.99.1）、`cargo check -p rustscrapling-daemon`、`cargo test -p rustscrapling-daemon`（6件）、`cargo clippy -p rustscrapling-daemon --all-targets -- -D warnings`がPASS。担当worktree `feat/daemon-persistent` の実測結果を統合コミットへ採用。
- 再起動をまたぐ条件付きGET本文/ETag・304復元、学習tierのTTL、キャッシュ期限切れ、Cookie・認証・クエリ等を含むリクエストの非永続化、private/no-store/Vary/Set-Cookie応答の非永続化、壊れたDBの型付きエラーを試験した。DBは専用private状態ディレクトリに保存される。
- `fetch-integration`機能を付けた検査、クロスプロセス実CLI E2E、本家と同じ実サイトでの性能・成功率比較はこの確認に含まれない。

## 2026-11-03 クローラの速度制限・robots追試

- 環境: Windows 11上のWSL2 Ubuntu 24.04、rootユーザーのRust 1.99.1、Debug test、Cargo.lock固定。`cargo test -p rustscrapling-spider --locked --offline` は9件PASS。
- 修正前は `FuturesUnordered` のfutureをバッチ構築後に初めてpollしていたため、ホスト別待機をバッチ構築中に行っても複数取得が一斉開始する場合があった。実際の取得開始前に同一ホスト専用の非同期ゲートを取得し、前回開始時刻からの間隔を保証するよう修正した。
- 60/90msの指定間隔に対し、同じホストの取得開始時刻が離れることをテスト。別ホストの遅い`robots.txt`応答でバッチ構築が遅延したケースも含む。これはモック取得の時刻を用いた機能試験で、実サイトにおける公平性や長時間の性能値を示すものではない。
- `robots.txt`はパスだけでなくクエリ文字列も照合するよう修正。`/search?private=true`を禁止し、`/search?private=false`を許可するモック試験を追加。既定の`obey_robots=false`は変更していない。

## 2026-11-03 統合再検証

- 環境は Windows 11 上の WSL2 Ubuntu 24.04、Rust 1.99.1、libxml2 2.9.14。Debug test、Cargo.lock固定、ビルド並列数2。
- cargo test --workspace --lib --tests --locked は終了コード0。Adaptive14、Browser2、CLI5、Daemon4、DOM10、Extract5、Fetch3、HTTP/TLS/QUIC8、MCP6、Select5、Spider6の計68件がPASSした。Adaptiveの30,000ケース照合を含む。
- HTTP/3: ローカルQUICサーバで自己署名CAを明示的に信頼し、実際のHTTP/3ネゴシエーションと本文受信を確認。HTTP/2フォールバックをHTTP/3成功としてカウントしない。
- python3 tests/e2e_local.py /root/rustscrapling-target/debug/rustscrapling は6件PASS。localhostのHTTP GET/POST、Markdown、select、auto、crawlを検証。
- python3 scripts/stress_local.py /root/rustscrapling-target/debug/rustscrapling はlocalhost固定HTML150ページ、並列数8、0.79秒、最大RSS24.99MiB、再開0.025秒。外部ネットワークや本家との差を測ったものではない。
- 24時間安定性、約200件の実サイトによる本家比較、Cloudflare成功率、Adaptiveの残りの速度目標は、これらの試験では達成を証明できない。

## 2026-11-03 WSL統合追試

- 機能復旧後のcargo test --workspace --lib --testsでは、Adaptive 8 unit + 6 fixture、Browser 1 unit + 1 localhost E2E、CLI 1 unit + 1 integration、Daemon 4 integration、DOM 10 unit、Extract 2 unit + 3 fixture、Fetch 3 unitまでPASSしたが、HTTP/3のローカル試験でRustls CryptoProvider自動選択例外があり、全体としてFAIL。全体合格を主張しない。
- Git管理情報が別作業により置換されていたため、元の未コミット差分を保護して別作業コピーrustscrapling-deliveryに復旧。
- Stage6へ6種テンプレートと4出力形式を追加。cargo test -p rustscrapling-spider はunit 6件すべてPASS。cargo test -p rustscrapling-cli --locked はunit 2件、stage2統合1件PASS。実サイト/本家比較は未実施。
- Criterionベンチマークをappendix_a.rsに追加したが、releaseで計測するまでは新しい性能値を記録しない。

## 段階1（2026-11-02）

計測条件: Muse VM（Ubuntu 24.04.5、4コア、メモリ7.7GiB）、release ビルド（`[profile.release] lto = true`、CARGO_BUILD_JOBS=2）。付録Aの合成ページ（416,497バイト、2000商品）。best-of-5、各計測の前に1回のウォームアップ。計測コード: `crates/rustscrapling-select/examples/bench_appendix.rs`。

| 処理 | RustScrapling | 本家（同一VM・同一日、BASELINE.md） | 段階目標 | 判定 |
|---|---|---|---|---|
| 解析（互換モード libxml2） | 13.5 ms | 8.5 ms | 15 ms以下 | 達成 |
| 解析（既定モード html5ever） | 16.5〜17.0 ms | 8.5 ms | 15 ms以下 | 未達（下記） |
| `css('.price::text')`（html5 DOM） | 0.74 ms | 10.3〜10.6 ms | 5 ms以下 | 達成（本家の約1/14） |
| `css('.price::text')`（libxml DOM） | 0.81 ms | 10.3〜10.6 ms | 5 ms以下 | 達成 |
| `xpath //span[@class='price']`（参考） | 1.5 ms | （未測） | なし | 参考値 |

html5ever 解析の未達について（Explain the Number）: 同じページで html5ever→RcDom のみの計測は 12.5 ms の回があり、セッション中の他作業（並行ビルド等）と4コアの競合で 16〜17 ms まで振れる。支配要因は html5ever のトークナイザとツリー構築そのもので、自前 TreeSink 化とワンショット入力でも 16.5 ms が下限だった。互換モードは目標内であり、段階8で criterion による管理された再測定を行う。目標未達として隠さず記録する。

選択の一致: `cargo test -p rustscrapling-select` の fixture 試験で、互換モードが oracle の正解データ88件（CSS 59 + XPath 29、エラー症例含む）と 100% 一致。既定モードとの差30件は docs/COMPAT.md に分類済み（tbody 補完と空白テキストの2クラスのみ、選択要素の集合に差なし）。セレクタ生成は本家の test_selector_generation の8症例＋id 短縮2症例を css/xpath・full/短縮の全組合せと往復選択で再現した。

## 段階2 adaptive（2026-11-02、Windows PC上の WSL2）

**互換性検証:** 本家固定コミットから生成した `fixtures/stage2_*.json` と `fixtures/oracle_storage.db` を使用。WSL Ubuntu 24.04、Rust 1.99.1、libxml2 2.9.14 を利用し、`cargo test -p rustscrapling-adaptive -p rustscrapling-cli --offline` で、Unicode文字列・タグ列・autojunk境界を含む `SequenceMatcher` 10,000件×3種が Python の `difflib` と **30,000/30,000件完全一致**。独立した Python 3.12 の `round(x, 2)` 正解データ **8,003/8,003件一致**。正解データの `element_to_dict`、`find_similar`、`find_by_text`、`find_by_regex`、スコア、`relocate(percentage=0,40)`、登録可能ドメイン、Python本家の SQLite BLOB 読込を照合。CLIの `select` / `similar` / `relocate` / `select --adaptive` を本家DBで実行し、さらに `--auto-save` でRustが保存したSQLite BLOBの読み戻し（BLOB/TEXT両対応）も通過。`python oracle/verify_stage2.py` による本家側からRust生成DBへの逆方向読込・JSONバイト単位の一致・再特定にも成功。**注意:** ローカルの別途インストール済み Python Scrapling は 0.4.9 であり、計画が要求する0.5固定コミットと同じものではない。追加の `round` 正解データはCPython標準関数だけを使って生成し、既存のScrapling正解データは再生成していない。

**性能測定条件:** AMD Ryzen 7 7735HS（8コア16スレッド）、WSL2 Ubuntu 24.04、メモリ7.8GiB、Rust 1.99.1。`[profile.release] lto = true`、`CARGO_BUILD_JOBS=2`、Rayonは既定スレッド数、libxml2互換DOM、付録Aの2000商品・約416KB合成ページを使用。`cargo run --release -p rustscrapling-adaptive --example bench_adaptive --offline`。`std::time::Instant` による各1回ウォームアップ後の best-of-3（relocate）、best-of-5（検索）、HTML解析時間は除外。共有Windowsホスト上の他のCargoビルドと負荷が重なったため変動があり、Criterionによる負荷を抑えた再測定は未実施。段階1のMuse VMおよび本家基準値との**同一環境・同一日の直接比較ではない**。

| 処理 | 今回の実測（ms） | 段階2目標 | 判定 |
|---|---|---|---|
| `relocate`（最初のreleaseビルド直後） | 111.25 | ≤50 | 未達 |
| `find_by_text`（同上） | 8.03 | ≤5 | 未達 |
| `find_similar`（同上） | 12.65 | ≤3 | 未達 |
| `relocate`（同一バイナリ再実行3回） | 85.50 / 111.94 / 88.86 | ≤50 | 未達 |
| `find_by_text`（同上） | 2.42 / 3.87 / 2.57 | ≤5 | 各回達成 |
| `find_similar`（同上） | 11.01 / 12.06 / 9.77 | ≤3 | 未達 |
| `relocate`（同一兄弟タグの厳密スコア最適化後、1回） | 49.53 | ≤50 | 単回達成、再測定要 |
| `find_by_text`（同上） | 7.01 | ≤5 | 未達 |
| `find_similar`（同上） | 15.09 | ≤3 | 未達 |
| `relocate`（短い入力インデックス＋属性類似度メモ化後のrelease再ビルド直後） | 51.73 | ≤50 | 単回未達 |
| `find_by_text`（同上） | 6.66 | ≤5 | 未達 |
| `find_similar`（同上） | 8.49 | ≤3 | 未達 |
| `relocate`（最終releaseバイナリ再実行3回） | 33.66 / 35.83 / 33.46 | ≤50 | 各回達成 |
| `find_by_text`（同上） | 5.85 / 6.28 / 3.23 | ≤5 | 1/3回達成 |
| `find_similar`（同上） | 9.38 / 6.12 / 6.65 | ≤3 | 未達 |

最終実装は `relocate` を追加実測3回とも50ms以下に収めた。ただし、共有ホストの負荷変動があるため、管理された専用環境での Criterion 計測による確認が残る。`find_by_text` は3回中2回、`find_similar` は3回とも段階2の目標未達。特に類似検索は `str_ratio` の小配列用インデックスと属性値の計算結果メモ化後でも目標と差がある。対象選択クレートの速度改善は段階2担当範囲外であり、この結果を未達として記録する。

# RESULTS.md: 測定と検証の結果

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

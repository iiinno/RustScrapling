# Plan.md第2版 対応状況（2026-11-02）

この表は実装と検証の証跡を区別した最新のチェックリスト。クレートが存在することを「機能完成」と扱わず、各段階の **Plan.md 10章の完了条件**に照らして判定する。

| 段階 | 実装した内容 | 実測・検証済み | 主要な未達・未検証 | 結論 |
|---|---|---|---|---|
| 0 環境・評価 | 11クレート、Rust、libxml2、Chrome環境、依存評価 | Muse VM基準値。WSL Rust1.99.1とlibxml2-dev2.9.14で再ビルド。既存RUSTScrapling adaptive 6テスト通過（QA文書） | 他のRust移植の網羅的な機能試験、MITM方式の実装検証は未完 | 部分達成 |
| 1 DOM/選択 | html5ever、libxml2互換DOM、CSS、XPath 1.0、selector-gen | 固定oracleのCSS 59＋XPath 29＝88件が互換モードで一致。CSS選択0.74ms（Muse VM） | 既定html5パース16.5〜17.0msで15ms目標未達。完全なXPath仕様網羅は別途試験要 | 部分達成 |
| 2 Adaptive | CPython SequenceMatcher、指紋、類似/再特定、SQLite互換、CLI | 3×10,000件一致、丸め8,003件一致、本家SQLite双方向読み書きとCLI試験成功。再特定33.46〜35.83ms（WSL） | find_similarは6.12〜9.38msで3ms目標未達。find_by_textは5ms超の試行あり。Criterion基準計測未実施 | 部分達成 |
| 3 HTTP/抽出 | wreq + wreq-util、QUIC HTTP/3経路、proxy、retry、body上限、htmd、無害化、JSON-LD/OG、BM25 | Extract単体・fixture計5テストPASSとの担当報告。HTTP単体・ローカルTLS/QUICの試験を別途実施 | 本家FetcherとのURL成功率比較と本家main_content_onlyの語95%条件未実施 | 部分達成 |
| 4 CDP browser | 低レベルCDP、DOM/Shadow DOM、ページ再利用、Cookie状態、Networkイベント、外部接続 | Windows Chromeで1 unit＋1実ブラウザE2E PASS。D-007に沿い明示evaluate以外ではRuntime.enableなしを確認 | CAPTCHA/Cloudflare solver、DoH、広告ブロック全件、認証proxy、network body記録など未完。実サイト成功率比較なし | 部分達成 |
| 5 Auto/Daemon | HTTP→browser→headed tier、ブロック検知、24h tier学習 | libraryの自動tier/期限切れなどのテスト | 持続デーモン、共有状態/ETag/同一URLの集約、150URL×8性能とメモリ未検証 | 部分達成 |
| 6 Crawl | TOML指定、同一URL指紋重複排除、bounded queue、並列、robots.txt、checkpoint、NDJSON | WSL unit 3件PASS（robots、重複排除・再開、URL拒否） | Plan7.3のCrawl/Sitemap/XMLFeed/CSVFeed/Shopify/Content全テンプレート、クラッシュ後のexactly-once保証、AutoThrottle等未完 | 部分達成 |
| 7 MCP | rmcpによるサーバ実装・本家ツール互換を開発中 | 完了判定前 | 本家`tests/ai/`との説明/引数・操作互換の全試験未完 | 未検証 |
| 8 仕上げ | ローカルHTTP CLI E2E、監査・回帰テスト、再現スクリプト | 各クレートの局所的テスト | 1時間の解析器＋セレクタファジング、24時間連続稼働、約200URL本家比較、150URL並列8で256MiB以内、全段階基準達成が未検証 | 未達 |

## 実装と計測の扱い

性能の値は `docs/RESULTS.md` の環境と試行条件があるものだけ引用する。上表の異なるPC/VMの値を相対比較して高速性の倍率を主張しない。Localhostや固定HTMLで成功したCDPは外部サイトのbot対策の突破保証にならない。標準出力の本文はHTML由来の不信なデータなので、可視の記事本文に含まれた指示まで自動的に信頼してはいけない。

今後、統合した各ブランチでの `cargo test --workspace --lib --tests --locked`、`tests/e2e_local.py`、ローカルChrome/HTTP3試験、Criterion本家比較の証跡を `docs/RESULTS.md` に追記し、未実施を「達成」に読み替えない。

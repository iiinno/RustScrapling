# Plan.md第2版 対応状況（2026-11-03）

この表は実装と検証の証跡を区別した最新のチェックリスト。クレートが存在することを「機能完成」と扱わず、各段階の **Plan.md 10章の完了条件**に照らして判定する。

| 段階 | 実装した内容 | 実測・検証済み | 主要な未達・未検証 | 結論 |
|---|---|---|---|---|
| 0 環境・評価 | 11クレート、Rust、libxml2、Chrome環境、依存評価 | Muse VM基準値。WSL Rust1.99.1とlibxml2-dev2.9.14で再ビルド。既存RUSTScrapling adaptive 6テスト通過（QA文書） | 他のRust移植の網羅的な機能試験、MITM方式の実装検証は未完 | 部分達成 |
| 1 DOM/選択 | html5ever、libxml2互換DOM、CSS、XPath 1.0、selector-gen | 固定oracleのCSS 59＋XPath 29＝88件が互換モードで一致。CSS選択0.74ms（Muse VM） | 既定html5パース16.5〜17.0msで15ms目標未達。完全なXPath仕様網羅は別途試験要 | 部分達成 |
| 2 Adaptive | CPython SequenceMatcher、指紋、類似/再特定、SQLite互換、CLI | 3×10,000件一致、丸め8,003件一致、本家SQLite双方向読み書きとCLI試験成功。再特定33.46〜35.83ms（WSL） | find_similarは6.12〜9.38msで3ms目標未達。find_by_textは5ms超の試行あり。Criterion基準計測未実施 | 部分達成 |
| 3 HTTP/抽出 | wreq + wreq-util、QUIC HTTP/3経路、proxy、retry、body上限、htmd、無害化、JSON-LD/OG、BM25 | Extract5件とHTTP/TLS/QUIC8件PASS、実HTTP/3接続とCLI GET/POST/Markdownも確認 | 本家Fetcherとの同一条件URL成功率比較とmain_content_onlyの語95%条件未実施 | 部分達成 |
| 4 CDP browser | 低レベルCDP、DOM/Shadow DOM、ページ再利用、Cookie状態、Networkイベント、外部接続 | Windows Chromeで1 unit＋1実ブラウザE2E PASS。D-007に沿い明示evaluate以外ではRuntime.enableなしを確認 | CAPTCHA/Cloudflare solver、DoH、広告ブロック全件、認証proxy、network body記録など未完。実サイト成功率比較なし | 部分達成 |
| 5 Auto/Daemon | HTTP→browser→headed tier、ブロック検知、24h学習、認証IPC、daemon start/stop/status、同一URL取得集約、ETag/Last-Modified、レート制限 | daemon計4件、CLI daemon計2件PASS。localhost 150ページ×並列8: 0.79秒、最大RSS24.99MiB | 学習永続化、同条件での本家比較、24h連続試験未達 | 部分達成 |
| 6 Crawl | TOML指定、同一URL指紋重複排除、bounded queue、並列、robots.txt、checkpoint。6種のテンプレートとNDJSON/JSON/CSV/XML出力を追加 | WSLのcrawlerテスト6件PASS、CLIの出力形式テストPASS | Python Spiderの全ミドルウェア互換、クラッシュ後のexactly-once保証、AutoThrottle等未完。全テンプレートの本家比較未実施 | 部分達成 |
| 7 MCP | rmcp stdio、本家16ツール/137引数、実HTTP・ブラウザ/CDP取得、セッション・本文抽出、未対応エラー | tool catalog2件、stdio1件、HTTP/CDPバックエンド3件PASS | 本家の全行動互換、screenshot・完全なbrowser state・全アクション未達 | 部分達成 |
| 8 仕上げ | ローカルCLI E2E、監査・回帰、CI・再現スクリプト、150ページ負荷試験 | WSL workspace68件PASS、CLI E2E6件PASS、固定150ページ×8で最大RSS24.99MiB | 1時間ファジング、24時間連続稼働、約200URL本家比較、実サイト成功率/速度/メモリ比較未達 | 未達 |

## 実装と計測の扱い

性能の値は `docs/RESULTS.md` の環境と試行条件があるものだけ引用する。上表の異なるPC/VMの値を相対比較して高速性の倍率を主張しない。Localhostや固定HTMLで成功したCDPは外部サイトのbot対策の突破保証にならない。標準出力の本文はHTML由来の不信なデータなので、可視の記事本文に含まれた指示まで自動的に信頼してはいけない。

今後、統合した各ブランチでの `cargo test --workspace --lib --tests --locked`、`tests/e2e_local.py`、ローカルChrome/HTTP3試験、Criterion本家比較の証跡を `docs/RESULTS.md` に追記し、未実施を「達成」に読み替えない。

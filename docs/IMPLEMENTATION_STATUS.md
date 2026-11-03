# Plan.md第2版 対応状況（2026-11-03）

この表は実装と検証の証跡を区別した最新のチェックリスト。クレートが存在することを「機能完成」と扱わず、各段階の **Plan.md 10章の完了条件**に照らして判定する。

| 段階 | 実装した内容 | 実測・検証済み | 主要な未達・未検証 | 結論 |
|---|---|---|---|---|
| 0 環境・評価 | 11クレート、Rust、libxml2、Chrome環境、依存評価 | Muse VM基準値。WSL Rust1.99.1とlibxml2-dev2.9.14で再ビルド。既存RUSTScrapling adaptive 6テスト通過（QA文書） | 他のRust移植の網羅的な機能試験、MITM方式の実装検証は未完 | 部分達成 |
| 1 DOM/選択 | html5ever、libxml2互換DOM、CSS、XPath 1.0、selector-gen | 固定oracleのCSS 59＋XPath 29＝88件が互換モードで一致。CSS選択0.74ms（Muse VM） | 既定html5パース16.5〜17.0msで15ms目標未達。完全なXPath仕様網羅は別途試験要 | 部分達成 |
| 2 Adaptive | CPython SequenceMatcher、指紋、類似/再特定、SQLite互換、CLI | 3×10,000件一致、丸め8,003件一致、本家SQLite双方向読み書きとCLI試験成功。再特定33.46〜35.83ms（WSL） | find_similarは6.12〜9.38msで3ms目標未達。find_by_textは5ms超の試行あり。Criterion基準計測未実施 | 部分達成 |
| 3 HTTP/抽出 | wreq + wreq-util、QUIC HTTP/3経路、proxy、retry、body上限、htmd、無害化、JSON-LD/OG、BM25。認証情報入りURLの拒否とcredential秘匿、認証情報付きLocationへのリダイレクト拒否を追加 | VPS HTTP/QUIC/TLSテストと新規credential redirectガードPASS（`2713e56`）、CLI GET/POST/Markdownも確認 | 本家Fetcherとの同一条件URL成功率比較とmain_content_onlyの語95%条件未実施 | 部分達成 |
| 4 CDP browser | CDP、DOM/Shadow DOM、ページ再利用、Cookie/localStorage状態、外部接続。`Page.navigate`を単一期限に統一。XHR/Fetchテキスト本文の同一session/requestIdでの有界記録（128件・64KiB/件・合計1MiB）、機密情報の一部除外。CSS click・focus/UTF-8追加入力・限定キー操作のCDP API | VPS mock試験とChrome155/156でそれぞれlocalhost HTTP E2E1/1、追加の実機`data:`入力/評価とlocalhost XHR本文を3/3ずつPASS。テスト専用basic password storeを使用 | 本番Launcherでのlocalhost障害、実サイトbot対策、ARIA/iframes、CAPTCHA/Cloudflare solver、DoH、広告ブロック全件、認証proxy、本家実サイト比較が未達 | 部分達成 |
| 5 Auto/Daemon | HTTP→browser→headed tier、ブロック検知、24h学習、認証IPC、daemon start/stop/status、同一URL取得集約、ETag/Last-Modified、レート制限。SQLiteで学習tier・条件付きGETキャッシュを復元。CLI subprocess起動・異常終了の型付きエラー強化 | VPSでdaemon `--features fetch-integration` テスト7件とCLI daemon4件（再起動後の304/学習復元、破損DB、認証・state漏えいを含む）PASS | 同条件での本家比較、24h連続試験未達 | 部分達成 |
| 6 Crawl | TOML、bounded並列queue、robots、checkpoint、6テンプレート・4出力、AutoThrottle、Crawl-delay/Request-rate、Retry-After秒数/date、private開発キャッシュ、allow/deny URL regex。`SpiderParser` asyncカスタムparseからGET-only次Request/JSON Itemをemitし既存安全frontierへ接続 | VPS Spider30/30とstrict Clippy PASS、全workspace113件PASS（`c6434bb`）。相対URL/リダイレクト/上限/重複/旧checkpoint互換を回帰試験 | Python本家の任意method/body/header付きRequestやper-request callback、LinkExtractor全規則、複数セッション、exactly-once保証・全テンプレート差分/実サイト比較が未達 | 部分達成 |
| 7 MCP | rmcp stdio、本家16ツール名・schema、実HTTP/CDP、session、Cookie/トップレベルlocalStorage状態v2、PNG/JPEG、CDP応答本文の有界MCP公開（128件、64KiB/件、1MiB、単調ID）、CSS click/テキスト追加入力・native input/textareaの上書き/限定キー・wait操作 | 全workspace113件PASS（`c6434bb`、MCP mock CDP・70 navigation Network試験を含む）。Browser基盤のChrome155/156実機入力・XHR本文試験3/3ずつPASS、native input上書きは`4d1ed34` Chrome155実機1/1 PASS | CLI daemonの永続ブラウザsession、MCP実Chrome経由の全アクション、IndexedDB/他フレーム、ARIA/selector/ref screenshot、Python本家12アクション完全対応が未達 | 部分達成 |
| 8 仕上げ | ローカルCLI E2E、監査・回帰、CI・再現スクリプト、150ページ負荷試験 | VPS `4d1ed34`全workspace113件PASS/0 FAIL、Chrome HTTP実機2件明示除外、実Chrome3件ignored。Chrome155/156実機を個別実行してそれぞれ3/3 PASS（`73858e0`）、localhost HTTP browserテスト各1/1 PASS（`527a1ef`）、native textbox上書き実機それぞれ1/1 PASS。全workspace strict Clippy・fmtともexit0（`4d1ed34`）。以前のCLI E2E6件、localhost150ページ×8は0.261秒/最大RSS24.46MiB/再開0.011秒 | MCP側Chrome HTTP実機未解決、1h fuzz、24h耐久、約200URL本家比較、実サイト成功率/速度/メモリ再測定未達 | 未達 |

## 実装と計測の扱い

性能の値は `docs/RESULTS.md` の環境と試行条件があるものだけ引用する。上表の異なるPC/VMの値を相対比較して高速性の倍率を主張しない。Localhostや固定HTMLで成功したCDPは外部サイトのbot対策の突破保証にならない。標準出力の本文はHTML由来の不信なデータなので、可視の記事本文に含まれた指示まで自動的に信頼してはいけない。

今後、統合した各ブランチでの `cargo test --workspace --lib --tests --locked`、`tests/e2e_local.py`、ローカルChrome/HTTP3試験、Criterion本家比較の証跡を `docs/RESULTS.md` に追記し、未実施を「達成」に読み替えない。

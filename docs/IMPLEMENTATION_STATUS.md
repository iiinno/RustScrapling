# Plan.md第2版 対応状況（2026-11-03）

この表は実装と検証の証跡を区別した最新のチェックリスト。クレートが存在することを「機能完成」と扱わず、各段階の **Plan.md 10章の完了条件**に照らして判定する。

| 段階 | 実装した内容 | 実測・検証済み | 主要な未達・未検証 | 結論 |
|---|---|---|---|---|
| 0 環境・評価 | 11クレート、Rust、libxml2、Chrome環境、依存評価 | Muse VM基準値。WSL Rust1.99.1とlibxml2-dev2.9.14で再ビルド。既存RUSTScrapling adaptive 6テスト通過（QA文書） | 他のRust移植の網羅的な機能試験、MITM方式の実装検証は未完 | 部分達成 |
| 1 DOM/選択 | html5ever、libxml2互換DOM、CSS、XPath 1.0、selector-gen | 固定oracleのCSS 59＋XPath 29＝88件が互換モードで一致。CSS選択0.74ms（Muse VM） | 既定html5パース16.5〜17.0msで15ms目標未達。完全なXPath仕様網羅は別途試験要 | 部分達成 |
| 2 Adaptive | CPython SequenceMatcher、指紋、類似/再特定、SQLite互換、CLI | 3×10,000件一致、丸め8,003件一致、本家SQLite双方向読み書きとCLI試験成功。再特定33.46〜35.83ms（WSL） | find_similarは6.12〜9.38msで3ms目標未達。find_by_textは5ms超の試行あり。Criterion基準計測未実施 | 部分達成 |
| 3 HTTP/抽出 | wreq + wreq-util、QUIC HTTP/3経路、proxy、retry、body上限、htmd、無害化、JSON-LD/OG、BM25。認証情報入りURLの拒否・再試行上限、抽出HTML/Markdown/正規URLのcredential秘匿を追加 | Extract6件とHTTP/TLS/QUIC回帰テストPASS、実HTTP/3接続とCLI GET/POST/Markdownも確認 | 本家Fetcherとの同一条件URL成功率比較とmain_content_onlyの語95%条件未実施 | 部分達成 |
| 4 CDP browser | 低レベルCDP、DOM/Shadow DOM、ページ再利用、Cookie状態、Networkイベント、外部接続。CDPのコマンド名/ID限定トレース、Chrome終了時の一時プロファイル削除競合に有界リトライを追加。`Page.navigate`のコマンド応答とロード待ちを単一期限に統一 | 以前のWindows Chromeで1 unit＋1実ブラウザE2E PASS。VPSで無応答CDPモック期限テスト1件PASS。VPSのChrome for Testing 156では実CDP data:ページのDOM/PNG/明示evaluateが1件PASS。Chrome155/156双方ともlocalhost HTTPへの遷移はタイムアウト | localhost HTTPを含む実機E2Eの安定化、CAPTCHA/Cloudflare solver、DoH、広告ブロック全件、認証proxy、network body記録、実サイト成功率比較が未達 | 部分達成 |
| 5 Auto/Daemon | HTTP→browser→headed tier、ブロック検知、24h学習、認証IPC、daemon start/stop/status、同一URL取得集約、ETag/Last-Modified、レート制限。SQLiteで学習tier・条件付きGETキャッシュを復元。CLI subprocess起動・異常終了の型付きエラー強化 | VPSでdaemon `--features fetch-integration` テスト7件とCLI daemon4件（再起動後の304/学習復元、破損DB、認証・state漏えいを含む）PASS | 同条件での本家比較、24h連続試験未達 | 部分達成 |
| 6 Crawl | TOML指定、URL指紋重複排除、bounded queue、並列、robots.txt、checkpoint、6種テンプレートと4出力形式。HTML base、ホスト別速度制限。対象外リンク/重複シードによる取りこぼしを修正。チェックポイントはUnixの0600/0700とsymlink拒否・旧権限移行。オプトインAutoThrottle、robotsのCrawl-delay/Request-rate、Retry-After秒数/HTTP-date、開発用privateレスポンスキャッシュを実装 | VPS Ubuntuでcrawler25件PASS（429並列待機、robots速度制限、開発キャッシュのoffline再処理・破損拒否・私有権限を含む）、`51e3d6c`時点の全workspace95件PASS | Python Spiderのカスタムparse/RequestとLinkExtractor規則、複数セッション、exactly-once保証、本家と全テンプレート差分・外部サイト性能比較が未達 | 部分達成 |
| 7 MCP | rmcp stdio、本家16ツール/137引数、実HTTP・ブラウザ/CDP取得、セッション・本文抽出。Cookie＋現行トップレベルoriginのlocalStorage(v2、旧v1読込可能)とCDP PNG/JPEG screenshotを実装 | tool catalog2件、stdio1件、HTTP/CDPバックエンド3件とmock CDP 1件PASS（v2保存復元、v1互換、origin不一致拒否を含む） | 実Chromeでの新機能E2E未実施。IndexedDB、他フレーム、selector/ref screenshot、ARIA、全アクション/本家互換未達 | 部分達成 |
| 8 仕上げ | ローカルCLI E2E、監査・回帰、CI・再現スクリプト、150ページ負荷試験 | VPS Ubuntu Rust1.99.1でChrome HTTP実機2件を明示的に除外した全workspace95件PASS・終了コード0（`51e3d6c`時点）。全workspace・全target strict Clippy PASS（同版）、その後のHTTP-date追加版はSpider25件PASS。VPS CLI E2E6件PASS、localhost150ページ×8は0.261秒/最大RSS24.46MiB/再開0.011秒 | VPS Chrome localhost HTTP実機E2E未達。1時間ファジング、24時間連続稼働、約200URL本家比較、実サイト成功率/速度/メモリ比較未達 | 未達 |

## 実装と計測の扱い

性能の値は `docs/RESULTS.md` の環境と試行条件があるものだけ引用する。上表の異なるPC/VMの値を相対比較して高速性の倍率を主張しない。Localhostや固定HTMLで成功したCDPは外部サイトのbot対策の突破保証にならない。標準出力の本文はHTML由来の不信なデータなので、可視の記事本文に含まれた指示まで自動的に信頼してはいけない。

今後、統合した各ブランチでの `cargo test --workspace --lib --tests --locked`、`tests/e2e_local.py`、ローカルChrome/HTTP3試験、Criterion本家比較の証跡を `docs/RESULTS.md` に追記し、未実施を「達成」に読み替えない。

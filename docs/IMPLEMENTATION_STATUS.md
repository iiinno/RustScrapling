# Plan.md 対応状況（2026-11-03、後継セッションで6・7行を更新）

この表は実装と検証の証跡を区別した最新のチェックリスト。クレートが存在することを「機能完成」と扱わず、各段階の **Plan.md 10章の完了条件**に照らして判定する。

| 段階 | 実装した内容 | 実測・検証済み | 主要な未達・未検証 | 結論 |
|---|---|---|---|---|
| 0 環境・評価 | 11クレート、Rust、libxml2、Chrome環境、依存評価 | Muse VM基準値。WSL Rust1.99.1とlibxml2-dev2.9.14で再ビルド。既存RUSTScrapling adaptive 6テスト通過（QA文書） | 他のRust移植の網羅的な機能試験、MITM方式の実装検証は未完 | 部分達成 |
| 1 DOM/選択 | html5ever、libxml2互換DOM、CSS、XPath 1.0、selector-gen | 固定oracleのCSS 59＋XPath 29＝88件が互換モードで一致。CSS選択0.74ms（Muse VM） | 既定html5パース16.5〜17.0msで15ms目標未達。完全なXPath仕様網羅は別途試験要 | 部分達成 |
| 2 Adaptive | CPython SequenceMatcher、指紋、類似/再特定、SQLite互換、CLI | 3×10,000件一致、丸め8,003件一致、本家SQLite双方向読み書きとCLI試験成功。再特定33.46〜35.83ms（WSL） | find_similarは6.12〜9.38msで3ms目標未達。find_by_textは5ms超の試行あり。Criterion基準計測未実施 | 部分達成 |
| 3 HTTP/抽出 | wreq + wreq-util、QUIC HTTP/3経路、proxy、retry、body上限、htmd、無害化、JSON-LD/OG、BM25。認証情報入りURLの拒否・再試行上限、抽出HTML/Markdown/正規URLのcredential秘匿を追加 | Extract6件とHTTP/TLS/QUIC回帰テストPASS、実HTTP/3接続とCLI GET/POST/Markdownも確認 | 本家Fetcherとの同一条件URL成功率比較とmain_content_onlyの語95%条件未実施 | 部分達成 |
| 4 CDP browser | 低レベルCDP、DOM/Shadow DOM、ページ再利用、Cookie状態、外部接続。`Page.navigate`を単一期限に統一。XHR/Fetch JSON/テキストのNetwork本文を同一page session/requestIdで結合して有界記録（128件・64KiB/件・合計1MiB）、機密情報を除外 | VPSでCDP mock通信本文試験を含む全workspace104件PASS。Chrome for Testing 156ではdata:ページ実機試験1件PASS。Chrome155/156双方ともlocalhost HTTPへの遷移はタイムアウト | localhost HTTP実機E2E、Network本文の実Chrome/実サイト検証、CAPTCHA/Cloudflare solver、DoH、広告ブロック全件、認証proxy、実サイト成功率比較が未達 | 部分達成 |
| 5 Auto/Daemon | HTTP→browser→headed tier、ブロック検知、24h学習、認証IPC、daemon start/stop/status、同一URL取得集約、ETag/Last-Modified、レート制限。SQLiteで学習tier・条件付きGETキャッシュを復元。CLI subprocess起動・異常終了の型付きエラー強化 | VPSでdaemon `--features fetch-integration` テスト7件とCLI daemon4件（再起動後の304/学習復元、破損DB、認証・state漏えいを含む）PASS | 同条件での本家比較、24h連続試験未達 | 部分達成 |
| 6 Crawl | TOML、bounded並列queue、robots、checkpoint、6テンプレート・4出力、AutoThrottle、Crawl-delay/Request-rate、Retry-After、private開発キャッシュ、allow/deny URL regex。`SpiderParser` async parse。**S-03部分**：SpiderRequestのmethod/body/headers/priority、request-aware dedupe、安定priority FIFO、checkpoint後方互換、機密checkpoint既定拒否、frontier 32MiB上限（`7752546`で統合） | 統合後の全workspace 127件PASS（`8bd7146`、Spider 39件含む、クラウドLinux） | cookies/session/metadata/callback/errback、CLI経由rich request、LinkExtractor全規則、複数セッション、exactly-once/at-least-once契約の実証、本家全テンプレート差分・外部比較が未達 | 部分達成 |
| 7 MCP | rmcp stdio、本家16ツールの**live `tools/list`と完全一致するカタログ**（input/outputSchema・annotations）、`$ref`対応バリデータ、structured出力のoutputSchema検証。実HTTP/CDP、session、Cookie/localStorage状態、PNG/JPEG・要素/ref screenshot、browser_actions全12種、**本家同一のARIA snapshot/`aria-ref`**、snapshot検索、**本家Convertor互換のmarkdown/html/text**（css_selector・main_content_only=false含む）、evaluate（arg・isolated_context両対応、`Runtime.enable`不要）、wait_selector/network_idle | schema oracle差分0（`51ed85a`）、Convertor 240/240、ARIA snapshot 6/6一致、全workspace 134 PASS、実Chrome E2E（ローカルChrome 153とGitHub runner Chrome） | Network記録の本家同等範囲（全種別・request headers/body・1000件/20MiB）、ブラウザ起動/取得オプション多数（cookies・extra_headers・locale・timezone・useragent・blocked_domains・disable_resources・google_search・proxy dict・cdp_url等）、iframe内snapshot/操作、IndexedDB状態、HTTP auth/proxy_auth/follow_redirects=safe、HTTP transport、Chrome155/156再検証 | 部分達成 |
| 8 仕上げ | ローカルCLI E2E、監査・回帰、CI・再現スクリプト、150ページ負荷試験 | VPS Ubuntu Rust1.99.1で`22a7d60`全workspace104件PASS、0件FAIL、Chrome HTTP実機2件明示除外、data:実機1件ignored。全workspace/all-targets strict Clippyとfmtチェックとも終了コード0。以前のVPS CLI E2E6件PASS、localhost150ページ×8は0.261秒/最大RSS24.46MiB/再開0.011秒 | VPS Chrome localhost HTTP実機E2E未達。1時間fuzz、24時間耐久、約200URL本家比較、実サイト成功率/速度/メモリ比較未達 | 未達 |

## 実装と計測の扱い

性能の値は `docs/RESULTS.md` の環境と試行条件があるものだけ引用する。上表の異なるPC/VMの値を相対比較して高速性の倍率を主張しない。Localhostや固定HTMLで成功したCDPは外部サイトのbot対策の突破保証にならない。標準出力の本文はHTML由来の不信なデータなので、可視の記事本文に含まれた指示まで自動的に信頼してはいけない。

今後、統合した各ブランチでの `cargo test --workspace --lib --tests --locked`、`tests/e2e_local.py`、ローカルChrome/HTTP3試験、Criterion本家比較の証跡を `docs/RESULTS.md` に追記し、未実施を「達成」に読み替えない。

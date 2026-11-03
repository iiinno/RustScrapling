# ScraplingのRust再実装: 先行実装の参照と採用状況

2026-11-03時点。**仕様の正解はPython版Scrapling v0.5**。既存Rust移植に実装があっても、本家との挙動一致を証明せずに「互換」とは扱わない。

| 参照元 | 確認した版 | 採用する設計・知見 | 本プロジェクトで補うこと |
|---|---|---|---|
| [D4Vinci/Scrapling](https://github.com/D4Vinci/Scrapling) | `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`（固定正解データ） | PythonのAPI・挙動・Spider/MCPの試験・完成条件の基準 | Rust独自の性能・単一バイナリ・AI向けデーモンも追加 |
| [Liohtml/RUSTScrapling](https://github.com/Liohtml/RUSTScrapling) | `2dd9a5eb0627fdb7b5c0aae7b5fc4dc9e8cc092c` | SequenceMatcher試験、robotsグループ、Crawl-delay、AutoThrottle、開発用レスポンスキャッシュ、スケジューラの考え方 | HTTPブラウザ偽装、実CDP、MCP、Python v0.5の全Spiderテンプレート・引数 |
| [chrisabruce/scrapling-rs](https://github.com/chrisabruce/scrapling-rs) | `0d61c3e88d95473d77d90d3e374ffded79de07da` | wreq、Playwright系の起動設定、Cloudflare処理フロー、MCP、PyO3の参考 | `Runtime.enable`を不要にするCDP経路、Python v0.5のAPI/Spider互換、再現性のある検証 |

現行RustScraplingには、これらを参考にして以下を実装済み。ただし直接コードを複製するのではなく、RustScraplingの既存アーキテクチャに合わせて実装・試験している。

- robots.txt: RustScrapling用とワイルドカード用の規則を分離し、複数グループ、Crawl-delay、Request-rate、URLのpath+queryを処理。robots指示は固定`delay_ms`/AutoThrottleの下限となる
- AutoThrottle: 429/503・通信失敗時のバックオフと、秒数/HTTP-date形式のRetry-After。待機中の他リクエストも新しい期限を再評価
- 開発キャッシュ: オプトイン、URLのハッシュ名、保存ファイルの私有権限、2xxのみ保存、チャレンジHTMLの簡易除外、破損検出、同一ページの再処理をサポート
- Spiderの宣言的`link_allow`/`link_deny` URL正規表現で、対象外リンクをfrontier割当て前に除外する（Python LinkExtractorの部分実装）
- CDP `Network.responseReceived/loadingFinished/getResponseBody` による有界テキストAPI本文収集。XHR/Fetchのみ、データ上限と機密情報の除外あり。MCPからも`browser_network_request(part="response_body")`で伏せ字済本文を参照可能（mock E2E、実Chromeの基盤E2EがPASS）
- Spider `crawl_with_parser`からasync JSON ItemとGET-only追加Requestを同じfrontierへ渡す。MCP `browser_actions`にはCSS単発click/追加入力・native input/textarea上書き/限定キー/待機の実CDP動作を追加

ライセンス: Python本家はBSD-3-Clause、RUSTScraplingはMIT OR Apache-2.0、scrapling-rsはMIT。RUSTScraplingのMIT著作権表示を `licenses/RUSTScrapling-MIT.txt` に残し、`NOTICE`で出典を明記する。取り込んだコード・アルゴリズムは検証とライセンスレビューを伴う。

## 「完全な再実装」に向けた未達

最も重要なのは、**Pythonの利用者が実際に利用できる機能に相当するRust API/CLI/MCP経路**があり、モックではなく適切な実機E2Eと本家比較が通ること。

- Browser: VPSのChrome155/156はテスト専用プロファイル＋`--password-store=basic`でlocalhost HTTP/CDPが動作（各1/1 PASS）。本番launcherへ適用すると資格情報の保存方式を弱めるため無条件適用不可。CDP本文/XHR/基本入力は実Chromeのlocalhost/データURLで検証したが、Cloudflare/実ログイン・Bot対策サイト比較、iframe/ARIA、全入力操作は未達
- Spider: async parse/GET-only Request/Itemの仕組みは追加。PythonのPOST/headers/body/cookies等のリッチなRequest/per-request callback、LinkExtractorの全規則（allow/deny URL regexのみ）、複数セッション、正確なblocked retry、exactly-once相当のクラッシュ復旧は未達
- MCP/CLI: 16ツール名を登録済みでも全引数の動作が本家と同じとは限らない。MCP network本文や一部のCDP操作、ネイティブtextboxの既定clearは実装したが、ARIA/selector指定スクリーンショット、rich contenteditable、dialog/checkbox/radio/combobox等、本家全アクション、daemonの恒久ブラウザ保持とCLIセッション運用は未達
- 品質証拠: 約200URLの同日同一環境比較、ブラウザ取得成功率、1時間fuzz、24時間耐久、性能/メモリ目標の再測定を継続して行う

**設計上の補足:** Pythonのクラス構文そのものをRustへ移す必要はないが、Spiderの`parse`/追加Request/出力項目の柔軟性はRust traitまたは宣言的設定で同等に表現できる必要がある。Plan.mdの旧「Pythonクラスとして書くSpiderは対象外」は、機能そのものを対象外にする意味ではない。

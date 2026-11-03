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

ライセンス: Python本家はBSD-3-Clause、RUSTScraplingはMIT OR Apache-2.0、scrapling-rsはMIT。RUSTScraplingのMIT著作権表示を `licenses/RUSTScrapling-MIT.txt` に残し、`NOTICE`で出典を明記する。取り込んだコード・アルゴリズムは検証とライセンスレビューを伴う。

## 「完全な再実装」に向けた未達

最も重要なのは、**Pythonの利用者が実際に利用できる機能に相当するRust API/CLI/MCP経路**があり、モックではなく適切な実機E2Eと本家比較が通ること。

- Browser: VPSのlocalhost HTTP実機Chromeで遷移が安定しない。通信本文記録、ブラウザ操作、実ログイン状態の復元、Bot対策の実サイト比較が未達
- Spider: Pythonのカスタムparse/Requestコールバック、LinkExtractorの規則、複数セッション、正確なblocked retry、全テンプレートの差分試験、exactly-once相当のクラッシュ復旧が未達
- MCP/CLI: 16ツール名を登録済みでも全引数の動作が本家と同じとは限らない。browser actions、ARIA/selector指定スクリーンショット、CLIセッション運用などが未達
- 品質証拠: 約200URLの同日同一環境比較、ブラウザ取得成功率、1時間fuzz、24時間耐久、性能/メモリ目標の再測定を継続して行う

**設計上の補足:** Pythonのクラス構文そのものをRustへ移す必要はないが、Spiderの`parse`/追加Request/出力項目の柔軟性はRust traitまたは宣言的設定で同等に表現できる必要がある。Plan.mdの旧「Pythonクラスとして書くSpiderは対象外」は、機能そのものを対象外にする意味ではない。

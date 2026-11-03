# GA Gate 台帳（Plan 3.1 / 12章）

Plan 3.1に従い各G項目を `PASS / FAIL / NOT_RUN / BLOCKED / PARTIAL` で記録する。`PARTIAL`は「一部の証跡はあるが合格条件の全体は未充足」を意味し、PASSではない。**現時点でGA（G-01〜G-13すべてPASS）は未達**。

最終更新：後継セッション、統合ブランチ `claude/laughing-clarke-xb55gw` HEAD `77e5339`（クラウドLinuxコンテナ＋GitHub Actions）。各行の証跡SHAは`docs/RESULTS.md`参照。

| ID | 状態 | 現在の証跡 | 不足（PASSに必要なもの） |
| --- | --- | --- | --- |
| G-01 本家公開機能100% verified | PARTIAL | M0棚卸し（196直接+130継承+32関数+10 CLI+16 MCP+12 action）。MCP `tools/list`本家と差分0、browser_actions全12種・ARIA snapshot（6/6）・Convertor（240/240）の差分試験、MCP各ツールのE2E | Inventory各行へのRust entrypoint/test ID紐付けと`verified`判定、Spider/CLI/Fetcher/Selector全API、残る未対応引数（認証付きbrowser proxy、solve_cloudflare、additional_args、network記録範囲） |
| G-02 CLI・daemon・MCP同一コアE2E | PARTIAL | daemonが`RustScraplingMcp`をホストし、CLI `session`で実ブラウザ/HTTP sessionをプロセス間で再利用（別プロセスE2E）、MCP stdio E2E、CLI E2E 6件、IPC負例、soakでdaemon再起動を反復 | 全公開操作・異常系の網羅表、同時実行ログ |
| G-03 Chrome155/156実機E2E | PARTIAL | `scripts/chrome_e2e_matrix.sh`：CfT **155.0.8059.84 / 156.0.8078.80**（`rustscrapling install`で導入）と153で browser 5/5・MCP 16/16・セッション再利用 headless/headed（Xvfb）・shell fetch 全PASS。headedのclick取りこぼし（約1/150）を発見しD-011で修正、修正後600ラウンド0件。CI `chrome-matrix`ジョブを追加 | 外部検証サイト（ボット検知ページ）での記録（このコンテナはTLS終端プロキシ経由のため不可、CIで実施予定）、CI `chrome-matrix`の緑 |
| G-04 約200URL成功率≥本家 | NOT_RUN | `bench/corpus.toml`（200件・8カテゴリ×25、機械的成功条件、ToS制限59件は`allowed_for_testing=false`）、`bench/compare_corpus.py`、CIワークフロー`g04-corpus.yml`（直接通信のrunnerで実行） | CIでの同日比較結果（static/browser、カテゴリ別）、制限付き59件を含めるかの所有者判断 |
| G-05 150URL並列8完了時間≤本家 | PASS（候補、localhost） | `29ae0df`同一マシン・交互5回・双方150/150：遅延0ms Rust 0.105s/本家0.687s、50ms 1.097s/1.692s、200ms 4.114s/4.665s（`bench/results/local_crawl_150x8_29ae0df.json`） | 実ネットワーク150URLでの比較（G-04 runner上）、daemon再利用条件、所有者のGA判定 |
| G-06 adaptive一致＋性能 | PASS（候補） | `0e30c70`で同一マシン同日：relocate 9.4–10.3ms、find_by_text ≤0.68ms、find_similar ≤1.31ms（本家4.1s/60ms/29ms）、既存oracle fixture全PASS（RESULTS.md） | fixture外入力でのスコア一致の追加検証、所有者によるGA判定 |
| G-07 本文抽出品質 | PARTIAL | MCPの`main_content_only`出力は本家Convertorとバイト一致（240/240）＝本家同等 | 注釈付き記事corpusでの語彙再現率・文字数比較（Rust独自抽出の評価を含む） |
| G-08 RSS≤256MiB（150×8） | PASS（候補、localhost） | 同上の計測でRustプロセスツリー最大RSS **18.0MiB**（本家53.9MiB）。Chromeは別計測 | 巨大HTML・実ネットワーク条件での追加計測 |
| G-09 1時間fuzz×4 | PASS（候補） | cargo-fuzz/libFuzzer＋ASan、各3601秒：html_parse 840,938回、selectors 4,723,863回、adaptive 707,655回、markdown 224,806回、**クラッシュ/メモリ違反/timeout 0件**（html_parse/selectors/markdown：HEAD `2a8cb7b`＋未コミットのCSS修正＝`a1dcefb`と同一コードの作業ツリーでbuild、adaptive：relocateデッドロック修正`caf5685`で再build。以後これらの対象クレートのfuzz対象関数は変更なし、`render_elements`追加のみ）。事前の短時間fuzzでCSS panicとrelocateデッドロックを発見・修正済み | 所有者のGA判定（UBSan等の追加は任意） |
| G-10 24h耐久 | NOT_RUN | `bench/soak.py`（daemon上のbrowser/HTTP session、state保存/読込、CLI get/shell、定期再起動、RSS/FD/プロセス/一時プロファイル）と1時間パイロット（RESULTS.md） | 24時間の実行とメトリクス |
| G-11 セキュリティ必須試験 | PARTIAL | `docs/THREAT_MODEL.md`（7.1の攻撃面→対策→回帰テスト）、IPC負例（未認証tool/fetch/stop、巨大・深いネスト・停止フレーム）、悪意ある状態ファイル負例、CDP endpoint allowlist（D-012）、`cargo audit --deny warnings` 0件（CI `supply-chain`） | 外部公開MCP HTTP transport（未実装）のSSRF/認証試験、残余リスクの解消または承認 |
| G-12 fmt/clippy/test/E2E/release smoke緑 | PARTIAL | `b6948c0`：全workspace 159 PASS・0 FAIL・8 ignored（実Chrome opt-in、別ジョブで実行）、strict clippy・fmt緑。`scripts/release_smoke.sh`をreleaseバイナリで実行しPASS（19サブコマンドhelp、E2E 6/6、SHA256記録）。CI `release-smoke`ジョブ追加 | 最終SHAでのCI全ジョブ緑、ignored 8件（実Chrome opt-in）の承認記録 |
| G-13 ドキュメント/NOTICE/SBOM/再現ビルド | PARTIAL | NOTICE/licenses、CycloneDX SBOM（398 components、ライセンス問題0）、CHANGELOG、README（install/shell/mcp/session）、release manifest＋SHA256SUMS | 新規環境での導入手順の第三者再現、再現ビルド（ビット同一性）検証 |

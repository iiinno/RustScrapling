# GA Gate 台帳（Plan 3.1 / 12章）

Plan 3.1に従い各G項目を `PASS / FAIL / NOT_RUN / BLOCKED / PARTIAL` で記録する。`PARTIAL`は「一部の証跡はあるが合格条件の全体は未充足」を意味し、PASSではない。**現時点でGA（G-01〜G-13すべてPASS）は未達**。

最終更新：後継セッション、統合ブランチ `claude/laughing-clarke-xb55gw` HEAD `0e30c70`（クラウドLinuxコンテナ＋GitHub Actions）。

| ID | 状態 | 現在の証跡 | 不足（PASSに必要なもの） |
| --- | --- | --- | --- |
| G-01 本家公開機能100% verified | PARTIAL | M0棚卸し（196直接+130継承+32関数+10 CLI+16 MCP+12 action）。MCP `tools/list`本家と差分0、browser_actions全12種・ARIA snapshot（6/6）・Convertor（240/240）の差分試験、MCP各ツールのE2E | Inventory各行へのRust entrypoint/test ID紐付けと`verified`判定、Spider/CLI/Fetcher/Selector全API、残る未対応引数（認証付きbrowser proxy、solve_cloudflare、additional_args、network記録範囲） |
| G-02 CLI・daemon・MCP同一コアE2E | PARTIAL | MCP stdio JSON-RPC E2E、CLI localhost E2E 6件、daemon既存テスト | CLI永続session（daemon上の実ブラウザ）、全公開操作・異常系の別プロセスE2E、再起動/同時実行ログ |
| G-03 Chrome155/156実機E2E | PARTIAL | Chrome 153（ローカル）とGitHub runnerのChromeで全action/snapshot/option E2E PASS | Chrome155/156での同一試験、外部検証サイト、headed、セッション再利用の安定性記録 |
| G-04 約200URL成功率≥本家 | NOT_RUN | — | `bench/corpus.toml`、同日同条件の本家/Rust比較スクリプトと結果 |
| G-05 150URL並列8完了時間≤本家 | NOT_RUN | 旧localhost 150×8記録のみ（本家比較なし） | 同一URL・同条件の本家比較、p50/p95、RSS |
| G-06 adaptive一致＋性能 | PASS（候補） | `0e30c70`で同一マシン同日：relocate 9.4–10.3ms、find_by_text ≤0.68ms、find_similar ≤1.31ms（本家4.1s/60ms/29ms）、既存oracle fixture全PASS（RESULTS.md） | fixture外入力でのスコア一致の追加検証、所有者によるGA判定 |
| G-07 本文抽出品質 | PARTIAL | MCPの`main_content_only`出力は本家Convertorとバイト一致（240/240）＝本家同等 | 注釈付き記事corpusでの語彙再現率・文字数比較（Rust独自抽出の評価を含む） |
| G-08 RSS≤256MiB（150×8） | NOT_RUN | — | process tree RSS計測 |
| G-09 1時間fuzz×4 | NOT_RUN | — | cargo-fuzz等のターゲットと1hログ |
| G-10 24h耐久 | NOT_RUN | — | 24hメトリクス |
| G-11 セキュリティ必須試験 | PARTIAL | 資格情報URL拒否、state path traversal拒否、network秘匿、safe redirect（内部IP拒否）等の負例テスト | threat model網羅表、IPC/状態ファイル/SSRFの全負例 |
| G-12 fmt/clippy/test/E2E/release smoke緑 | PARTIAL | `0e30c70`まで fmt・strict clippy・全workspace test・CLI E2E・実Chrome E2E（ローカル＋GitHub Actions）が緑 | リリースバイナリsmoke、意図的skipの承認記録 |
| G-13 ドキュメント/NOTICE/SBOM/再現ビルド | PARTIAL | NOTICEにPlaywright/markdownify/Scrapling等の出典追加 | SBOM、CHANGELOG、新規環境インストール再現、成果物checksum |

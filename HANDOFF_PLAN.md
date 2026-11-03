# HANDOFF_PLAN.md — 次の担当モデル向け引き継ぎ計画（2026-11-03）

**注：現行計画は `Plan.md` v4（最速実運用）。本書はGAゲート視点の旧引き継ぎ記録で、Plan.md と矛盾する場合は Plan.md に従う。**

## 0. 結論（現在地）

- **GA（G-01〜G-13全PASS）まではまだ遠い。** 最大の残課題は G-01（本家API 394項目の検証紐付け、348件未紐付け）と G-04（実サイト成功率で本家に負けている）と G-10（24h耐久未実施）。
- 機能面は大半が実装済み：MCP 16ツール/12アクション、CLI（extract/shell/install/mcp --http/session/daemon）、Spider、HTTP、ブラウザ。
- 残りは「単純作業」と「難しい判断」が混在。単純作業＝G-01の紐付けとテスト追加、24h soak実行。難しい＝G-04の成功率改善。

## 1. 最初にやること（順番厳守）

1. ブランチ`claude/laughing-clarke-xb55gw`を取得。HEADに**未検証のWIPコミット**がある（下記2）。まず検証する：
   ```bash
   CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 cargo test --workspace --locked --lib --tests
   CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings
   scripts/quick_checks.sh
   ```
   失敗したらWIPを直すか revert（`git revert <WIP SHA>`）。
2. Rustのpub fnを追加・移動したら**必ず** `python3 oracle/audit_parity.py` を実行してから`scripts/quick_checks.sh`。忘れるとCIの`upstream-parity`ジョブが落ちる（何度も踏んだ）。
3. **ディスク注意**：コンテナは数GBしか空かない。`target/debug/incremental`や古い`target/debug/deps`は削除可。`CARGO_INCREMENTAL=0`推奨。ディスク満杯中のテスト/soak結果は無効（実際に起きた）。
4. Cargo/RustビルドはWindows/WSLで実行しない（AGENTS.md）。

## 2. 未検証WIP（最後のコミット）

内容：
- `crates/rustscrapling-browser/src/lib.rs`：`BrowserGetOptions`に`settings: Option<PageSettings>`と`referrer`を追加（`Copy`派生を外した）。`get()`で既定ページ作成時に`apply_settings`、`navigate_with_referrer`使用。
- `crates/rustscrapling-cli/src/browser_adapter.rs`：CLIのCDP取得（fetch/auto/stealth/shell）に本家StealthyFetcher既定（Google referer、30秒、自前起動時のみHeadlessChrome UA置換と1920×1080）を適用。
- `bench/compare_corpus.py`：Rust側を`extract get/fetch URL out.txt`（MCP実行器経由＝本家等価経路）に変更、判定が分かれたURLを`disagreements`として出力。

ビルドは通過済み、**テストは中断で未完了**。

## 3. ゲート別の残作業

| ゲート | 状態 | 次の手順 |
| --- | --- | --- |
| G-01 | PARTIAL（394中 verified35/partial11/未紐付け348） | `docs/compat/EVIDENCE.json`に追記→`python3 oracle/check_evidence.py`。メソッド(Selector42, TextHandler30, Selectors16, Spider14…)と継承130。Rustに対応APIが無いもの（TextHandler.json/re_first等）は**Rustライブラリ関数を追加し本家オラクルで差分テスト**するか、`unsupported`と正直に記録。名前一致だけでverifiedにしない |
| G-02 | PARTIAL | 全公開操作×異常系の別プロセスE2E網羅表 |
| G-03 | PARTIAL | CI `chrome-matrix`（CfT155/156、headed含む）が`1ec8557`以降で緑か確認。外部検証サイトはCIで |
| G-04 | **FAIL**（CI実測 static: Rust 80.9% vs 本家 83.7%、browser: 72.3% vs 87.2%） | 下記「判断が難しい点 A」 |
| G-05/G-08 | PASS候補(localhost) | 実ネットワーク条件の追加計測 |
| G-06 | PASS候補 | — |
| G-07 | PARTIAL（自作8ページで条件達成） | ライセンス確認済みの実記事corpusで再評価 |
| G-09 | PASS候補（4×1h、0件） | — |
| G-10 | NOT_RUN | `bench/soak.py --duration 86400`。1hパイロットはディスク満杯で12件失敗＝無効、再実行要 |
| G-11 | PARTIAL | THREAT_MODEL.mdの残余（ブラウザ取得のサブリソース内部宛先、DNS再解決TOCTOU） |
| G-12/G-13 | PARTIAL | 最終SHAでCI全緑、ignored(実Chrome opt-in)承認記録、第三者再現手順 |

## 4. 判断が難しい点（事前判断）

**A. G-04の成功率改善**（最重要）
- 計測はこのコンテナでは不可（TLS終端プロキシ経由で指紋が無意味）。必ずCIの`.github/workflows/g04-corpus.yml`（push時に`bench/corpus.toml`/`compare_corpus.py`変更で起動、またはworkflow_dispatch）で。
- 手順：WIPのpush後のG-04 run→ログの`disagreements`でRustだけ失敗したURLと理由（status/expected_text_missing/error）を見る→原因別に直す。
- 仮説（未検証）：(1) CLI経路が本家設定を欠いていた（WIPで対処）、(2) ブラウザで`detect_block`の誤検知、(3) load待ち不足でJS本文欠落、(4) static側のimpersonate/ヘッダー差。**本家に勝つためのボット回避を新規追加する前に、まず本家と同等経路になっているかを確認**。
- `allowed_for_testing=false`の59件を含めるかは**所有者判断**（ToS）。勝手に含めない。

**B. 本家との意図的差分（変えない）**：DECISIONS.md D-010〜D-015。特に`--no-sandbox`を付けない、`hidden`属性除去(D-013)、外部公開MCPの内部宛先拒否(D-014)、要素clickの配送確認(D-015)。

**C. 「verified」の基準**：本家オラクル（固定SHAの実行結果）と比較する試験があり、CIで緑のSHAにあるものだけ。モック成功・実機成功・本家互換を混同しない。

**D. ライセンス**：本家ad_domains（Peter Lowe's list）は再配布条件未確認のため同梱しない（`--block-ads`はunsupported）。移植時はNOTICE/licensesへ記録。

## 5. 実装のコア（場所）

- MCP実行器：`crates/rustscrapling-mcp/src/backend.rs`（全ツール、`NetworkPolicy`）、HTTP transport `src/http_transport.rs`。
- ブラウザ：`crates/rustscrapling-browser/src/{lib.rs,actions.rs,injected.js,settings.rs}`（起動引数`upstream_launch_args`、click配送確認`element_click`）。
- 抽出：`crates/rustscrapling-extract/src/{compat.rs,markdownify.rs}`（本家Convertor移植、240/240一致）。
- CLI：`crates/rustscrapling-cli/src/{main.rs,extract.rs,shell.rs,curl.rs,install.rs}`。
- オラクル：`oracle/*.py`（本家venv：`python -I`で固定SHAの本家を実行）、期待値は`fixtures/oracle/`。
- 証跡：`docs/RESULTS.md`、`docs/GA_GATES.md`、`docs/compat/EVIDENCE.md`、`docs/THREAT_MODEL.md`、`docs/DECISIONS.md`。

## 6. 報告ルール

各変更でブランチ/SHA、ファイル、コマンド、exit code、証跡、残ゲートID、リスクを報告。未実装はtyped errorで返し、成功扱いしない。PRは依頼があるまで作らない。

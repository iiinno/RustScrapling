# RustScrapling — 後継エージェントへの引継ぎ / 2026-11-03

> **この文書は引継ぎの起点。成功条件の正本は必ず `Plan.md`（Revision 3）を読むこと。**
>
> 新しいGitHub作業場：**https://github.com/iiinno/RustScrapling**（Private）
>
> GitHubの`main`は、検証対象の統合commit `7a695fdc61eb7ea443d8ca16ee912c186595419c`に引継ぎ文書を追加したスナップショット。以下のSHAは状況説明の基点であり、作業開始時に最新のHEADとbranch一覧を再確認すること。

## 1. あなたの役割と最終成果

ユーザーはRustScraplingを、**Python製Scrapling v0.5の公開機能を可能な限り完全にRustへ再実装した、実際に使用できる高品質なスクレイピングシステム**として完成させることを依頼している。Rustが好きなための移植ではなく、取得成功率・応答待ち時間・データ品質・AIエージェント連携・安定運用も重要。先行する`RUSTScrapling`の良い点はライセンスと出典を守って参考にするが、機能が足りない部分は自身で実装する。

**完了の定義は `Plan.md` のG-01〜G-13すべてPASS**。本家の機能/API/エラー/既定値の対応率、実Chrome E2E、Spider復旧、MCP16ツール全操作、200URL比較、150ページ並列8の性能、fuzz/24h耐久、ライセンスと配布を含む。「登録した」「mockが通った」「ビルド成功」はGAと同義ではない。

### 最初に読むファイル

1. `Plan.md` — v3の成功条件、各機能D/A/E/H/B/F/S/P/MCP/CとM0〜M9工程。
2. `docs/IMPLEMENTATION_STATUS.md` と `docs/RESULTS.md` — 既存進捗・過去実測。**新HEADで再検証**。
3. `docs/DECISIONS.md`、`docs/COMPAT.md`、`docs/UPSTREAM_PORTS.md`、`docs/QA_AUDIT.md`。
4. `docs/compat/README.md`、`docs/compat/PARITY_MATRIX.md`、`docs/compat/PARITY_AUDIT.json`、`docs/compat/QA_RUN_2026-11-03.md` — Python固定SHAの機能棚卸しとRust側の未検証状態。
5. `crates/`、`.github/workflows/ci.yml`、`oracle/`、`fixtures/`、`tests/e2e_local.py`。

## 2. 現在の状態（確認された事実）

| 観点 | 実際の状態 |
| --- | --- |
| 計画 | `Plan.md` v3、G-01〜G-13とM0〜M9。計画は作成済みだが**製品GAは未達** |
| 統合コードの引継ぎ基点 | Windows `<workspace>/RustScrapling`、branch `integration/plan-v2-recovered`、commit `7a695fd`。GitHub `main`にはこの基点＋引継ぎファイルを公開 |
| 本家pin | `D4Vinci/Scrapling` commit `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`。Pythonはoracle/比較試験のためにのみ利用。本体のPython依存は禁止 |
| MCP | 16ツールの名前・静的引数/既定値は一致確認。ただしバックエンドBrowserAction12種のうちmatch分岐は4種（click/textbox/press_key/wait_time）、それも部分対応。スキーマ一致はruntime parityではない |
| API inventory | 既存クラス直接196宣言・継承130参照・CLI10関数を棚卸し。あとからmodule-level APIとRust entrypoint候補の監査が追加された。範囲とCI結果は現在の`docs/compat/`で確認する |
| 過去の全ワークスペース回帰 | 2026-11-03、統合SHA `024a146`: 116 PASS/0 FAIL、Chrome実機3 ignored、Chrome関係2 filtered、strict Clippy PASS、fmt PASS（VPSの3GiB制限scope）。**以降の全コミットを保証しない** |
| 追加検証 | 統合HEAD `7a695fd`のM0 Python監査14件PASS。Chrome155/156の検証記録は `docs/compat/` と`docs/RESULTS.md`等の該当コミットを参照。ライブサイトへの一般化は不可 |
| CLI実行 | 以前の通常バイナリビルドで180秒/240秒のtimeoutが発生。別ビルド済みCLIのsmoke記録は存在するが、**現HEADの生成物と同一SHAでの合否は未確定** |
| 抽出改善 | JSON-LD `@graph`著者参照とcredential-bearing URL拒否は統合済み（`259d0e9`/`024a146`） |
| 未統合・本番にない機能 | ARIA/iframe、高度なBrowserAction、MCPの実体検証、Spider rich Requestの一部、永続CLIブラウザsession、性能/耐久試験など |

### ローカルの重要な未コミット変更（GitHubには含まれない）

以下の5ファイルは統合worktreeに**最初から存在した他作業者由来の未コミット変更**。引継ぎ時に無断commit/reset/checkoutで破壊しないこと。**GitHubのmainには含まれないが、`handoff/local-docs-wip`（`7d89ce1`）へ未レビューの参考スナップショットを別途保存済み**。必要な差分を所有者・担当者の進捗と照合する。

- `crates/rustscrapling-mcp/README.md`
- `docs/COMPAT.md`
- `docs/IMPLEMENTATION_STATUS.md`
- `docs/RESULTS.md`
- `docs/UPSTREAM_PORTS.md`

GitHubの`main`はコミット済みの本体のみを掲載。5ファイルの原本はローカルの未コミット状態のまま保存されており、別branchのスナップショットは**品質保証済み変更やmainへのmerge承認ではない**。

## 3. 後継者が優先レビューすべき開発ブランチ

以下は**GitHubの同名ブランチに並行公開する作業成果**であり、main統合済みを意味しない。

### A. `feat/spider-rich-request-s03`（S-03部分実装）

- HEAD `98707e6254113eed1787734030eb2a931f4d56b7`。
- 2コミット：`ce77df3662ff3be0a6b73e08965d89593a7e190c` → `98707e6254113eed1787734030eb2a931f4d56b7`。
- 基点 `f434759`はmainの祖先。SpiderRequestへmethod/body/headers/priority、GET互換、実POST転送、request-aware dedupe、安定priority FIFO、checkpoint後方互換、cache rich bypass、32MiB上限、checkpoint secretの既定拒否を追加。
- VPSの独立worktreeで **Spider 39テストPASS**、strict Clippy PASS、fmt PASS、localhost POST wireを実証。ただし**main統合後の全workspaceテストは未実施**。
- 未達：cookies/session/metadata/callback/errback、CLI経由のrich request、at-least-once/exactly-onceの保証、機密checkpointの暗号化、priority大frontierの計算量。
- **最初に差分と安全性をレビューしてからmainへ統合し、全workspace回帰・Clippy・localhost E2Eを実施。**

### B. `feat/m0-runtime-schema`（Python本家MCP実schema比較）

- HEAD `f0275ced9013b3430211314f9e833ff6d4f2df52`。base `fdc0ac2`。
- 別途の前提コミット `e00b0b0`（runtime schema oracle/test追加）が必要。**`f0275ce`単独cherry-pickは2ファイルのmodify/delete衝突を起こす**。実際に統合worktreeで試みたが、衝突を`git cherry-pick --abort`で安全に解消済み。5つの既存dirty資料は同一ハッシュで保護済み。
- upstream実`tools/list`とRustカタログを比較：両方16ツール、名称/タイトル/説明一致、16ツールに合計348件の**構造上の**schema/annotation差（`$defs`/`$ref`/`const`/`title`、outputSchema等）。348件=348個の機能バグではない。`docs/compat/MCP_RUNTIME_SCHEMA_LIVE_V05.md`を参照。
- 通常23テスト=21 PASS/2 optional skip、隔離venvでlive oracle設定時23 PASS。本家依存インストール/pycキャッシュとdirty source検査について再現可能な手順あり。
- **親コミットとセットでレビュー**。live contract→JSON Schema意味論→Rust executor E2Eまで照合。単なるJSON表現差を無条件に「未互換」としない。

### C. その他の独立作業

ローカルには`fix/browser-http-test-keyring`（Chrome localhostのテスト環境）、`feat/browser-native-pointer`（native操作）等多数のworktreeが存在する。GitHubの初期mainと上記2ブランチが引継ぎの主対象であり、他branchは`git worktree list`・各コミットと進捗記録を調査して**二重実装・無断上書きを避ける**。

未コミットドキュメントの保全用には**`handoff/local-docs-wip`**がある。`docs/compat/LOCAL_DOCS_WIP.md`を先に読み、他作業者の成果と重複しないか確認する。**レビューなしの直接merge禁止**。

## 4. 開発環境と守るべき制約

### Windowsのローカル環境

- 統合root：`<workspace>/RustScrapling`。
- **Windows本体とWSLでCargo/Rustのビルド・テストは実行しない**。PythonによるAST監査、文書・diff検査は可。
- 全agentは独立worktreeを作り、main/integration rootと別担当のworktreeを勝手に変更しない。
- ローカルの元`origin`はGitHubではないため、**新しいGitHubは`https://github.com/iiinno/RustScrapling.git`**。取得後`git remote -v`を確認する。

### 安全なLinux検証

- 現行VPS：`ssh <user>@<host>`。ホストへの接続権はエージェントの環境に依存し、**接続できると決めつけない**。
- VPS本体：`~/Project/RustScrapling`、既存サービスは停止禁止、グローバルkill禁止。専用一時worktree、`systemd-run --user --scope -p MemoryMax=3000M -p MemorySwapMax=0 -- nice -n 12 timeout 420s ...`等で負荷制御。メモリと実行中ジョブを事前にチェック。
- 1.5GiB上限での全workspace Cargoは以前OOM。3GiBで全体116件成功した事実はあるが、他サービスに影響がある場合は保留してGitHub Actionsなどの独立Linux runnerへ切替。
- Chrome実機テストは既存Chrome・使い捨てprofileで、外部ネットワーク・認証情報・安全設定を区別。試験用`--password-store=basic`を本番利用プロファイルへ無条件に適用しない。
- `.github/workflows/ci.yml`はM0監査・Rust workspace fmt/test/Clippy/build・localhost E2Eを規定。ただしCI設定が存在することと、GitHubで最新コミットのCIが緑なことは異なる。

## 5. 最初に実行する作業（推奨順）

1. GitHub `main`をcloneし、`Plan.md`と本書を読む。`git status`, `git branch -a`, `git log --oneline -20`, `.github/workflows/ci.yml` を確認。
2. GitHub Actionsの実行状況と失敗理由を確認。mainのGA合格を**自動推定しない**。M0ローカル監査：`python3 -m unittest discover -s tests -p 'test_m0*.py' -v`。固定上流別cloneを使いinventory `--check`。
3. `feat/m0-runtime-schema`の`e00b0b0`+`f0275ce`をレビューしてsafeにmainへ統合。必要なら差分だけ切り直す。JSON Schemaの意味的差と表現差を分け、executor E2EのテストIDを対応付ける。
4. `feat/spider-rich-request-s03`の2コミットをレビュー、課題と機密保存・性能回帰に注意しながら統合。CI+VPSで全workspace試験を走らせる。S-03の残りを継続。
5. Plan M0の未完成：上流module-level API、外部継承、Python run-time schema、Rust entrypointの正確な対応、テスト証拠と`verified`判定を埋める。
6. Plan M2〜M4：本番Chrome/iframe/ARIA/12 BrowserAction/状態保存、CLI+daemon永続session、MCP16ツールの実executorへ進む。
7. Plan M5〜M9：Spider全機能、HTTP/auto差分、抽出・Adaptiveの性能、200URL比較、fuzz/24h、配布。**G-01〜G-13すべてPASSまで継続**。

### 必須の検証と報告

```bash
python3 -m unittest discover -s tests -p 'test_m0*.py' -v
python3 oracle/audit_parity.py --check
cargo fmt --all -- --check
cargo test --workspace --lib --tests --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 tests/e2e_local.py target/debug/rustscrapling
```

Cargo系は**安全なLinux runnerのみ**で実行。実行していないものはPASSと書かない。ブラウザ実機E2E、性能/外部比較は別ゲートで、pytest/unittestと混同しない。テストの無条件skipを増やさない。

報告フォーマット：`RESULT / BRANCH+SHA / FILES / TESTS (command, exit code, counts) / EVIDENCE / REMAINING ACCEPTANCE IDS / RISKS / NEXT`。本家APIの意味を満たさない操作を成功として実装しない。

## 6. 新エージェントへの最初の指示（コピー可）

> あなたはRustScraplingの後継開発エージェントです。プライベートGitHubリポジトリ `iiinno/RustScrapling` を作業の正本とし、必ず `Plan.md` v3、`AGENTS.md`、`HANDOFF.md`、`docs/compat/` を確認して、Python Scrapling v0.5固定SHAとの**完全な機能互換を実機・差分テストで証明する**ことを目標に開発を継続してください。最初に既存main・GitHub CI・未統合2ブランチを監査し、衝突や未コミット変更を保護したうえで、M0の実schema監査とS-03 rich Requestをレビュー・統合、全体回帰を行ってください。次にBrowser、MCP、Spiderの残機能をPlanの受け入れID順に実装してください。Windows/WSLでのCargo禁止、VPS既存サービス停止・global kill禁止、専用worktree/安全なLinux runnerによる検証、秘密情報保護を厳守。テスト・実装・統合・GAの状態を区別し、未達を隠さず、コミットSHAと結果を提示すること。完成まで実際にコードを編集・検証してください。

---

このドキュメントが古い場合は、**最新のGit履歴・CIの実行結果・`Plan.md`の受け入れ条件**を優先して判断する。ユーザーに実装方法を丸投げせず、明らかな仕様は自身で調査・実装し、重要な設計変更やリリース前の承認が必要なときだけ相談する。

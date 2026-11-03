# Plan.md — RustScrapling 最速実運用計画（v4、2026-11-03）

**この文書が現行の計画。** 旧計画（GA受け入れ契約 v3、G-01〜G-13）は `docs/PLAN_GA_CONTRACT.md` に退避し**凍結**した。旧ゲートは「将来公開製品にするなら」の参考であり、今は満たす必要はない。

## 1. 目的と役割

- 目的：所有者のリサーチスタック（RustXNG → PageSift / Obscura / RustScrapling ほか、AutoCLI から MCP/CLI で利用、VPS 常駐）に、Scrapling の魂である**検出回避付き取得**を Rust で組み込み、**できるだけ早く実運用に入れる**。
- RustScrapling の担当範囲：①検出回避付きブラウザ取得（他ツールが弾かれるサイト）、②セッション/ログイン状態と操作（`browser_actions`）、③並列クロール（Spider）と adaptive セレクタ。通常ページの軽い抽出は PageSift 等が担うので、ここで張り合わない。
- 組み込み口：**`rustscrapling mcp`（本家 Scrapling と同じ16ツール）を第一**、CLI（`extract`/`get`/`fetch`/`crawl`）を第二。Rust ライブラリ API の整備は後回し。
- 「完成」の定義：下の §3 必須修正が入り、VPS で MCP として常用され、失敗が型付きで返ること。成功率は**運用しながら**上げる（§5）。

## 2. 凍結すること（やらない）

本家API 394項目の網羅検証（G-01）、24時間耐久（G-10）、SBOM・脅威モデルの拡充、200URL汎用コーパスで本家に勝つこと（G-04）、バイト一致の追加証明。既存の資産（テスト、`docs/RESULTS.md`、`docs/DECISIONS.md`、CI）は維持するが**増やすことを目的にしない**。CIが壊れたら直す、のみ。

## 3. 必須修正（この順に。各1日以内が目安）

0. **WIPの検証**：HEAD 付近の WIP コミット（CLIブラウザ取得の本家既定値、`bench/compare_corpus.py`）を検証。
   ```bash
   CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 cargo test --workspace --locked --lib --tests
   CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings
   python3 oracle/audit_parity.py && scripts/quick_checks.sh
   ```
   pub fn を増減したら `python3 oracle/audit_parity.py` を必ず再生成（忘れるとCIの parity ジョブが落ちる）。
1. **HTTP経路の文字コード対応（最重要・日本語サイト）**：現状、本文を常に `String::from_utf8_lossy` で読むため Shift_JIS/EUC-JP が**文字化けしたまま成功扱い**になる（`grep -rn from_utf8_lossy crates/*/src`：mcp backend、cli network/shell、spider）。
   - 実装：`rustscrapling-http` に `decode_body(headers, body) -> String` を追加し、判定順は ①`Content-Type` の `charset` ②先頭1024バイト内の `<meta charset>`/`<meta http-equiv content="...charset=">` ③BOM ④UTF-8妥当ならUTF-8 ⑤日本語推定（`chardetng` 等）、デコードは `encoding_rs`。全呼び出し箇所をこれに置換。
   - テスト：Shift_JIS/EUC-JP/ヘッダー無しmetaのみ/BOM付き の4ケースをループバックサーバで。ブラウザ経路は Chrome がデコードするので対象外。
2. **一時プロファイルの残留**：1時間 soak で `rustscrapling-cdp-*` 一時ディレクトリが約2個/時残った（daemon 停止・再起動時）。`BrowserClient::launch` の `owned_profile` が `Drop`/強制終了経路で消えていない可能性。`Drop for BrowserClient` でプロセス kill 後にプロファイル削除を試みる＋daemon 起動時に自分のPID以外の古い `rustscrapling-cdp-*`（例：24h超）を掃除。`bench/soak.py --duration 1800` で残留0を確認。
3. **ブラウザのタブプール（並列利用する場合）**：`BrowserClient::get` は既定ページ1枚を mutex で使い回すため、CLI/daemon のブラウザ取得は**実質直列**。本家 `StealthySession(max_pages=N)` 相当に、1ブラウザ・最大Nタブのプールにする（タブはリクエスト毎に借りて返す、設定は作成時に `apply_settings`）。MCP の `browser_open` セッションはセッション=1タブのままでよい。急がないなら「同時ブラウザ取得は1」と運用で割り切り、後回し可。

## 4. VPS配置（半日）

```bash
cargo build --release --locked -p rustscrapling-cli
target/release/rustscrapling install          # Chrome for Testing 156（固定SHA-256検証）
target/release/rustscrapling mcp              # stdio。AutoCLI等のMCPクライアントに登録
```
- HTTPで使う場合は `rustscrapling mcp --http --auth-token <token>`（loopbackのみ）。外部公開は避ける（公開時は内部宛先が自動拒否される：D-014）。
- root で動かす場合 Chrome は `--no-sandbox` が必要だが、**本番は非rootユーザーで動かす**（サンドボックス維持）。Ubuntu 24.04 で /opt 外の Chrome を使うなら `kernel.apparmor_restrict_unprivileged_userns=0` が必要な場合がある。
- 常駐の CLI 利用は `rustscrapling daemon start` → `session ...`。

## 5. 運用しながら改善するループ（継続）

1. RustScrapling が失敗したURLと型付き理由（`blocked`/`timeout`/`http_status`/`parse` 等）をスタック側でログする。
2. 週1回、溜まった失敗URLで本家 Python と比較：`bench/compare_corpus.py`（`bench/corpus.toml` の代わりに失敗URLのTOMLを渡す）。**計測は直接通信の環境で**（TLS終端プロキシ下では指紋が無意味。GitHub Actions の `g04-corpus.yml` か VPS 本体）。
3. 本家だけ成功したURLの型ごとに直す。既知の差：`solve_cloudflare` 未実装、patchright 相当パッチ・browserforge 相当の指紋生成の差は未測定。CI実測（`5a6a4c5`、修正前）でブラウザ経路 Rust 72% 対 本家 87%。まず「本家と同等の経路・既定値になっているか」を疑い、新しい回避技術の追加はその後。

## 6. 判断の拠り所（迷ったとき）

- **未実装は型付きエラーで返す。偽の成功は絶対に作らない**（文字化け成功・空本文成功を含む）。
- 本家との意図的差分は維持：`docs/DECISIONS.md` D-010〜D-015（サンドボックス維持、`hidden`属性除去、外部公開MCPの内部宛先拒否、要素clickの配送確認）。
- 「動いた」の区別：モック成功／実機成功／本家と同じ結果、を混同して報告しない。
- ライセンス：移植・同梱時は NOTICE/licenses に記録。本家の広告ドメイン一覧は再配布条件未確認のため同梱しない。
- 資格情報・cookie・token・プロファイルを Git やログに出さない。
- ディスクに注意（コンテナ・VPSとも）。`CARGO_INCREMENTAL=0`、古い `target/debug/deps` は削除可。満杯中に取ったテスト結果は無効。

## 7. 主要コードの場所

- MCP：`crates/rustscrapling-mcp/src/backend.rs`（全16ツール、`NetworkPolicy`）、`src/http_transport.rs`
- ブラウザ：`crates/rustscrapling-browser/src/lib.rs`（`BrowserClient::get/launch`、状態ファイル）、`actions.rs`（12アクション、`element_click`）、`settings.rs`（`upstream_launch_args`、`PageSettings`）、`injected.js`
- HTTP：`crates/rustscrapling-http/src/lib.rs`（wreq、safe redirect、cookie jar）
- 抽出：`crates/rustscrapling-extract/src/compat.rs`（本家Convertor移植）、`markdownify.rs`
- CLI：`crates/rustscrapling-cli/src/`（`main.rs`、`extract.rs`、`shell.rs`、`install.rs`、`browser_adapter.rs`、`daemon.rs`）
- Spider：`crates/rustscrapling-spider/src/lib.rs`
- 計測：`bench/compare_corpus.py`、`bench/compare_local_crawl.py`、`bench/soak.py`

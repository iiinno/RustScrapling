# Plan.md — RustScrapling 最速実運用計画（v4.1、2026-11-03）

**この文書が現行の計画。** 旧計画（GA受け入れ契約 v3、G-01〜G-13）は `docs/PLAN_GA_CONTRACT.md` に退避し**凍結**した。

## 1. 目的と組み込み先（research-stack を読んだ上での確定事項）

- 組み込み先は `iiinno/research-stack` の **Research Router**（Rust、`router/src/main.rs`、MCP `/mcp` の `research_fetch`）。Router 側 Plan の Phase 6 が **`fetch-v1` アダプタで PageSift → RustScrapling → Obscura の順** に入れる予定。RustScrapling 自身の MCP サーバは当面使わない（Router が唯一のMCP窓口）。
- 既存 Obscura アダプタ（`browser_fetch()`）が手本：URL を Router の `validate_public_url` で検査 → `systemd-run --user --scope -p MemoryMax=800M -p CPUQuota=100% timeout 25s <bin> fetch URL --dump text` を起動 → stdout を上限付きで読む → `{"provider","url","status":"ok|partial","retrieved_at","truncated","content"}` を返す。SSRF検査は Router が行うので RustScrapling 側の追加ポリシーは不要。
- **RustScrapling 側の推奨インターフェース**：`rustscrapling auto <URL> --format json --timeout 20 --max-chars N`（HTTP→ブラウザの段階昇格を1コマンドで行い、成功時 stdout に `{"status","url","tier","content":{"markdown",...}}`、失敗時 stderr に `{"error":{"kind":"blocked|timeout|dns|tls|http_status|...","message","hint"}}` と exit 1）。アダプタはこれを Obscura と同じ形で起動し、`content.markdown` を `content` に、`tier` と `status` を付記する。ブラウザは `--chrome-path`（`rustscrapling install` で入れた CfT）を明示。
- RustScrapling の担当：PageSift が失敗した「難しいページ」（ボット対策・JS描画）を、Obscura より前に救うこと。通常ページは PageSift の仕事。
- 「完成」の定義：§3 の必須修正が入り、Router の `fetch-v1` から呼ばれ、失敗が型付きで返り、Router 側 Phase 6 の独立回帰試験（重要実サイトの取得件数・エラー率・リソース比較）を通ること。

### 所有者の決定（2026-11-03、確定）
1. **フォールバック条件**：PageSift が **403・429・空本文** のとき RustScrapling へ回す（Router `fetch_routed` の `auto` 分岐を変更：現在は BAD_GATEWAY のみ Obscura へ）。RustScrapling も失敗したら Obscura。
2. **防御回避は全部やる**：Scrapling の検出回避機能をすべて実装・使用する（robots.txt は本家同様に既定OFF、Cloudflare 等のチャレンジ突破も実装する）。research-stack の Plan.md にある「Cloudflare 403 は防御回避しない」は所有者の方針ではない（別途 research-stack 側も所有者が修正する）。
   - 実務上の歯止めとして残すもの（倫理ではなく**ブロック・IP凍結を避けるため**）：ドメイン別の同時数・間隔制御、Retry-After 尊重、無限再試行禁止、チャレンジ失敗時は型付き `blocked` で返す。
   - 評価用 `bench/corpus.toml` の `allowed_for_testing=false`（59件）も `--include-restricted` で含めてよい。

### 検出回避の実装ワークストリーム（必須修正の後、成功率改善の本体）
本家（`scrapling/engines/_browsers/`、patchright、browserforge）を読み、同等以上を Rust で。優先順：
1. **`solve_cloudflare`**：本家の Cloudflare 対応（Turnstile/interstitial の検出 → 待機 → チェックボックスのクリック → 通過確認）を移植。現状 MCP/CLI では `unsupported`。`BrowserPage::run_actions` の click（実ポインタ、D-015 の配送確認付き）を使う。
2. **patchright 相当の痕跡除去**：本家 patchright が当てるパッチ（CDP 由来の検出点：`Runtime.enable` 回避は済、`navigator.webdriver`、utility world 経由の評価、console/Worker 経由の検出など）を一覧化し、未対応分を CDP 側で再現。
3. **browserforge 相当の指紋整合**：UA・Client Hints・ヘッダー順・画面・ロケール・タイムゾーンの一貫した組を生成し、HTTP（wreq impersonate）とブラウザで揃える。
4. 計測：`bench/compare_corpus.py --include-restricted`（直接通信環境、GitHub Actions `g04-corpus.yml`）で本家と比較し、bot-challenge/news カテゴリの差を詰める。

## 2. 凍結すること（やらない）

（注：検出回避は凍結対象ではない。上の「検出回避の実装ワークストリーム」が成功率改善の本体）


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

## 4. VPS配置（半日）— Router Phase 6 の手順に従う。無断でビルド・起動しない（research-stack の禁止事項）

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

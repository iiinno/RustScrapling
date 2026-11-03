# Plan.md — RustScrapling 完成から research-stack 本番検証までの一気通貫計画（v5、2026-11-03）

対象読者：自律実行するコーディングエージェント。**上から順に実行し、各フェーズの「合格条件」を満たしてから次へ進む。** 旧GA契約は `docs/PLAN_GA_CONTRACT.md`（凍結、参照不要）。

---

## 0. 前提・絶対ルール（違反しそうなら作業を止めて報告）

### リポジトリ
- RustScrapling：`iiinno/RustScrapling`、作業ブランチ `claude/laughing-clarke-xb55gw`（ここで開発・push）。
- research-stack：`iiinno/research-stack`（Research Router `router/src/main.rs`、運用 `ops/`）。変更は**別ブランチ**（例 `feat/fetch-v1-rustscrapling`）で push。`main` へ直接 push しない。

### 所有者の決定（確定。変更しない）
1. Router の取得順は **PageSift → RustScrapling → Obscura**。PageSift が **403・429・空本文・502系** のとき RustScrapling へ、RustScrapling も失敗したら Obscura へ。
2. **検出回避は全部やる**：Scrapling の機能をすべて実装・使用。robots.txt は本家同様に既定で無視、Cloudflare 等のチャレンジ突破も実装する。research-stack Plan.md の「Cloudflare 403 は防御回避しない」は所有者の方針ではない（削除してよい）。
3. 残す歯止めは**ブロック・IP凍結回避のための実務**のみ：ドメイン別同時数と間隔、Retry-After 尊重、無限再試行禁止、突破失敗は型付き `blocked` で返す。
4. RustScrapling の research-stack への導入・ビルド・起動は**この計画をもって所有者が承認済み**（research-stack Phase 6）。

### 安全ルール（環境を壊さない）
- **Windows/WSL で Cargo を実行しない。** Linux（CI か VPS）のみ。VPS では `systemd-run --user --scope -p MemoryMax=3G nice -n 10 timeout 3600 cargo ...`、`CARGO_BUILD_JOBS=2`、`CARGO_INCREMENTAL=0`。
- VPS の既存サービス（research-router / rustxng / pagesift / mcpmem / tailscale serve）を**停止しない**。全体 kill、docker 全停止、Tailscale Serve/Funnel 設定の上書き禁止。Router 更新は「新バイナリを別名で置き、テストが通ってから差し替え、`systemctl --user restart research-router` のみ」。
- トークン・cookie・`router.env`・ブラウザプロファイルを Git・ログ・チャットに出さない。
- ディスク：作業前に `df -h`。空き 5GB 未満なら `target/debug/incremental` と古い `target/debug/deps` を削除。**ディスク満杯中に得たテスト結果は無効**として再実行。
- 偽の成功を作らない：文字化け・空本文・チャレンジページを成功として返さない。未実装は `unsupported` の型付きエラー。
- pub fn を増減したら `python3 oracle/audit_parity.py` を実行してから `scripts/quick_checks.sh`（忘れると CI の parity ジョブが落ちる）。

### 毎フェーズ共通の検証コマンド（RustScrapling）
```bash
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 cargo test --workspace --locked --lib --tests
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets --locked -- -D warnings
python3 oracle/audit_parity.py && scripts/quick_checks.sh
```
実機 Chrome 試験（Phase 3 以降は必須）：`scripts/chrome_e2e_matrix.sh /path/to/chrome`（root 環境では `--no-sandbox` を付けるラッパーを渡す。本番は非root）。

### 報告フォーマット（各フェーズ末に1回）
ブランチ/コミットSHA、変更ファイル、実行コマンドと exit code、テスト件数（passed/failed/ignored）、合格条件の達否、残課題、リスク。

---

## Phase 1：WIP の検証（0.5日）

HEAD 付近の WIP コミット「wip: upstream defaults for CLI browser fetches; corpus runner via extract」はビルドのみ確認済み・テスト未完。
1. 共通検証コマンドを実行。
2. 失敗したら原因は `crates/rustscrapling-browser/src/lib.rs` の `BrowserGetOptions`（`settings`/`referrer` 追加、`Copy` 削除）か `crates/rustscrapling-cli/src/browser_adapter.rs` の `get_options`。直せなければ `git revert <WIPのSHA>`。

**合格条件**：workspace テスト 0 failed、clippy 0、quick_checks OK。

---

## Phase 2：必須修正（2〜3日）

### 2-1. HTTP 経路の文字コード対応【最重要】
問題：本文を常に `String::from_utf8_lossy` で読むため、日本の Shift_JIS/EUC-JP サイトが**文字化けしたまま成功扱い**。該当：`grep -rn from_utf8_lossy crates/*/src`（mcp `backend.rs`、cli `network.rs`/`shell.rs`、spider `lib.rs`）。

実装（`crates/rustscrapling-http` に追加。依存に `encoding_rs` と `chardetng`）：
```rust
/// Decode an HTTP body: charset from Content-Type, then <meta> in the first
/// 2 KiB, then BOM, then valid UTF-8, then a chardetng guess (TLD hint).
pub fn decode_body(headers: &[(String, String)], body: &[u8], url: &str) -> (String, &'static str) {
    use encoding_rs::Encoding;
    let from_header = headers.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, v)| v.split(';').find_map(|p| p.trim().strip_prefix("charset=")))
        .and_then(|c| Encoding::for_label(c.trim_matches('"').as_bytes()));
    let from_meta = || {
        let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
        let i = head.find("charset=")?;
        let rest = head[i + 8..].trim_start_matches(['"', '\'']);
        let label: String = rest.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        Encoding::for_label(label.as_bytes())
    };
    let encoding = from_header.or_else(from_meta)
        .or_else(|| Encoding::for_bom(body).map(|(e, _)| e))
        .unwrap_or_else(|| if std::str::from_utf8(body).is_ok() { encoding_rs::UTF_8 } else {
            let mut d = chardetng::EncodingDetector::new();
            d.feed(body, true);
            d.guess(url.rsplit('.').next().map(str::as_bytes), true)
        });
    let (text, _, _) = encoding.decode(body);
    (text.into_owned(), encoding.name())
}
```
- HTML を扱う全呼び出し箇所を置換（バイナリ応答は対象外）。
- テスト（`crates/rustscrapling-http/tests/charset.rs`、ループバックサーバ）：①ヘッダー `charset=Shift_JIS` ②ヘッダー無し・`<meta charset="EUC-JP">` ③UTF-8 BOM ④ヘッダー無し・meta 無しの Shift_JIS（推定）。期待文字列「日本語のテスト」が復元されること。本文は `encoding_rs::SHIFT_JIS.encode("...")` で作る。

**合格**：4ケース PASS、既存テスト全 PASS。

### 2-2. 一時プロファイルの残留
1時間 soak で `rustscrapling-cdp-*`（`std::env::temp_dir()` 配下）が約2個/時残った。
- `crates/rustscrapling-browser/src/lib.rs` の `impl Drop for BrowserClient`：子プロセス kill 後に `owned_profile` を `std::fs::remove_dir_all`（失敗は無視、`shutdown()` と二重でも無害に）。
- daemon 起動時に、`temp_dir` 内の `rustscrapling-cdp-*` で **mtime が 6時間以上前** のものを削除。
- 検証：`python3 bench/soak.py target/release/rustscrapling --chrome <chrome> --duration 1800 --restart-every 10` で `after_stop.temp_profiles == 0` かつ `slope_per_hour_second_half.temp_profiles == 0`。

### 2-3. ブラウザのタブプール
現状 `BrowserClient::get` は既定ページ1枚を mutex で使い回すため直列。Phase 4 の Router 連携は**1呼び出し=1プロセス**（Obscura と同形）なので**今は不要**。daemon 常駐化するときに実装：`max_pages`（既定4）を `tokio::sync::Semaphore` で貸し出し、作成時に `apply_settings`。

**Phase 2 合格条件**：2-1・2-2 完了、共通検証 OK、CI 緑。

---

## Phase 3：検出回避（成功率の本体、3〜7日）

本家ソース（固定SHA）を読む：
```bash
git clone https://github.com/D4Vinci/Scrapling ../scrapling-v05-reference
git -C ../scrapling-v05-reference checkout 95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0
grep -rn "solve_cloudflare\|cloudflare\|turnstile\|challenge" ../scrapling-v05-reference/scrapling/engines | head -50
```
移植時は BSD-3-Clause の出典を `NOTICE` に追記。

### 3-1. `solve_cloudflare`
- 本家の手順（interstitial/Turnstile 検出 → 待機 → iframe 内チェックボックスを**実ポインタでクリック** → 通過まで待機 → 本文）を `crates/rustscrapling-mcp/src/backend.rs` の `fetch_on_page` 後段に実装。Turnstile は別オリジン iframe なので、クリック座標はトップページ側の iframe の `getBoundingClientRect` から計算し、座標クリック（`BrowserAction::Click` の Point）を使う。
- 現在 `solve_cloudflare=true` は `browser_options()` で `unsupported`。実装に置き換え、`extract fetch --solve-cloudflare` と CLI `auto`/`fetch` のブラウザ段（`crates/rustscrapling-cli/src/browser_adapter.rs`）でも有効化（`auto` は既定で有効）。
- 制限時間内に通過しなければ `blocked`。
- テスト：JS で数秒後に本文を差し替える疑似チャレンジページで、待機・再判定ロジックを決定的に試験。実チャレンジは Phase 6 の実測で。

### 3-2. patchright 相当の痕跡除去
- patchright（`pip download patchright==1.64.4`）のパッチ内容を確認し、`docs/STEALTH.md` に表を作る：検出点 / 本家の対策 / RustScrapling の現状 / 対応。
- 対応済み：`Runtime.enable` 不送信（D-007）、isolated world 評価、`--disable-blink-features=AutomationControlled`、headless UA の `HeadlessChrome` 置換、Playwright 起動スイッチ（D-011）。
- 確認候補：`navigator.webdriver`、`addScriptToEvaluateOnNewDocument` の痕跡、console API 差、Worker 内 UA、`window.chrome` 系の欠落、WebGL ベンダ、Permissions API、plugins。bot.sannysoft.com・browserscan・creepjs を**直接通信環境で**開いて埋める。

### 3-3. browserforge 相当の指紋整合
- UA・`Sec-CH-UA*`・Accept-Language・プラットフォーム・画面・タイムゾーン・ロケールが矛盾しない「実在する組」を数十件テーブルで持ち（乱数で組み立てない）、`crates/rustscrapling-browser/src/settings.rs` から選ぶ。HTTP 側（`rustscrapling-http` の impersonate）とブラウザ側で同じ組を使う。

### 3-4. 計測と改善ループ
- `bench/compare_corpus.py --include-restricted`（`bench/corpus.toml` 200件）を**直接通信環境**で：GitHub Actions `.github/workflows/g04-corpus.yml`（`bench/corpus.toml` か `compare_corpus.py` の変更 push で起動）か VPS。**TLS 終端プロキシ下（クラウド開発コンテナ等）の結果は無効**。
- 出力の `disagreements`（Rust だけ失敗した URL と理由）を見て直す。前回実測（修正前 `5a6a4c5`）：browser 経路 Rust 72.3% 対 本家 87.2%、static 80.9% 対 83.7%。

**合格条件**：直接通信環境で browser 経路の成功率が本家と同等以上、bot-challenge カテゴリで本家以上。届かなければ差の内訳を報告して Phase 4 へ進んでよい（運用しながら改善）。

---

## Phase 4：Router の fetch-v1 アダプタ（research-stack、1日）

### 4-1. 呼び出し方（Obscura アダプタ `browser_fetch()` と同形）
```bash
rustscrapling auto <URL> --format json --timeout 20 --max-chars 200000 --chrome-path <CfT chrome>
```
- 成功：exit 0、stdout に JSON。使うキー：`status`（HTTP）、`url`（最終URL）、`tier`（`http`/`browser`）、`content.markdown`。
- 失敗：exit 1、stderr 最終行に `{"schema_version":1,"error":{"kind":"blocked|timeout|dns|tls|http_status|...","message":...}}`。
- ブラウザは `rustscrapling install --dir ~/services/research-stack/browsers` の Chrome for Testing（固定SHA-256検証）。**非rootで実行**。

### 4-2. `router/src/main.rs` の変更
1. `rustscrapling_fetch(target)` を `browser_fetch` の隣に追加（雛形）：
```rust
async fn rustscrapling_fetch(target:&str) -> ApiResult<Value> {
    let target=validate_public_url(target).await?;
    let child=Command::new("/usr/bin/systemd-run")
        .env_clear()
        .env("XDG_RUNTIME_DIR","/run/user/1000")
        .env("DBUS_SESSION_BUS_ADDRESS","unix:path=/run/user/1000/bus")
        .env("LANG","C.UTF-8")
        .env("HOME","/home/inouemasaki")
        .args(["--user","--scope","-p","MemoryMax=1200M","-p","MemorySwapMax=0","-p","CPUQuota=150%",
            "/usr/bin/timeout","40s",
            "/home/inouemasaki/services/research-stack/bin/rustscrapling",
            "auto",target.as_str(),"--format","json","--timeout","20","--max-chars","200000",
            "--chrome-path","/home/inouemasaki/services/research-stack/browsers/chrome-156.0.8078.80-linux64/chrome-linux64/chrome"])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().map_err(|_|err(StatusCode::BAD_GATEWAY,"RustScrapling could not start"))?;
    // stdout/stderr を上限付き（MAX_PAGE_BYTES+1）・43秒タイムアウトで読む（Obscura と同じ書き方）。
    // exit 0 → JSON を parse し content.markdown を取り出す。空なら BAD_GATEWAY。
    // exit≠0 → stderr 最終行の error.kind だけをメッセージに含めて BAD_GATEWAY（message 本文は外に出さない）。
    // 成功時：{"provider":"rustscrapling","url":target,"source_url":url,"source_status":status,
    //   "tier":tier,"retrieved_at":now,"status":"ok|partial","truncated":..,"content":markdown}
    todo!()
}
```
   パスは VPS の実配置に合わせる（`ls ~/services/research-stack/bin`）。
2. PageSift の失敗を区別：`fetch()` は PageSift 応答が 4xx だと `FORBIDDEN` を返し**フォールバックしない**。403/429 と空本文は「PageSift がブロックされた」と判別できるエラー（理由文字列 `pagesift_blocked` などの専用 enum が望ましい）にする。PageSift への入力不正（400/422）はフォールバックしない。
3. `fetch_routed`：
```rust
"auto"=>match fetch(app,target).await {
    Ok(d)=>Ok(d),
    Err(e) if is_fallback_worthy(&e) => match rustscrapling_fetch(target).await {
        Ok(d)=>Ok(d),
        Err(_) => browser_fetch(target).await,   // Obscura
    },
    Err(e)=>Err(e),
},
"rustscrapling"=>rustscrapling_fetch(target).await,
```
   `engine` enum と `research_fetch` の inputSchema/description に `rustscrapling` を追加。`research_status` を `{"enabled":true,"installed":true,"adapter":"fetch-v1"}` に。
4. RustScrapling の同時実行は `tokio::sync::Semaphore`（同時2）で制限。
5. テスト（`router/tests/`）：偽の `rustscrapling`（成功JSON／失敗JSON を返すシェルスクリプト）で、PageSift 403 → RustScrapling 成功、RustScrapling 失敗 → Obscura、PageSift 400 → フォールバックなし、の分岐試験。

**合格条件**：router の既存＋新規テスト PASS、`cargo clippy -- -D warnings` 0。

---

## Phase 5：VPS 導入（0.5日）

```bash
cd ~/build/RustScrapling && git pull
systemd-run --user --scope -p MemoryMax=3G nice -n 10 timeout 3600 \
  env CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo build --release --locked -p rustscrapling-cli
scripts/release_smoke.sh target/release/rustscrapling target/release-smoke      # 合格が前提
install -m 755 target/release/rustscrapling ~/services/research-stack/bin/rustscrapling.new
~/services/research-stack/bin/rustscrapling.new install --dir ~/services/research-stack/browsers
mv ~/services/research-stack/bin/rustscrapling.new ~/services/research-stack/bin/rustscrapling
~/services/research-stack/bin/rustscrapling auto https://example.com --format json --chrome-path <上の chrome>  # exit 0
# Router：新バイナリを別名でビルド→テスト→差し替え→ research-router だけ再起動
```
- Ubuntu 24.04 で /opt 外の Chrome サンドボックスが起動しない場合のみ `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`（**所有者に確認してから**）。

---

## Phase 6：research-stack 一気通貫検証（1日）

### 6-1. 健全性
```bash
systemctl --user status research-router research-rustxng research-pagesift research-mcpmem
curl -fsS http://127.0.0.1:19101/healthz
```
MCP `tools/list` は10ツールのまま（`research_fetch` の engine に `rustscrapling` が増える）。`research_status` で rustscrapling enabled。

### 6-2. 経路別の取得試験
`ops/research-router-fetch-v1.py` を新規作成し、Router の `/v1/fetch`（または MCP `research_fetch`）を叩き、各 URL の `provider`・`status`・本文先頭・所要時間を記録：
| ケース | 例 | 期待 |
| --- | --- | --- |
| 通常 | https://example.com/ | provider=pagesift |
| 日本語 Shift_JIS/EUC-JP | `Content-Type: ...charset=shift_jis` を返す実在の古いサイト（RustXNG で探す） | rustscrapling 経由なら文字化け無し（「縺」「繧」「�」が無い） |
| JS 描画 | https://quotes.toscrape.com/js/ | rustscrapling（tier=browser）、「Albert Einstein」を含む |
| Cloudflare | https://www.scrapingcourse.com/cloudflare-challenge | rustscrapling で「You bypassed the Cloudflare challenge」 |
| 対ボット | https://www.scrapingcourse.com/antibot-challenge、https://nowsecure.nl/ | rustscrapling 成功。失敗時は blocked → Obscura へ |
| 存在しない | https://nonexistent.invalid/ | 型付き失敗（成功扱いしない） |
| 内部アドレス | http://127.0.0.1/ | Router の `validate_public_url` で拒否 |

### 6-3. 実リサーチ通し試験
RustXNG で日本語（「生成AI 最新 ニュース」「睡眠 改善 方法」）と英語（「LLM agent benchmark 2026」）を検索 → 各上位10件を `research_fetch(engine=auto)` → provider 別件数、成功率、失敗理由、平均時間、ピークメモリを集計。**PageSift 失敗を RustScrapling が救った件数**を必ず報告。

### 6-4. 負荷・残留
同時4リクエスト×20件で Router が落ちない、RustScrapling の同時実行が2を超えない、終了後に `ls /tmp | grep rustscrapling-cdp` が空、`pgrep -f chrome-linux64` が空。

**Phase 6 合格条件**：6-1 全緑、6-2 が期待どおり（Cloudflare/対ボットは失敗時に blocked→Obscura へ正しく落ちれば可、成功率は報告）、6-3 で救済件数 > 0 かつ文字化け 0、6-4 で残留 0・サービス停止 0。

---

## Phase 7：運用改善ループ（継続）
- Router ログから RustScrapling の失敗（URL・kind）を週次抽出 → `bench/compare_corpus.py` 用 TOML に変換 → 本家と比較 → 差のある型から Phase 3 を回す。
- 長期運用の不具合は見つけ次第直す（所有者方針）。

---

## 付録 A：迷ったとき
- 「本家と同じか」より「読みたいページが取れて、壊れた結果を成功と言わないか」を優先。
- 本家との意図的差分は維持：`docs/DECISIONS.md` D-010〜D-015（サンドボックス維持、`hidden` 属性除去、クリック配送確認など）。D-014（外部公開 MCP の内部宛先拒否）は Router 経由では使わない。
- 失敗し続けるサイトに再試行回数を増やして挑まない（IP 凍結リスク）。手法を変える（Phase 3）。
- 不明点が所有者の価値判断なら止めて質問。技術的な試行錯誤は自分で解決。

## 付録 B：主要コード（RustScrapling）
- CLI：`crates/rustscrapling-cli/src/{main.rs,network.rs(get/auto/fetch),extract.rs,shell.rs,install.rs,browser_adapter.rs,daemon.rs}`
- MCP 実行器：`crates/rustscrapling-mcp/src/backend.rs`（`fetch_on_page`、`browser_options`、`http_request`）
- ブラウザ：`crates/rustscrapling-browser/src/{lib.rs,actions.rs,settings.rs,injected.js}`
- HTTP：`crates/rustscrapling-http/src/lib.rs`
- 抽出：`crates/rustscrapling-extract/src/{lib.rs,compat.rs,markdownify.rs}`
- 段階制御・ブロック判定：`crates/rustscrapling-fetch/src/lib.rs`（`FetchTier::Auto`、`detect_block`）
- 計測：`bench/compare_corpus.py`、`bench/corpus.toml`、`bench/soak.py`、`.github/workflows/g04-corpus.yml`
- 証跡：`docs/RESULTS.md`、`docs/DECISIONS.md`、`docs/THREAT_MODEL.md`

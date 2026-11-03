# STEALTH.md — 検出点ごとの対策と実測状況

対象は本家 Scrapling の固定 SHA `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`（`scrapling/engines/_browsers/_stealth.py`、`_base.py`、`pyproject.toml` の `patchright==1.64.4`）。BSD-3-Clause の出典は `NOTICE` に記載済み。

「計測」列は**このリポジトリで実際に観測したかどうか**を表す。`未計測` は対策の有無だけを示し、突破できることの証拠ではない。直接通信環境（TLS 終端プロキシ配下は無効）で bot.sannysoft.com、browserscan、creepjs を開いて埋める。

| 検出点 | 本家の対策 | RustScrapling の現状 | 対応 | 計測 |
| --- | --- | --- | --- | --- |
| `navigator.webdriver` | Playwright 既定の `--disable-blink-features=AutomationControlled` | 同じスイッチを発行（`PLAYWRIGHT_SWITCHES`） | 同等 | 未計測 |
| CDP の `Runtime.enable` 痕跡 | patchright は `Runtime.enable` を送らない | 送らない（DECISIONS D-007）。評価は `DOM.resolveNode` 経由 | 同等 | 未計測 |
| `addScriptToEvaluateOnNewDocument` の痕跡 | patchright は使わない | 使わない（`Page.addScriptToEvaluateOnNewDocument` を呼ばない） | 同等 | 未計測 |
| console API の差し替え痕跡 | 差し替えない | 差し替えない | 同等 | 未計測 |
| isolated world の露出 | patchright 相当の分離 | utility world で評価（`__rustscrapling_utility__`） | 同等 | 未計測 |
| headless UA の `HeadlessChrome` | UA から `HeadlessChrome` を除去 | 除去する（`settings.rs`） | 同等 | 未計測 |
| Worker 内の UA | 明示設定なし | 未対応（Worker の `navigator.userAgent` は別経路） | **未対応** | 未計測 |
| `window.chrome` 系の欠落 | 明示対策なし（素の Chrome で充足） | 素の Chrome を起動するため充足見込み | 同等（推定） | 未計測 |
| WebGL ベンダ / レンダラ | `--disable-webgl` 等の opt-in のみ | 既定で無効化しない（素の値） | 同等 | 未計測 |
| Permissions API | 明示対策なし | 未対応 | **未対応** | 未計測 |
| plugins / mimeTypes | 明示対策なし | 素の Chrome の値 | 同等（推定） | 未計測 |
| 起動スイッチの集合 | Playwright `chromiumSwitches()` + `DEFAULT_ARGS` + `STEALTH_ARGS` | 同じ集合を移植し、`--enable-features` 系を 1 本にマージ | 同等 | 未計測 |
| サンドボックス | 本家は Playwright 既定で `--no-sandbox` を付けない | 付けない（DECISIONS D-011） | 意図的に同等 | 同等 |
| `Sec-CH-UA*` と UA の整合 | browserforge 相当の指紋整合 | **未実装**（UA は個別指定、Client Hints は Chrome 任せ） | **未対応** | 未計測 |
| Accept-Language / タイムゾーン / ロケール | `PageSettings` で設定 | HTTP 側とブラウザ側で同じ組を選ぶ仕組みが**未実装** | **未対応** | 未計測 |
| Cloudflare interstitial / Turnstile | `_cloudflare_solver` | 移植済み（`solve_cloudflare`、cType 判定 + 座標クリック + 2 回再試行） | 同等（未検証） | 未計測 |

## 対応が必要な残り

1. **Worker 内 UA** — Worker から `navigator.userAgent` を読む検査がある。`Network.setUserAgentOverride` は Worker に波及しないため、`--user-agent` 起動スイッチとの併用が要る。
2. **Permissions API** — headless で `Notification.permission` が `default` のままなど、素の Chrome と差が出る。`Browser.grantPermissions` は使わない方針で、必要なら `Page.setPermission` 相当を検討する。
3. **指紋の整合（Phase 3-3）** — UA・`Sec-CH-UA*`・Accept-Language・プラットフォーム・画面・タイムゾーン・ロケールを「実在する組」として数十件のテーブルに持ち、HTTP 側（`rustscrapling-http` の impersonate）とブラウザ側で同じ組を使う。現状は個別に指定できず、組み合わせの整合は保証していない。

## 計測手順（直接通信環境）

```bash
# TLS 終端プロキシ配下では無効。VPS から直接叩くこと。
rustscrapling auto https://bot.sannysoft.com/ --format json --tier browser --chrome-path <CfT chrome>
rustscrapling auto https://browserscan.net/ --format json --tier browser --chrome-path <CfT chrome>
rustscrapling auto https://abrahamjuliot.github.io/creepjs/ --format json --tier browser --chrome-path <CfT chrome>
```

結果は `docs/RESULTS.md` に追記し、本表の「計測」列を `済（日付・判定）` に更新する。

# RESULTS.md: 測定と検証の結果

## 段階1（2026-11-02）

計測条件: Muse VM（Ubuntu 24.04.5、4コア、メモリ7.7GiB）、release ビルド（`[profile.release] lto = true`、CARGO_BUILD_JOBS=2）。付録Aの合成ページ（416,497バイト、2000商品）。best-of-5、各計測の前に1回のウォームアップ。計測コード: `crates/rustscrapling-select/examples/bench_appendix.rs`。

| 処理 | RustScrapling | 本家（同一VM・同一日、BASELINE.md） | 段階目標 | 判定 |
|---|---|---|---|---|
| 解析（互換モード libxml2） | 13.5 ms | 8.5 ms | 15 ms以下 | 達成 |
| 解析（既定モード html5ever） | 16.5〜17.0 ms | 8.5 ms | 15 ms以下 | 未達（下記） |
| `css('.price::text')`（html5 DOM） | 0.74 ms | 10.3〜10.6 ms | 5 ms以下 | 達成（本家の約1/14） |
| `css('.price::text')`（libxml DOM） | 0.81 ms | 10.3〜10.6 ms | 5 ms以下 | 達成 |
| `xpath //span[@class='price']`（参考） | 1.5 ms | （未測） | なし | 参考値 |

html5ever 解析の未達について（Explain the Number）: 同じページで html5ever→RcDom のみの計測は 12.5 ms の回があり、セッション中の他作業（並行ビルド等）と4コアの競合で 16〜17 ms まで振れる。支配要因は html5ever のトークナイザとツリー構築そのもので、自前 TreeSink 化とワンショット入力でも 16.5 ms が下限だった。互換モードは目標内であり、段階8で criterion による管理された再測定を行う。目標未達として隠さず記録する。

選択の一致: `cargo test -p rustscrapling-select` の fixture 試験で、互換モードが oracle の正解データ88件（CSS 59 + XPath 29、エラー症例含む）と 100% 一致。既定モードとの差30件は docs/COMPAT.md に分類済み（tbody 補完と空白テキストの2クラスのみ、選択要素の集合に差なし）。セレクタ生成は本家の test_selector_generation の8症例＋id 短縮2症例を css/xpath・full/短縮の全組合せと往復選択で再現した。

## 段階2 adaptive（2026-11-02、Windows PC上の WSL2）

**互換性検証:** 本家固定コミットから生成した `fixtures/stage2_*.json` と `fixtures/oracle_storage.db` を使用。WSL Ubuntu 24.04、Rust 1.99.1、libxml2 2.9.14 を利用し、`cargo test -p rustscrapling-adaptive -p rustscrapling-cli --offline` で、Unicode文字列・タグ列・autojunk境界を含む `SequenceMatcher` 10,000件×3種が Python の `difflib` と **30,000/30,000件完全一致**。独立した Python 3.12 の `round(x, 2)` 正解データ **8,003/8,003件一致**。正解データの `element_to_dict`、`find_similar`、`find_by_text`、`find_by_regex`、スコア、`relocate(percentage=0,40)`、登録可能ドメイン、Python本家の SQLite BLOB 読込を照合。CLIの `select` / `similar` / `relocate` / `select --adaptive` を本家DBで実行し、さらに `--auto-save` でRustが保存したSQLite BLOBの読み戻し（BLOB/TEXT両対応）も通過。`python oracle/verify_stage2.py` による本家側からRust生成DBへの逆方向読込・JSONバイト単位の一致・再特定にも成功。**注意:** ローカルの別途インストール済み Python Scrapling は 0.4.9 であり、計画が要求する0.5固定コミットと同じものではない。追加の `round` 正解データはCPython標準関数だけを使って生成し、既存のScrapling正解データは再生成していない。

**性能測定条件:** AMD Ryzen 7 7735HS（8コア16スレッド）、WSL2 Ubuntu 24.04、メモリ7.8GiB、Rust 1.99.1。`[profile.release] lto = true`、`CARGO_BUILD_JOBS=2`、Rayonは既定スレッド数、libxml2互換DOM、付録Aの2000商品・約416KB合成ページを使用。`cargo run --release -p rustscrapling-adaptive --example bench_adaptive --offline`。`std::time::Instant` による各1回ウォームアップ後の best-of-3（relocate）、best-of-5（検索）、HTML解析時間は除外。共有Windowsホスト上の他のCargoビルドと負荷が重なったため変動があり、Criterionによる負荷を抑えた再測定は未実施。段階1のMuse VMおよび本家基準値との**同一環境・同一日の直接比較ではない**。

| 処理 | 今回の実測（ms） | 段階2目標 | 判定 |
|---|---|---|---|
| `relocate`（最初のreleaseビルド直後） | 111.25 | ≤50 | 未達 |
| `find_by_text`（同上） | 8.03 | ≤5 | 未達 |
| `find_similar`（同上） | 12.65 | ≤3 | 未達 |
| `relocate`（同一バイナリ再実行3回） | 85.50 / 111.94 / 88.86 | ≤50 | 未達 |
| `find_by_text`（同上） | 2.42 / 3.87 / 2.57 | ≤5 | 各回達成 |
| `find_similar`（同上） | 11.01 / 12.06 / 9.77 | ≤3 | 未達 |
| `relocate`（同一兄弟タグの厳密スコア最適化後、1回） | 49.53 | ≤50 | 単回達成、再測定要 |
| `find_by_text`（同上） | 7.01 | ≤5 | 未達 |
| `find_similar`（同上） | 15.09 | ≤3 | 未達 |
| `relocate`（短い入力インデックス＋属性類似度メモ化後のrelease再ビルド直後） | 51.73 | ≤50 | 単回未達 |
| `find_by_text`（同上） | 6.66 | ≤5 | 未達 |
| `find_similar`（同上） | 8.49 | ≤3 | 未達 |
| `relocate`（最終releaseバイナリ再実行3回） | 33.66 / 35.83 / 33.46 | ≤50 | 各回達成 |
| `find_by_text`（同上） | 5.85 / 6.28 / 3.23 | ≤5 | 1/3回達成 |
| `find_similar`（同上） | 9.38 / 6.12 / 6.65 | ≤3 | 未達 |

最終実装は `relocate` を追加実測3回とも50ms以下に収めた。ただし、共有ホストの負荷変動があるため、管理された専用環境での Criterion 計測による確認が残る。`find_by_text` は3回中2回、`find_similar` は3回とも段階2の目標未達。特に類似検索は `str_ratio` の小配列用インデックスと属性値の計算結果メモ化後でも目標と差がある。対象選択クレートの速度改善は段階2担当範囲外であり、この結果を未達として記録する。

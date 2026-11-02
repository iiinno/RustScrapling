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

## 段階2（2026-11-02）

計測条件: 段階1と同じ（Muse VM、release、lto、CARGO_BUILD_JOBS=2、付録Aページ 416,497バイト）。relocate は best-of-3（ウォームアップ1回）、find_by_text / find_similar は best-of-5。計測コード: `crates/rustscrapling-adaptive/examples/bench_adaptive.rs`。この VM は負荷変動が大きく（load average 0.1〜4.7、steal 約5〜7%）、単発の数値は大きく振れるため、複数回の最良値で判定した。

| 処理 | RustScrapling | 本家（BASELINE.md） | 段階目標 | 判定 |
|---|---|---|---|---|
| relocate（並列） | 28.0〜30.7 ms | 5305〜5677 ms | 50 ms以下 | 達成（本家の約1/180） |
| find_by_text | 0.91〜1.09 ms | 67.8〜70.0 ms | 5 ms以下 | 達成 |
| find_similar | 1.40〜1.43 ms | 42.5〜43.0 ms | 3 ms以下 | 達成 |

正しさの照合（すべて oracle の正解データと一致）:

- SequenceMatcher 比率: ランダム生成3万件（文字列・タプル・長さ150〜420の長文）で difflib と完全一致。
- element_to_dict: general / filter 文書の全要素辞書が本家と完全一致。
- find_similar / find_by_text / find_by_regex: fixture の全ケース一致（閾値0.9で空、partial・大小文字・clean の各組合せ含む）。
- storage: 本家が書いた DB を Rust が読んで relocate 一致。逆向き（Rust が書いた DB を本家が読む）は `oracle/verify_stage2.py` で JSON バイト同一と relocate 一致を確認。
- CLI スモーク: select（css/xpath/text）、similar、保存→ページ改変→relocate の一巡を通した。この過程で「Rust が書いた DB を Rust が読めない」不具合（element_data の TEXT/BLOB 非対称）を検出して修正し、回帰テスト `storage::tests::rust_written_db_roundtrips` を追加した（詳細は COMPAT.md 5節）。

性能の内訳と教訓（Explain the Number）:

- relocate の支配コストは siblings 比率だった。候補ごとに「元の要素の兄弟1999件」と候補の兄弟を SequenceMatcher で比べるため、候補約1万件 × 兄弟走査が効く。兄弟リスト単位のメモ化（異なる兄弟リストは実ページでは数種類しかない）と、親ステージの構成要素（タグ比率・属性 dict_diff・親テキスト比率）メモ化で逐次 167 ms → 並列 28 ms まで短縮した。メモ化は構成要素を分けて保持し、加算順序を変えないためスコアはビット単位で不変（fixture の個別スコア照合で確認済み）。
- 途中の大きな落とし穴: SequenceMatcher の DP 行バッファに `SmallVec<[usize; 64]>`（インライン64要素＝512バイト）を使い、`mem::swap` で入れ替えたため、行ごとに約3KBのコピーが発生し relocate が約6倍に悪化した（逐次 164 ms → 958 ms）。参照の入れ替え（ping-pong）に変えて解消。バッファの値渡し swap はインライン容量に比例する。
- 同じく、Scratch バッファを matcher 構造体へ移した際に find_longest_match 呼び出し間のリセット漏れがあり、古い行の値が次の呼び出しの初行に混入した（difflib との照合テストが検出）。呼び出し冒頭で touched リストに基づき両バッファを初期化して修正。

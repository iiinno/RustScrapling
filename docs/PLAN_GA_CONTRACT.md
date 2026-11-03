# Plan.md — RustScrapling 完成計画 / Acceptance Contract

> **Revision 3 / 2026-11-03 / 作業の正本（Single Source of Truth）**
>
> **目標：Python Scrapling v0.5の実用機能をRustで完全に再実装し、互換性を検証したうえで、取得成功率・待ち時間・出力品質・継続運用の堅牢性において本家と同等以上のシステムを完成させる。**
>
> 実装が存在すること、コマンドが登録されること、モックテストが通ることは、単独では「完成」を意味しない。本書に定めた機能契約・実機E2E・本家差分試験・安全性・性能計測が揃った場合にのみ完成を宣言する。

| 項目 | 固定内容 |
| --- | --- |
| 製品 | **RustScrapling**（CLI: `rustscrapling`、crate prefix: `rustscrapling-`） |
| 仕様の正本 | [D4Vinci/Scrapling](https://github.com/D4Vinci/Scrapling) **`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`**（2026-11-01時点のv0.5基準） |
| 既存の設計判断 | `docs/DECISIONS.md` と本書第5章を保持。判断の変更は記録と承認が必要 |
| 現在の統合基点 | `integration/plan-v2-recovered` / `4d1ed34`（本書作成時のスナップショット。以後のHEADを確認し直す） |
| 完成の定義 | **GA Gate**（第3章・第12章）を全件満たし、`docs/RESULTS.md` に再現可能な証跡があること |
| 開発優先順位 | 取得成功率 → 待ち時間 → 出力品質 → 資源効率。ただし安全性・正確性・秘密保護は犠牲にしない |
| 主な運用 | 1調査約150ページ、反復取得50ページ追加、複数AIエージェントの並列利用 |
| 実行形態 | 同じRustコアに基づくCLI・常駐daemon・MCP（stdio、および設計上のHTTP transport） |

## 0. この文書の使い方

- **本書は「何をもって成功とするか」の契約**。`docs/IMPLEMENTATION_STATUS.md` は進捗台帳、`docs/RESULTS.md` は検証証跡、`docs/COMPAT.md` は挙動の差、`docs/DECISIONS.md` は意思決定の履歴とする。矛盾した場合は、まず実装・テストを調べ、黙って本書を書き換えずに差分を記録する。
- 計画時点の事実と将来の要件を混ぜない。`[x]` は**指定の統合コミットで検証済み**のときだけ用いる。部分実装・未実施・環境障害は`[ ]`を維持する。
- ステージ間の並行開発は可。ただし各機能の成功条件を満たす前に「完成」と扱わず、統合・リリースのゲートを飛ばさない。
- Python版の**公開された利用機能**をRust API、CLIまたはMCPのいずれかで等価に表現できること。Pythonクラスの文法・内部実装の逐語移植は不要。基準の凍結日は固定であり、今後のScrapling最新版への追従はこのGAの範囲外。
- 上流の仕様に曖昧な点があれば**固定コミットのソース・テスト・実行結果の順**に確認。推測によるAPI名や挙動を正式仕様にしない。
- 進捗評価の値は`not_started / partial / implemented_unverified / verified / blocked / intentionally_excluded`。`verified` にはテスト名・実行環境・統合SHA・ログを必須とする。
- 不具合修正で検証ゲートを緩めない。テストの無効化は理由・対応Issue・再有効化条件を記録した例外に限る。

## 1. 目的、範囲、非目標

### 1.1 最終製品のあるべき姿

1. 一般的なHTML、JavaScript描画ページ、難しいサイトを**適切な取得手段**で取り、必要なら自動で段階的に切り替える。
2. CSS・XPath・テキスト・正規表現・adaptiveを通して、構造変化後でも必要な要素を抽出できる。
3. HTTP、ブラウザ、クローラ、MCP、CLI、daemonの機能が同じ型・安全境界・出力契約に接続される。
4. 150ページ程度の調査と50ページ程度の再訪問で、ブラウザ再利用、ETag/Last-Modified、学習済み取得tier、適切な並列性を使う。
5. ページ本文・タイトル・著者・日付・正規URL・Markdown・関連度を、エージェントが扱いやすい**不信データ**として返す。
6. 中断、再起動、サイトの失敗、ネットワークやプロキシの問題、複数エージェントの並走が起きても、安全に継続・復旧できる。

### 1.2 完全互換の意味

**機能互換**とは、固定したPython版v0.5の公開機能・引数・既定値・成功時の意味・失敗時の意味を、Rust版の対応API/CLI/MCPで実行でき、差分テストで同等と認められること。次の内容を含む。

- Selector・DOM・CSS/XPath・擬似要素・text/tail・適応的再特定・保存。
- Fetcher（HTTP）・DynamicFetcher・StealthyFetcher相当の取得・ブラウザ操作・状態・ネットワーク。
- Spiderと6テンプレート、`parse`/Request/Response/Item、LinkExtractor、再試行とresume、出力形式。
- 本家のMCP **16ツールの公開名・説明・スキーマ・各引数の挙動**、ブラウザアクションの全種別。
- Python版CLI・シェルの**利用可能な機能の等価経路**。CLI文法はRust用体系でもよいが対応表を作る。
- Rust側の追加機能（auto tier、学習、daemon、結果の順位付け等）は、本家の意味を壊さず別途検証する。

**対象外（既存判断を維持）**：

- `integrations/scrapy.py` のScrapy接続、Python向けの`PyO3`公開API、Pythonクラス構文そのもの。
- `IPython`実装・依存そのもの。ただし`scrapling shell`が提供する対話的な探索の**機能用途**はRust REPL/対話UI等で満たす。
- 有料CAPTCHA代行サービス連携、独立したV8-only取得エンジン、本家にないadaptive重み学習。
- これら以外の公開機能を「難しい」「RustではPythonと異なる」だけで対象外にしない。変更は`docs/DECISIONS.md`に明記し所有者判断を要する。

### 1.3 対象プラットフォーム

- **必須GA**：Ubuntu Linux x86_64上で、ビルド、CLI、daemon、MCP、Chrome for Testing/headed・headlessの再現可能な動作を実証する。
- **互換性管理対象**：Windows 11 x64。ビルド依存（libxml2/clang/暗号ライブラリ）・データパス・IPCを整理し、対応可否を明記する。Windows対応をGAで名乗るには**専用Windows runnerでの同じ合否ゲート**が必要。
- 現行作業の運用制限により、**Windows/WSLではCargo/Rustビルドを実行しない**。この制限を性能や互換性の合格根拠として読み替えない。実行許可が変わるまでVPSまたは専用Linux CIで検証する。

## 2. 固定仕様から機能一覧を確定する（最初の必須成果物）

### 2.1 Inventoryの作り方

`oracle/` に固定SHAの本家ソースを隔離配置し、次を**コードとテストの両方**から機械的に列挙する。

| 領域 | 棚卸し対象 | 最低限の比較項目 |
| --- | --- | --- |
| Parser/Selector | exports、`Selector`、`SelectorList`、`Adaptor`、探索/生成ヘルパー | API名、引数、既定値、結果型、空結果、例外、順序、XPath/CSS挙動 |
| Adaptive/Storage | `save`、`retrieve`、`relocate`、`find_similar`、保存関連 | JSONキー、SQLite、URL/ドメイン境界、スコア丸め、候補選択 |
| HTTP | `Fetcher`と`AsyncFetcher`相当の公開操作・session/ProxyRotator | HTTP method、header/body/cookie、TLS/UA、proxy、timeout、retry、redirect |
| Browser | Dynamic/Stealthy fetcher、page/context、状態・CDP操作 | 起動引数、ナビゲーション、JS、入力、通信履歴、保存/復元 |
| Spider | Spider/Request/Response/Item、link middleware、6テンプレート | callback、request metadata、並列、block/retry、robots、pipeline、出力 |
| MCP | `scrapling/core/ai/server.py` と browser action実装 | 16ツール、説明、JSON schema、全action variant、return/error |
| CLI/Shell | `cli.py`、`core/shell.py` | サブコマンド、flags、入出力、interactive用途 |

**作成・維持する証跡（新規ファイルはステージM0で作成）**：

- `docs/compat/UPSTREAM_API_V05.json`：各公開APIの機械可読リスト。`id / upstream_source / signature / defaults / expected_semantics / rust_entrypoint / test_ids / status / exception`。
- `docs/compat/PARITY_MATRIX.md`：利用者向け全機能の対応表。`implemented`と`verified`を分離。
- `docs/compat/MCP_ACTIONS_V05.json`：Python版から抽出した16ツールとactionの完全スキーマ。**現時点の「12種」という記述は棚卸しで確認するまで暫定**。
- `fixtures/oracle/`：固定SHAから生成したHTML・Response・MCP・Spider・ブラウザ操作の決定的期待値。fixture生成コード・入力データの出典・乱数seedを同じコミットで保持する。

**Coverage Gate**：棚卸しリストの各`id`に、対応実装と自動テストまたは承認済みの`intentionally_excluded`が**必ず1件以上**存在する。未マップのAPI・サイレントな`unsupported`・名前だけの実装が1件でもあればGA不可。

### 2.2 仕様とテストの優先順位

1. 本家固定SHAの公開APIとソース。
2. 本家テストの期待値（実行時の前提も保存する）。
3. 同一入力に対するPython本家とRust版の実測比較。
4. Rust独自の追加機能の設計文書。
5. 仕様外の細部はセキュアな振る舞いを優先し、差分を`docs/COMPAT.md`に残す。

Python内部のprivate関数は網羅の分母に含めない。ただしその振る舞いを使う公開機能に必要な部分は実装する。複数表現のAPIは対応IDを集約してもよいが、**引数の組合せの差**は試験対象から除かない。

## 3. Definition of Done — 成功を宣言するための合格条件

### 3.1 GA必須ゲート（全てAND）

| ID | 合格条件 | 合格証跡 |
| --- | --- | --- |
| G-01 | 第2章のInventoryにおける対象公開機能を100%対応・差分試験で`verified`（例外は既定で0、明示した第1.2節の対象外だけ許可） | `PARITY_MATRIX.md`、fixture、差分テストログ |
| G-02 | CLI・daemon・MCPが1つのバージョンから同一コアを呼び、全公開操作・主要な異常系を実E2Eで通す | 別プロセスE2E、JSON-RPC、再起動/同時実行のログ |
| G-03 | Chrome155/156の実機E2E（localhostと外部検証サイト、headed/headless、セッション再利用）が安定し、Python版のブラウザ機能に相当する操作を実証 | バージョン別テスト、ブラウザ起動ログ、結果ファイル |
| G-04 | 約200URLの同日・同マシン・同IP・同プロキシ・同時間制限で、全体とカテゴリ別の**取得成功率が本家以上** | `bench/corpus.toml`、raw outcome、比較スクリプト |
| G-05 | 同一150URL・同一並列数8・同一取得条件の完了時間が本家以下。失敗URLを黙って落として高速化しない | 反復計測・p50/p95・成功数・プロセスRSS |
| G-06 | adaptive互換スコア/再特定がoracleと**100%一致**。1万要素ページの`relocate`が50ms以下、`find_by_text` ≤5ms、`find_similar` ≤3ms | 再現可能なrelease計測＋oracle diff |
| G-07 | 本文抽出は本家`main_content_only`の評価対象語を95%以上保持し、**同等の入力ページにつき**出力文字数は本家以下。記事欠落や有害なスクリプト混入がない | 固定記事コーパスの比較ログ、失敗例レビュー |
| G-08 | 150ページ並列8のRust本体の**最大RSS ≤256MiB**（Chrome/OS cacheは別計測）。メモリ越え、巨大HTML、誤った再試行で無制限に蓄積しない | process tree RSS、負荷ログ |
| G-09 | HTML parser・CSS/XPath・adaptive・Markdownそれぞれ**1時間fuzz**（ASan/UBSan等が可能なら併用）、クラッシュ/メモリ違反0件 | seed/corpus、CIログ、検知結果 |
| G-10 | 24時間の連続稼働でセッション管理・再試行・停止/再起動・キャッシュを繰り返し、未回収リソース増加や秘密漏えい、データ破損なし | 24hメトリクス、バグ判定、再実行条件 |
| G-11 | セキュリティ必須テスト全件PASS。未認証IPC、秘密情報の出力、資格情報付きURL混入、悪意ある状態ファイル、意図しないローカル権限拡張なし | threat model、負例・回帰ログ |
| G-12 | サポート対象で`fmt`・strict`clippy`・`test --workspace`・CLI E2E・MCP実機E2E・リリースバイナリsmokeが緑。意図的skipは個別承認がない限りGA不可 | pinned SHA、環境、exit code、ログ |
| G-13 | README/CLIヘルプ/API・schema/設定移行/NOTICE/SBOM・ライセンスが実装と一致し、再現ビルド・導入手順が第三者に再現可能 | 新規環境再現テスト、成果物checksum |

**GA判定方法**：各G項目に`PASS / FAIL / NOT_RUN / BLOCKED`と実際のログURL・SHAを書く。`NOT_RUN`、`BLOCKED`は`PASS`ではない。承認なしのゲート除外、前回ビルドの合格ログ流用は禁止。

### 3.2 ストレッチ目標（必須条件とは混同しない）

- 取得成功率を本家より**+5ポイント以上**向上。
- 150ページ処理のp50/p95短縮と、50ページ再訪時のキャッシュ・daemon再利用による短縮。
- Python版を上回る候補ランキング・抽出ノイズ低減・障害からの自己復旧。
- メモリ上限・CPU負荷を保ったまま、複数エージェント並列のスループット向上。

上記は実測が良好な場合だけ達成と表示する。+5pt未達でもG-04を満たせば「機能互換GA」は可能だが、「本家を取得成功率で上回った」という主張は不可。

### 3.3 比較の定義を固定する

- **成功率**＝`success / attempted`。attemptedは規定corpus内で試行対象に含めた全URL。timeout・blocked・captcha・無効本文・unsupportedも失敗。ネットワーク由来の例外除外は、**両実装に同じ条件を適用し理由と件数を公表**。
- **成功判定**＝HTTP結果だけでなく、カテゴリ別にstatus・bot challengeでないこと・本文/指定要素の最低充足を判定する。JSページは描画後DOMを使う。
- **150ページ所要時間**＝全入力を処理して最後の結果を確定するまで。開始/終了時刻、並列数8、timeouts、失敗数、キャッシュ暖機条件を揃える。異なる成功件数の速度比較で「勝利」としない。
- **精度**＝英日・技術文書・表を含む注釈付き記事corpusで、Unicode正規化・空白・記号の扱いを固定した語彙再現率。日本語は事前に定義した同一tokenizerを両側へ使用。比率と記事単位での外れ値を記録。
- **資源**＝RustScraplingのPID/子プロセスとChromeのPIDを分離。ビルド時RSSと実行時RSSを混同しない。CPU・メモリ・温度・プロキシ等の測定条件を併記。
- すべての評価は**本家とRust版を同一日・同マシン・同ネットワークで交互実行**し、実行順序バイアスを抑える。少なくとも3反復し、結果一覧・分布を保持する。

## 4. アーキテクチャと各クレートの責務

`CLI / MCP / daemon IPC → Fetch / Spider / Selector → HTTP / Browser / DOM / Adaptive / Extract`。ブラウザとHTTPの二重実装をそれぞれの上位層で増殖させず、同じ制限・error型・URL検証を共有する。

| クレート | 責務 | 固有の合格条件 |
| --- | --- | --- |
| `rustscrapling-dom` | html5ever既定DOM、libxml2互換DOM、arena/属性順序/text/tail/Shadow DOM連携用表現 | 本家fixture・DOM不変条件・fuzz |
| `rustscrapling-select` | CSS、XPath1.0、`::text`、`::attr()`、regex/text、selector generation | 元ページと変更ページの選択差分oracle一致 |
| `rustscrapling-adaptive` | 指紋、SequenceMatcher/丸め、保存/移行、retrieve/relocate/similar | 3×10,000 + 境界値＋SQLite相互運用＋性能 |
| `rustscrapling-extract` | 主本文、Markdown、安全URL、metadata/JSON-LD、BM25 | 記事コーパス・secret/active markup除去・品質 |
| `rustscrapling-http` | HTTP1/2/3、TLS指紋、cookies/proxy、retry、stream/timeout | Fetcherの引数・wire挙動・実/ローカルserver |
| `rustscrapling-browser` | CDP page/context、stealth、ナビ・入力・フレーム・状態・通信記録 | Chrome155/156の実E2E、全action、上限と秘匿 |
| `rustscrapling-fetch` | 共通Response/Request/error、自動tier、block判定・fallback | 同一URLの直/daemon利用で結果契約一致 |
| `rustscrapling-spider` | frontier、並列/robots、テンプレート、callback、checkpoint、pipeline | 同等のSpiderケース・resume・多セッション |
| `rustscrapling-daemon` | 認証IPC、セッション/ブラウザpool、cache、学習、同時要求集約 | restart、権限隔離、再利用、複数client |
| `rustscrapling-mcp` | 16ツール、schema/description/JSON-RPC、適切なservice lifetime | 上流のツール別契約＋実Chrome/HTTP E2E |
| `rustscrapling-cli` | `get/post/put/delete/fetch/stealth/auto`、selector、crawl、session、network、daemon、MCP、install/REPL | stdout/exit/error/schema、daemon/直接経路一致 |

- Rust実行本体はPython不要。Pythonは`oracle/`と差分評価スクリプトに限定する。
- 既存選定：tokio、rayon、アリーナDOM、html5ever、互換libxml2、wreq/wreq-util、HTTP/3のquinn+h3、chromiumoxide CDP低レベル経路、htmd、rmcp、rusqlite。
- `Runtime.enable`はstealth pageのDOM読取時に**自動送信しない**。明示evaluateの有無を分けて検証する。検知回避を保証する宣伝は実サイト比較まで禁止。
- ブラウザの有効なセッションを**プロセスを越えて利用**する場合はdaemonまたは永続MCPサーバが実体を持つこと。IDだけ返す偽物のsessionは重大な不具合。
- 設定変更・フォールバックは観測可能なtraceを返しつつ、cookie/token/queryなど秘密情報はログへ出さない。失敗は`typed error`で伝える。

## 5. 変更しない設計判断・ライセンス・安全境界

以下は旧Plan第5・14章、`docs/DECISIONS.md`の既決事項を引き継ぐ。

1. 名称`RustScrapling`を採用。既存の`RUSTScrapling`と紛らわしいことを承知する。現時点でcrates.io公開を前提にせず、公開する場合は命名衝突・商標/パッケージ名を別途確認する。
2. robots.txt順守は本家互換のため**既定OFF**。`--obey-robots` / `obey_robots = true`で有効化可能。利用者には対象サイトの条件・負荷への配慮を案内する。
3. 標準の計測並列数は8。これを勝手に変えて成功率/速度を比較しない。
4. 既定DOMはhtml5ever、互換モードはlibxml2。`text`/`tail`と要素順序の差は`docs/COMPAT.md`で明確化。XPath1.0はarena DOM用の実装と互換側の参照経路を区別する。
5. adaptiveスコアと丸めはPythonの`difflib.SequenceMatcher`（autojunk含む）/`round`に一致させる。PSLによる登録可能ドメイン境界、同じSQLite保存形式とする。
6. HTTPは`wreq`＋`wreq-util`、HTTP/3は`quinn`＋`h3`の別経路。プロキシ・証明書の検証を既定で弱めない。
7. ブラウザはCDP低レベル経路を優先。上流`patchright`との取得成功率比較に劣る場合のみ、Node.jsの`patchright`ドライバ子プロセス案を`docs/DECISIONS.md`で評価する。**Rust本体からPythonへの依存は追加しない。**
8. コードの出典・借用箇所・ライセンス義務を`NOTICE`と`licenses/`に残す。Python本家BSD-3-Clause、参照Rust移植MIT/Apache-2.0。ライセンス表記なしのコードの丸写しは禁止。
9. Rust由来の追加機能（daemon、auto tier、BM25等）は**互換モードの振る舞いを上書きしない**。追加機能の既定値変更は移行手順と差分試験が必要。

## 6. 機能別の完成契約（Acceptance Criteria）

以下のIDを`docs/compat/PARITY_MATRIX.md`のテストIDに対応付ける。表の「完成」とは、**ユニット＋差分テスト＋必要な実E2E**がすべて通ったことを指す。下記はサマリーであり、未列挙の本家公開引数は第2章Inventoryによって自動的に追加される。

### 6.1 DOM・Selector・XPath（D-01〜D-09）

| ID | 完成条件 |
| --- | --- |
| D-01 | 本家の`Selector`/`SelectorList`相当の生成、配列/単一結果、`get`/`getall`、テキスト/属性/HTML取得、空集合・エラー処理を網羅 |
| D-02 | HTML5標準の木構築とlibxml2互換の木構築。コメント・NUL・空白・tbody・不正HTML・Unicodeをoracleと比較 |
| D-03 | Elementの`text`（最初の子要素前）と`tail`（兄弟間）・`text_content`が互換モードで一致 |
| D-04 | CSSセレクタ全対象構文、擬似要素`::text`/`::attr(name)`、複合/不正構文のエラーが上流と一致 |
| D-05 | XPath1.0の軸・述語・変数・関数・名前空間・ノード集合とスカラ返却・不正式の挙動を対象fixturesで比較 |
| D-06 | find_by_text/find_by_regexとfirst_match・exact等、本家の公開引数を網羅。Python`re`にしかない構文は等価エンジン追加または明示した対応が必要 |
| D-07 | selector生成（CSS/XPath）、相対セレクタ・兄弟・位置・同名要素が構造変更後も意味のあるものを返す |
| D-08 | 同じHTMLの反復解析で結果・順序・文字表現が決定的。大規模入力でCPU/メモリがbounded |
| D-09 | 既存oracle88件（CSS59+XPath29）を維持し、Python版の全関連テスト/エッジケースへ拡大。選択結果の相違は0件 |

### 6.2 Adaptive・保存（A-01〜A-08）

| ID | 完成条件 |
| --- | --- |
| A-01 | `element_to_dict`のJSONキー・属性順・空値の扱い・子タグ・text/tailが本家一致 |
| A-02 | Unicode文字列、要素パス、200要素境界を含む3×10,000のSequenceMatcher oracleに完全一致 |
| A-03 | Pythonのbanker's rounding、閾値境界、採点順序、同スコアの安定tie-breakを完全一致 |
| A-04 | `save/retrieve/relocate`が`--compat`の同じ入力に同じ要素・スコアを返し、タグ・クラス・属性・位置変更に対応 |
| A-05 | `find_similar`の候補数・順位・percentage、`find_by_text`・regexなど公開の検索APIを実装 |
| A-06 | 登録可能ドメイン/サブドメイン・IP・punycode・異なるURLの識別境界をテスト |
| A-07 | 本家SQLite `storage(url, identifier, element_data)`とBLOB/TEXT双方向互換、破損・並列書込・データ移行をテスト |
| A-08 | `relocate≤50ms / find_by_text≤5ms / find_similar≤3ms`のrelease計測に合格。正確性のためにスコア定義を変えない |

### 6.3 本文抽出・AI向け品質（E-01〜E-08）

| ID | 完成条件 |
| --- | --- |
| E-01 | HTMLから`main_content_only`相当の主本文を抽出。記事・一覧・技術文書・短文・table・コード・多言語の固定corpusで比較 |
| E-02 | Markdownの見出し、リンク、画像、表、コード、引用、リスト、空行が本家用途を損なわない。HTML/plain textも一貫 |
| E-03 | 非表示要素、nav/footer/広告、script/style、unsafe URL、画像pixel等の除外。表示されている悪意の指示は**不信データ**であり無害と断言しない |
| E-04 | HTML/属性/URL・entity/制御文字/リダイレクトやcredential URLのテスト。secretをmarkdown/html/metadataへ混入させない |
| E-05 | title・description・author・公開/更新日時・canonical・OG・JSON-LD（`@graph`/参照解決含む）を抽出 |
| E-06 | BM25 paragraph/page rankは日本語CJKを含め再現可能、安定tie-break、クエリなしの文書順を保持 |
| E-07 | 本家`main_content_only`の注釈語彙再現率≥95%、文字数は対応する本家出力以下。個別の大量欠落例をレビュー |
| E-08 | 既存の抽出強化コミット`bcc36fa`/`b899edb`は**未統合**として扱い、統合後に単体＋全体回帰を再実行して初めてverified |

### 6.4 HTTP・Proxy・HTTP/3（H-01〜H-12）

| ID | 完成条件 |
| --- | --- |
| H-01 | GET/POST/PUT/DELETEおよび本家Fetcher/AsyncFetcherの公開method・引数・戻り値（必要ならRustで等価なAPI） |
| H-02 | headers、query、body/form/json、multipart、cookies、session継続、HTTP状態と最終URLの保持 |
| H-03 | browser fingerprint/impersonation、stealthy headers、UA、一貫したHTTP/2/TLS、`http3`指定時のQUIC実送信と正確なprotocol表示 |
| H-04 | proxy、`proxies`、環境変数proxy、認証proxy、rotation。宛先・scheme・redirectが変わっても認証情報を漏らさない |
| H-05 | redirect/follow_redirects、method変更、cookie保持、Authorizationの転送境界、無限redirect、credential-bearing Location拒否 |
| H-06 | retries/timeout、DNS/TLS/Proxy/connection/read/write timeoutの型付きエラー、キャンセル、Retry-After、429/503を決定的なローカルserverで確認 |
| H-07 | gzip/br/zstd等、charset/Unicode/バイナリ、本家Responseのstatus/url/header/body/text/selector/markdownへの接続 |
| H-08 | 各レスポンスサイズ上限、streaming、巨大・遅い・途中切断・chunked・compress bomb耐性 |
| H-09 | TLS検証は既定ON。無検証モードは本家互換の明示オプションがある場合のみ実装し危険を表示 |
| H-10 | localhost/IPv4/IPv6、HTTP/1.1・2・3、プロキシ直結/失敗の再現E2E |
| H-11 | 固定URLカテゴリのHTTP取得成功率を本家と比較し、認識できない差分を列挙 |
| H-12 | 経路の機密値はstderr/trace/MCP/daemon履歴に平文で露出させない |

### 6.5 Browser・Stealth・CDP（B-01〜B-16）

| ID | 完成条件 |
| --- | --- |
| B-01 | headless/headed、Chrome binary/外部`cdp_url`、複数browser/page/context、起動/終了/タイムアウト/クリーンアップ |
| B-02 | navigator、DOM、shadow DOM、iframe（同一origin/クロスoriginの可否を区別）、frames内要素参照/selector |
| B-03 | request interception、network events（URL/method/status/headers/body）、安定ID・単調cursor・bounded履歴・欠落数と`has_more`の正確性 |
| B-04 | XHR/FetchのテキストAPI本文を安全に取得、binary/大容量/秘密・不明JSONは保守的に扱う。データ欠落も明示 |
| B-05 | cookie、localStorage、sessionStorage、IndexedDB等、Python版が保存する状態に相当する範囲を永続化・復元・隔離 |
| B-06 | browserforge相当のfingerprint/headers、広告ドメイン遮断、proxy/認証proxy、DoH、ブラウザ起動引数を上流に照らして評価 |
| B-07 | `Runtime.enable`の暗黙送信がないことをwire traceで確認。JS評価が必要な場合は明示区別 |
| B-08 | Cloudflare等のチャレンジ検知・本家`_cloudflare_solver`相当の制御。解けない場合は`blocked/captcha`などの正しい失敗を返す |
| B-09 | セッション内page再利用、複数page、同時操作、操作対象の識別・閉鎖、既存コンテキスト再接続 |
| B-10 | DOM経由click、可視/無効/他要素遮蔽の扱い、座標・ボタン操作・スクロール・focusを実Chromeで確認 |
| B-11 | native input、textarea、contenteditable、清掃/追記、日本語Unicode入力、選択・キーボード・checkbox/radio/combobox/dialogの全上流操作 |
| B-12 | CSS/ARIA/ref selectorとframe target、要素指定screenshot、PNG/JPEG、viewport/regionの異常値検証 |
| B-13 | navigation/load状態、JS描画後wait、timeout、ページ遷移、ページクローズ、エラー復旧を実機確認 |
| B-14 | Chrome155/156のlocalhost HTTP不安定を**本番で安全に解決**。テスト専用`--password-store=basic`は一般ユーザープロファイルへ自動適用しない |
| B-15 | 同一日にPython StealthyFetcher/DynamicFetcherと難易度別サイト成功率を比較。防御の突破率を根拠なしに宣言しない |
| B-16 | CDPのbrowser/context/page lifecycle、proxy credentials、state、network本文、全アクションのE2EをMCP経由でも確認 |

**B-08に関する境界**：認証やアクセス権限のないページを強行突破することは目標ではない。第三者サービスへの負荷・規約・法令・利用許諾に配慮し、検証は管理できるページや許可された公開対象を優先する。

### 6.6 Fetch auto・フォールバック（F-01〜F-07）

| ID | 完成条件 |
| --- | --- |
| F-01 | HTTP → ブラウザ → headed等の複数tierを、設定された時間/試行上限の中で選択 |
| F-02 | block/403/429/challenge/空本文/不正本文を正確に判定し、成功として履歴を汚さない |
| F-03 | 次tierへのfallback条件・retry回数・proxy選択をtraceに残す。機密値を残さない |
| F-04 | サイトごとに成功tierを学習（既定TTL24時間）、期限切れ・失敗時リセット、永続復元 |
| F-05 | ETag/Last-Modifiedと304、cache keyのmethod/header/privacy区別、破損キャッシュの扱い |
| F-06 | daemon有無で`Response`/metadata/errorの契約が一致。差分は性能や明示service metadataに限定 |
| F-07 | 150URL反復取得時、ブラウザpool/リクエスト同時集約/速度制御が有効に働くことを測定 |

### 6.7 Spider・Crawl（S-01〜S-13）

| ID | 完成条件 |
| --- | --- |
| S-01 | CrawlSpider / SitemapSpider / XMLFeedSpider / CSVFeedSpider / ShopifySpider / ContentSpiderの6テンプレートが本家の用途と一致 |
| S-02 | async`parse(response)`、カスタム`Request`生成、複数Item出力、parseからの追加Requestが共通frontierへ安全に接続 |
| S-03 | RequestはGETに限定しない。method、body、headers、cookies、priority、session、metadata、callback、errbackを持てる |
| S-04 | LinkExtractorのallow/deny以外の本家公開規則（domain、extension、canonicalize、unique、tags/attrs等）をInventoryで照合 |
| S-05 | middleware/Item pipeline・transform・validation・error処理・同等のhookをRust trait等で提供 |
| S-06 | クロール並列数、domain/origin別速度、request priority、AutoThrottle、Retry-After、失敗retryを統一 |
| S-07 | robots.txtは既定OFF。ON時、User-agent group、Allow/Disallow、Crawl-delay、Request-rate、障害時方針を本家と比較 |
| S-08 | 開発用cacheは明示opt-inで再解析し、通常のcheckpointを汚さない。本文や秘密を安全に保存 |
| S-09 | `crawldir`にfrontier/inflight/visited/item delivery状態を永続化し、クラッシュ・再起動で未処理URLの取りこぼしがない |
| S-10 | 重複排除はURLだけでなく必要に応じmethod/body等を考慮。crawldir＋ファイル出力のクラッシュ復旧E2Eでは**最終結果の欠落0・重複0**が必要。writerとの原子的commitが不可能なstdout等の経路は`at-least-once`＋安定dedupe keyを明記し、本家の保証との違いをGA前に照合する |
| S-11 | JSON/NDJSON/CSV/XMLのstreaming出力が破損しない。途中停止・再開、UTF-8/escaping、失敗Itemの扱いを検証 |
| S-12 | 6テンプレートそれぞれに本家サンプル相当E2Eと公開ページでの差分テスト |
| S-13 | 大規模frontierがbounded。上限超過・不正URL・リダイレクト・クロスドメイン要求が予約slotやrobots制限を回避しない |

### 6.8 Daemon・IPC・セッション（P-01〜P-10）

| ID | 完成条件 |
| --- | --- |
| P-01 | `daemon start/stop/status`、子プロセス起動の成功/失敗/タイムアウト、予期しない終了とstate不整合を扱う |
| P-02 | デフォルトはloopback / OS所有者限定IPC。認証token、権限0600相当、権限昇格/セッションハイジャック拒否 |
| P-03 | `session open/close/list/save-state/load-state`が**実体のあるブラウザcontext**を管理し、CLIを終了・再起動しても契約に従って参照できる |
| P-04 | 複数クライアント同時アクセス、所有者分離、page/context IDの再利用事故を防止 |
| P-05 | 学習tier・cache・状態のSQLite永続化、version migration、競合書込、破損・停電をテスト |
| P-06 | 同一URLの同時実行集約、per-domain rate gate、取消時にも他clientの処理を勝手に破棄しない |
| P-07 | プロセス間でのbounded queue、流量制限、使用期限/アイドル回収、browser poolサイズ |
| P-08 | token/state/cookies/保存bodyを平文ログへ出さず、保存先の権限とcleanupを検証 |
| P-09 | network searchは**browser subresourceとfetch完了メタデータの差**をAPIで明示し、CLI/MCPの同名概念を混同しない |
| P-10 | daemonなし/あり、連続実行/再起動/複数clientのE2Eで、結果内容と失敗分類の整合性を確認 |

### 6.9 MCP・CLI・REPL（MCP-01〜MCP-09、C-01〜C-08）

**本家16ツール（すべてschema＋実executor＋E2Eが必須）**：

| # | ツール | 受け入れ内容 |
| --- | --- | --- |
| 1 | `make_request` | HTTP options、response/error、sanitized output |
| 2 | `browser_fetch` | browser mode、navigation、rendered result |
| 3 | `browser_fetch_once` | 一回限りのbrowser contextと確実なcleanup |
| 4 | `session_make_request` | session内HTTP stateとrequest options |
| 5 | `open_request_session` | HTTP sessionの実体・認証・寿命 |
| 6 | `browser_open` | browser/page sessionを生成・返却 |
| 7 | `browser_save_state` | cookie/storageを含む対象範囲の保存 |
| 8 | `browser_load_state` | 復元・version・隔離・破損時error |
| 9 | `close_session` | idempotent closeまたは正しいunknown-session error |
| 10 | `list_sessions` | 偽IDや閉鎖済みIDを返さない |
| 11 | `browser_network_requests` | bounded list、filtered cursor、機密秘匿 |
| 12 | `browser_network_request` | id指定、headers/response_bodyの実取得 |
| 13 | `browser_extract` | CSS/XPath/text/HTML/attr/result formatting |
| 14 | `browser_actions` | 本家action variantsすべて（schemaのname・args・errorも一致） |
| 15 | `browser_evaluate` | explicit JS、time limit、結果型、危険な無制限実行拒否 |
| 16 | `browser_screenshot` | page/element/scope、PNG/JPEG実バイト、失敗時typed error |

- **MCP-01〜03**：本家説明文・name・JSON schema・enum・defaultをsnapshot差分テストで照合。ツールの名前があるだけで成功にしない。
- **MCP-04〜06**：stdio JSON-RPC initialize/list/call/closeとnotification、非同期/順序、複数session、無効arguments、method-not-foundをテスト。
- **MCP-07〜09**：実Chrome・HTTP・state・network・browser_actionsをMCPクライアントから通し、バイナリ/文字列/JSON/エラーが上流契約に従う。HTTP transportを公開する場合は認証・loopback境界・streamingを別途検証。

**CLI機能群**：`get/post/put/delete`、`fetch`、`stealth`、`auto`、`select`、`similar`、`relocate`、`selector-gen`、`md`、`crawl`、`session open/close/list/save-state/load-state`、`network search`、`daemon start/stop/status`、`mcp`、`install`、`shell`相当の対話探索。

- **C-01**：全コマンドに使用例・型付き引数・`--help`・終了コード・契約テスト。
- **C-02**：stdoutはバージョン付きJSON既定（クロールは明示NDJSONの設計差を記録）。`--format json|ndjson|md|text|html`、`--out`、`--max-chars`でJSON断裂やログ混入を起こさない。
- **C-03**：`error.kind`と`error.hint`、部分失敗、タイムアウト、Unsupported/Blockedの区別。
- **C-04**：CLIからの永続session/ネットワーク/daemon操作が実際のサービスに結び付く。
- **C-05**：`install`はChrome for Testingなど必要部品をバージョン/チェックサム検証付きで導入。副作用・権限・失敗時cleanupを表示。
- **C-06**：本家shellの対話探索用途（ページ取得、DOM/selector試行、抽出結果検査）に等価なRust CLI REPL/対話入口。
- **C-07**：認証情報を含む入力とJSON/NDJSON/Markdownの出力が漏えいしないこと。
- **C-08**：既存`tests/e2e_local.py`を拡張し、別プロセスE2Eでstderr/exit/daemon切替とバイナリsmokeを確認。

## 7. セキュリティ・資源制約・運用保証（非交渉）

### 7.1 Threat model

攻撃面は、(a)不正HTML/JSとページ本文、(b)HTTP応答/redirect/URL、(c)CDP・ブラウザ状態、(d)MCPクライアント引数、(e)daemon IPCとstate file、(f)第三者依存ライブラリ。重要資産はcookie/token/proxy認証・ホストファイル・ブラウザprofile・ログ・AIエージェントの判断。

- URL受け入れはHTTP(S)等の明示schemeに限定。資格情報を含むURL、protocol-relative/bslash、control char、credential-bearing Location、予期しないscheme変更を検査。
- ローカルホストや内部アドレスへのアクセスはテスト/利用用途があるため一律に禁止しない。ただし**外部公開するMCP HTTP/API**では明示allowlistとloopback/private-range制御を設け、任意の第三者が内部ネットワークを読めるSSRFを防ぐ。
- DOMから作ったMarkdown/HTMLが`<script>`等の実行要素を復活させない。取得した可視本文が命令に見えても**AIへの命令権限を持たない**。
- network body・header・URL・screenshot・HAR等は秘匿対象になり得る。原則メタデータのみ、必要なデータは上限/明示指定/フィルタ/権限付きで扱う。ヒューリスティックなredactionを**万能と表現しない**。
- Cookieや状態ファイルのパーミッション、認証、暗号化/OS秘密ストア、ディレクトリtraversal、symlink攻撃、JSON deep nesting、巨大zipや画像をテスト。
- CDPの外部接続は許可されたendpointのみ。実ユーザーの通常Chromeプロファイルへ勝手に接続・変更せず、テスト専用分離profileを使う。
- 資格情報の保存方式を弱める`--password-store=basic`を、本番で無条件に追加しない。試験専用の使い捨てプロファイルに限る。
- 取得の再試行、crawl、browser tab、XHR履歴、HTML解析、IPC request/response、DBをboundedにする。欠落を`dropped_count`/`has_more`/typed errorsで可視化する。
- URLやヘッダーに秘密がある場合、デバッグ/エラーでも平文を出さない。安全な環境変数、権限限定state、回収可能な一時ディレクトリを採用。
- 依存の脆弱性/ライセンスのスキャン、`Cargo.lock`固定、再現可能なビルドをCIで管理。

### 7.2 失敗・再開の意味

「取得に失敗した」は沈黙・空文字成功・偽セッション成功を許さない。`blocked / captcha / timeout / dns / tls / proxy / http_status / parse / invalid_argument / unsupported / cancelled / internal`等の理由を返す。`unsupported`が1件でも第2章の対応対象に残ればGA不可。

Spider再開は **(a) no lost work、(b) bounded duplicate、(c) item delivery semantics**を分離して検証する。exactly-onceが原子的に達成できない場合、再実行可能なdedupe keyを提供し`at-least-once`を正しい契約として残す。完全な再現テスト無しに「exactly-once」と宣言しない。

### 7.3 個別機能の境界

- `Runtime.enable`を送らないことはボット対策の突破保証ではない。
- robots.txtを既定OFFにすることは無制限にクロールする許可ではない。速度制御・対象範囲・明示設定を守る。
- challengeに遭遇した場合、無限retryやproxy乱用で負荷を増やさず停止・fallback・失敗分類する。
- HTTP/3が失敗してHTTP/2へ降格した場合は実際のprotocolを返し、HTTP/3成功と報告しない。
- 外部ネットワークのテストは低頻度・許可されたサンプル主体とし、検証用URL・責任者・制限を管理する。

## 8. 検証環境・負荷試験・ベンチマーク設計

### 8.1 再現性

すべてのログに`git_sha, upstream_sha, datetime_utc, OS/kernel, CPU, RAM, Rust/Cargo, Chrome version, flags, libxml2 version, proxy type, corpus hash, timeout, concurrency, warm/cold, iterations, exit_code`を保存。

**原則**：Windows/WSLでCargoは禁止（現行運用）。VPSの既存サービスを停止/全体killしない。Cargoは専用worktreeと専用scopeで実行し、`nice`、`timeout`、`MemoryMax`、`MemorySwapMax=0`等を設定。Chromeプロファイルは使い捨て。

重要：**1.5GiB制限での全workspace Cargoビルドは`chromiumoxide_cdp`付近でOOMになった実績がある**。これをRustのテスト失敗と混同しない。抽出クレート限定の試験は同制限下で成功。全workspaceについては`docs/RESULTS.md`にある約3GiB上限の既存成功条件を基準に、**VPSの空きメモリ・サービス負荷を先に確認**し、安全な隔離環境/CIを選ぶ。無断でリソース上限を緩めたり既存サービスを停止しない。

### 8.2 約200URL Corpus

`bench/corpus.toml`に約200の**固定URLと機械的な成功条件**を定義。カテゴリはnews / commerce / docs / government / academic / JS-rendered / bot-challenge / other。各entryに`id, category, url, expected_text_or_selector, min_text_chars, allowed_status, needs_js, timeout_s, allowed_for_testing, privacy_class`を持たせる。

- 自動実行可能な公開ページを優先。ログイン必須ページは利用許諾とテストアカウント管理を確認できるものだけ。
- 一部の実サイトが消滅・リダイレクトしても**当日の双方の結果**を記録し、URLを都合よく除外しない。
- 200URLのうちカテゴリごとの件数・成功率・タイムアウトを必ず表示。全体の成功率だけでJS・bot-challengeの弱さを隠さない。
- Python版もRust版も`static / dynamic / stealth / auto`に相当する手段を整理して比較。公平な`best applicable upstream mode`と、同じ固定モード同士の比較を区別。

### 8.3 速度・資源

1. 付録Aの現物`oracle/bench_appendix_a.py`で2,000商品/約1万要素/約416KBのHTMLを再生。
2. `Selector` parse、`.price::text`、`find_similar`、`find_by_text`、`relocate`、Markdownをrelease buildで測定。計測中の他の重いビルド・Chrome等は避ける。
3. `criterion`等でwarm-up、試行分布、p50/p95、メモリ/CPUを記録。同一マシンで本家と比較する。
4. 150ページ並列8をcold・warm・daemon再利用の3条件で測定し、success/failureと時間を一体として評価。
5. 計測値が目標未達の場合、プロファイルして該当クレートを最適化。`compat`の意味・安全制約を崩さない。
6. parserの既定モード15ms以下・CSS`.price::text`5ms以下という従来目標を維持し、再現実測。既存の16.5〜17ms観測は未達として扱う。
7. 横断的な24h耐久でRSS/FD/tabs/browser processes/DBサイズ/履歴とlatencyが単調増加していないことを調べ、停止・障害復旧を再現する。

### 8.4 試験の層

| レイヤー | 最低限の実施内容 | 失敗時の扱い |
| --- | --- | --- |
| L0 静的 | `cargo fmt --all --check`、strict Clippy、licenses/security scan | merge block |
| L1 ユニット | pure parser、score、serialize、URL guard、bounded queue、typed errors | merge block |
| L2 差分 | 固定上流oracleと引数組合せ/結果/エラー比較。property test | 互換機能はmerge block |
| L3 ローカル統合 | HTTP1/2/3 servers、proxy、redirect、daemon IPC、CLI、stdio MCP | merge block |
| L4 実Chrome | Chrome155/156 headless/headed、state、iframe/ARIA/actions/network、MCP | Browser変更のmerge block |
| L5 外部比較 | 200URL、同日同環境、本家vsRust | GA block（開発中は継続計測） |
| L6 耐久/安全 | 1h fuzz×4、24h soak、corrupt state/secret tests | GA block |
| L7 リリース | clean checkout build、docs、パッケージ、supported OS install smoke | GA block |

試験はテスト対象外の機能も必ずCIで再評価。ignored/filtered/out-of-scopeは**件数と理由をすべて記録**し、通過件数から都合よく消さない。

## 9. 実行ステージ・依存関係・出口条件

**ステージは優先度・依存順**であり、時間見積もりや達成率の装飾ではない。各ステージの終了条件は「担当者が実装完了と言った」ではなく、レビュー可能なcommit・テスト・証跡が揃うこと。

| ステージ | 作業内容 | 依存 | 完了時の必須成果物 |
| --- | --- | --- | --- |
| **M0 固定仕様・回帰台帳** | Python v0.5公開API/全schema/全action/Spider hookの正確なInventory化。現行Rustコードとの差分判定 | なし・最優先 | `UPSTREAM_API_V05.json`、`MCP_ACTIONS_V05.json`、`PARITY_MATRIX.md`、不足件数とテストID |
| **M1 統合基盤と品質ゲート** | 作業ツリー/PR監査、独立成果の安全な統合、脆弱な未完成実装の識別、VPS CI/Chrome fixture再現環境 | M0と並行可 | クリーンな統合SHA、Stage 0〜7回帰、各担当差分の引き継ぎ、既知fail一覧 |
| **M2 Browser基盤安定化** | 本番Chrome起動問題、context/frame/page lifecycle、外部CDP、state/secret/cleanup、実機検証 | M1 | 安定したChrome155/156 E2E、漏えい/クラッシュ負例 |
| **M3 Browser全操作・Stealth** | ARIA/iframe、全action、element screenshot、storage、proxy/DoH/adblock、Cloudflare処理、難サイト比較 | M0+M2 | B-01〜B-16のtest ID全件verified、差分一覧・取得率 |
| **M4 MCP・CLI・Daemon完全化** | 16 executor完全互換、永続session、MCP/CLI両経路、対話shell、HTTP transportの安全な実装 | M0+M2、M3とは一部並行 | MCPツールfixture100%、daemon restart、CLI実E2E、schema契約 |
| **M5 Spider完全化** | GET-only拡張、Request/Response/Item、callback/errback/pipeline、LinkExtractor、resumeの意味・全テンプレート | M0+M1+F型 | 上流`tests/spiders`相当の差分テスト、クラッシュ復旧E2E |
| **M6 HTTP・Fetchの機能差解消** | Fetcher全引数、proxy/redirect/HTTP3、block分類、auto tier/caching同等性 | M0+M1、M2と並行可 | 本家HTTP引数マトリクス、ローカルserver＋外部比較 |
| **M7 DOM・Adaptive・抽出精度/性能** | XPath/regex残差、相互SQLite、抽出品質、計測ボトルネック、パーサ最適化 | M0+M1、M2〜M6と並行可 | 互換fixture全件一致、指定性能・main_contentの品質計測 |
| **M8 横断セキュリティ・負荷・長時間運用** | 200URL比較、150件負荷、fuzz 4種、24h endurance、悪意入力、CI・本番運用 | M2〜M7 | G-01〜G-12の証跡、未達一覧0、再現手順 |
| **M9 GA準備・公開** | README/API/CLI docs、設定/スキーマ版、CHANGELOG/NOTICE/SBOM、releaseパッケージ | M8 | 全G項目PASS、新規環境インストール確認、署名/チェックサム、利用者向けリリースノート |

### 9.1 各ステージの詳細出口

**M0：棚卸し**

`core/ai/server.py`のツール引数・description、`_browser_actions.py`の全variant、`spiders`のpublic hook、`cli.py`の公開オプションを固定SHAから抽出し、**本家にあるがRustに無いIDが数えられる状態**にする。現行の「16ツール登録済み」は成功ではない。元テストの直接移植か等価な自動テストを各IDへ紐付ける。API行が増えるだけの計画文書は完成扱いにしない。

**M1：統合**

統合HEAD・他作業者の担当・変更中のファイルを先に調査。既存の未コミットファイルを勝手に`reset`/`checkout`しない。別worktreeのdiff、commit、テストの証拠を確認して一件ずつ取り込む。特に以下を**証拠とともに**判断する。

- `feat/extract-schema-graph`の`bcc36fa`/`b899edb`は抽出単体9/9 PASS、strict Clippy PASS（VPS隔離環境）。全workspaceは1.5GiB制限でOOM、**未検証**。
- 統合時点`4d1ed34`はVPSでworkspace113件PASS、0FAIL、実機Chromeの一部を別実行/除外、strict Clippy PASS。これは**将来のHEAD全体の保証ではない**。
- `docs/IMPLEMENTATION_STATUS.md`等の未コミット差分は他作業に属する可能性があるため保護。過去の`rustscrapling-*` worktreeの差分がすでにcherry-pick済みかも必ず確認。
- ダミー値を返すsession/unsupported API/未テスト機能は、契約外の成功として扱わず優先度を付けて直す。

**M2：本番Browserの安定化**

テスト用`--password-store=basic`でChrome155/156のlocalhost CDPが通る事実と、本番用Launcherの問題を切り分ける。XDG/D-Bus/鍵管理/サンドボックス、プロキシ、Chrome終了/プロファイル削除の競合を実証。ログイン状態の安全な保管を優先し、セキュリティを弱める一時回避を恒久化しない。

**M3：本家ブラウザ機能の充足**

ARIA・frame・contenteditableなどの操作をCDPから実体として実装し、`browser_actions`の全schemaをMCPの実機で照合。外部proxy/auth/DoH/adblock、stateをパターン別に検証する。Cloudflare対応は**本家の設計と公開機能**を固定条件で比較し、成功率が劣るときだけ`docs/DECISIONS.md`の代替案を検討する。

**M4：利用経路の完全化**

MCP16ツールの説明だけでなく、ツールの各パラメータを実executorに接続。CLI`session`もdaemon上の実page/contextを扱う。daemon依存時の呼出し・終了時/未稼働時のerror・MCP専有sessionとの隔離をテスト。Shell/REPL相当の探索体験を明文化して実装。APIとCLIのヘルプ/仕様を同時更新する。

**M5：Spiderの柔軟性**

`SpiderParser`の追加Requestにmethod/header/body/cookie/priority/callback/metadataを持たせる。GET-onlyの古いAPIがある場合は移行互換を保つ。Checkpointでinflightがlostしない設計を先にテストで固定してから修正。`exactly-once`が必要なら出力とfrontierのtransaction境界まで設計し、単なるvisited-setでは保証しない。

**M6：HTTPとFetch**

Python`Fetcher`の引数を一件ずつ照合し、TLS fingerprint/HTTP2/3/proxy/redirect/cookie/retryに欠落がないか検証する。自動tierの学習とブラウザへfallbackする意味を調整。成功時は実protocol・tier、失敗時は型付きerror。`http3`指定を受け付けるだけ、cache 304を成功とみなすだけ、という見かけの互換を拒否する。

**M7：正確性と本文品質**

XPath1.0とPython`re`互換の境界、DOMの差、Adaptiveをoracleでゼロ差にする。JSON-LD/URL safe/Markdown・BM25のfixture拡充。性能目標に未達の`find_similar`/`find_by_text`/既定HTML parserを計測して改善。本家より短くても記事本文を削りすぎた出力は不合格。

**M8：完成試験**

全機能差分を閉じた時点のSHAを固定し、同じ日に上流と200URL比較・150URL負荷を行う。fuzz/24h実運用を完走する。観測されたリソース増加や失敗は根本原因修正→ゲート再実行。テスト条件を変えて合格に見せない。

**M9：出荷**

リリース前に依存ライセンス・NOTICE・秘密/個人情報を確認。single binaryの配布条件（外部Chromeや静的ライブラリの必要性）を**実際のインストール手順**に記載。crates.ioに出す場合は名称判断を取り直す。`CHANGELOG`と既知制限を公開し、GA以前のbuildは`preview`等と呼ぶ。

### 9.2 並行開発の禁止事項・許容事項

- 各agentは**独立worktree・独立branch**を使い、担当クレート/機能範囲だけ変更。統合ブランチは統合担当のみが更新。
- 子担当の仕事は「特定受け入れIDに対してコード＋テスト＋記録を出す」こと。`Plan.md`・`Cargo.lock`・横断schemaを変更する場合は先に統合担当へ知らせる。
- 読取・調査・レビューは並行してよい。別担当の作業ツリーの変更/cleanup、root`.git`の再初期化、既存サービス停止、全体プロセスkillは禁止。
- 本家・先行実装のソースは適法なライセンス表示と検証付きで利用。コードを参考にして加速するが、不明な流用コードを無記名で貼らない。
- 最優先は**既存動作の維持**。新しい機能で回帰した場合は修正してから統合する。不要機能・重複した経路の追加は控える。
- 依存変更は`Cargo.lock`を含めて再現可能にし、必要な場合にのみ行う。
- チーム間でAPI仕様が衝突する場合、共通型とテストを先に確定して変更する。根本解決のない`unsafe`や同義関数の量産で回避しない。

## 10. 最初に着手する作業バックログ

これは**実装開始時に使う順番付きタスクリスト**。常に現行HEADと担当者の状況を確認し、未着手・既対応の判定を更新してから実施する。各タスクの成果は少なくとも`commit SHA / modified files / tests / blocked reason / next dependency`を持つ。

| 順位 | タスク | 主担当の境界 | 終了の確認 |
| --- | --- | --- | --- |
| 01 | M0公開APIと16 MCP tools/全browser actionsの自動棚卸し | `oracle/`＋`docs/compat/` | source/fixture/test IDによる対応率が測れる |
| 02 | 他worktree・抽出安全修正の統合監査 | 統合担当、保護されたdocs | 重複cherry-pick無し、全変更の出典とtest記録 |
| 03 | 安全なVPS/CI回帰手順と1.5GiB OOM対策 | `scripts/`・CI | 全workspace検証がサービスに干渉せず再現 |
| 04 | 本番Chrome launcherのlocalhost遷移問題解決 | `rustscrapling-browser` | basic password-storeの本番強制なしで実Chrome E2E |
| 05 | frame/ARIA/refの要素指定基盤 | `rustscrapling-browser` | CSS/ARIA/iframe selector実Chrome E2E |
| 06 | 本家全browser actionsとscreenshotの接続 | `browser`＋`mcp`を別タスク分離 | action matrix100%、MCP経由E2E |
| 07 | Daemon永続browser session＋CLI session | `daemon`＋`cli` | 別プロセスstart/list/save/load/closeが実動 |
| 08 | MCP16 executorの残り全引数とschema/description照合 | `rustscrapling-mcp` | 16 tool matrix完了、unsupportedが公開対象に残らない |
| 09 | Spider rich Request/parse callbacks | `rustscrapling-spider` | POST/body/header/cookie/priority/callbackのfixture |
| 10 | Spider LinkExtractor/pipeline/checkpoint | `rustscrapling-spider` | 上流hook差分0、クラッシュ復旧E2E |
| 11 | Fetcher引数・proxy・HTTP/3/autoの差分解消 | `http`＋`fetch` | server fixture＋本家相当request比較 |
| 12 | Selector/XPath/regex/Adaptive最適化 | `select`＋`adaptive` | oracleゼロ差＋性能threshold |
| 13 | Extract本文品質・metadata差分 | `extract` | 本家語句保持95%、char bound、悪意HTML |
| 14 | 200URL比較・150件負荷・fuzz・soak | `bench`・`tests`・CI | G-04〜G-11の本物の証跡 |
| 15 | ドキュメント/リリース | `docs`・`README`・Packaging | すべてのGAゲートがPASS |

順序の柔軟性：01〜03は直ちに並行可能。04の後に05→06→07/08。09〜13はAPI/CI基盤が整った後なら複数担当で並行可能。ただし**MCPとDaemonのブラウザ型、SpiderとFetchのRequest型**の変更は、先に契約を共有して衝突を避ける。

## 11. 実装・レビュー・統合の標準手順

### 11.1 タスクを切る時

1. `git status` / `git worktree list` / HEADを確認。すでに実装済みのコミットや作業中のdiffを確認し、二重実装を防ぐ。
2. 対象`Acceptance ID`・固定上流の該当箇所・現行Rustの差分・担当crates/変更禁止範囲を短いタスク文にする。
3. その対象の**失敗する再現テストまたは上流fixture**から始める。結果だけ書き換えない。
4. 独立worktreeを使用し、1PRを一つのレビュー可能な機能単位に限定する。
5. 秘密情報を扱う場合はmockまたは匿名化fixtureで試験。生cookie/tokenをログやコミットに残さない。

### 11.2 実装が終わった時

- `cargo fmt --all --check`・対象crates test・strict`clippy -D warnings`（実行が許される安全な環境のみ）。
- HTTP/MCP/Browser/Spiderの接続変更は、対象のmock/localhost＋実プロセス/Chrome E2Eも必須。
- 統合後の`cargo test --workspace --locked --lib --tests`と必要な`ignored`実機テストは**区別して**記録。
- `git diff --check`、`git status`、秘密情報チェック、第三者コードライセンス確認。
- 結果を`docs/RESULTS.md`、進捗を`docs/IMPLEMENTATION_STATUS.md`、契約と差分を`PARITY_MATRIX.md`/`docs/COMPAT.md`に記載。
- 既存の未コミット・未所有ファイルを変更したときは作業を中断し、勝手に上書きせずに統合担当へ報告。

### 11.3 統合の前提

1. ベースSHAと取り込み先HEADを比較。コミットが既に含まれていないか確認。
2. 開発者の作業ツリーは読むだけ。必要ならreview worktreeを作り、diffとテストで検証。
3. 依存を考慮した`cherry-pick`/PR merge。競合解消では**テストと本家の意味**を優先。
4. 統合SHAで差分・lint・test・E2E再実施。不完全なら`partial`に戻して追加修正。
5. 取り込んだcommit SHA、検証SHA、既知制限を記録。branch削除は所有者/担当者の確認と未統合変更0件の検証後に限る。

### 11.4 報告フォーマット（各タスク）

`RESULT`（達成/部分/失敗）、`FILES`、`COMMITS`、`TESTS`（実行したコマンドとexit code）、`EVIDENCE`（環境・ログパス）、`GAPS`（未達ID）、`RISKS`、`NEXT`（依存）を記載。測っていない成功率やスループット、異なる環境から算出した倍率は記載しない。

## 12. リリース判定用チェックリスト

次のチェックボックスは**現在の未達を隠さないよう初期値すべて未完了**。M0で作る対応表とともに`verified`証跡があるものだけ更新する。

### 上流互換

- [ ] upstream固定SHA・公開API/全引数/既定値/エラーを自動で棚卸し
- [ ] Parser/CSS/XPath/Regex/selector generationの差分テスト全件PASS
- [ ] Adaptive/SQLite/Domain/score roundtrip、互換スコア完全一致
- [ ] HTTP Fetcher/AsyncFetcher・proxy/TLS/HTTP3の公開機能に欠落なし
- [ ] Dynamic/Stealthy Browserの全機能と実ChromeでのE2E
- [ ] Spider6テンプレート、rich Request、parse/callback/pipeline、再開保証
- [ ] MCP16ツール＋本家browser actions全variantの実動作・schema一致
- [ ] CLI全コマンド、永続session、daemon、対話shell相当
- [ ] Inventory上の対象行の`verified`率100%（未マップ0、偽成功0）

### 取得品質・性能

- [ ] 約200URLをPython本家・Rust同条件同日に比較し、全体・カテゴリ別成功率が本家以上
- [ ] 150URL、並列8で全体完了時間が本家以下
- [ ] 1万要素で`relocate ≤50ms`、`find_by_text ≤5ms`、`find_similar ≤3ms`
- [ ] CSS`.price::text ≤5ms`、HTML既定parser`≤15ms`（旧計画から継承）
- [ ] 本文語彙保持≥95%、出力文字数≤本家
- [ ] Rust本体の最大RSS≤256MiB（Chromeは分離）、大きなリクエストでもbounded

### 安全・継続運用

- [ ] secrets/SSRF/authenticated IPC/cookie/profile/URL redactionの負例テスト
- [ ] Chrome155/156実機・MCP経由・本番launcherで不安定なNavigationが残っていない
- [ ] 1時間fuzz×4、致命的な異常0件
- [ ] 24h耐久、資源リーク・state破損・再起動後の喪失なし
- [ ] 全workspace fmt/strict Clippy/tests、CLI/MCP/daemon E2E、package smoke成功
- [ ] 第三者ライセンス・NOTICE・SBOM・依存セキュリティ監査完了

### リリース

- [ ] サポートOS/Chromeバージョン・インストール/更新/削除を新環境で検証
- [ ] バージョン付き入出力JSON schema・API docs・README・CHANGELOGと実装一致
- [ ] 第3章G-01〜G-13すべて`PASS`、ログとSHAを追跡可能
- [ ] 所有者レビューを経てGAの公開可否を判断

## 13. 既知の事実・未達（2026-11-03現在）

本節は**作業開始時のスナップショット**であり、永久に正しい状態ではない。次の担当者はHEADと実際の証跡を確認して更新する。

| 分野 | 既にあるもの | 未達・検証不足 |
| --- | --- | --- |
| Stage 0 | 11クレート、Rust/libxml2/Chrome環境、先行Rust移植の一部評価 | 先行実装2件の全テスト、採用根拠の完全な記録 |
| DOM/Selector | CSS59・XPath29の固定oracle88件互換モード一致 | XPath網羅・既定HTML parser15ms目標 |
| Adaptive | SequenceMatcher3×10,000一致、丸め8,003一致、SQLite相互運用、relocate33〜36ms観測 | find_similar 3ms目標未達、find_by_text一部5ms超、追加oracle |
| HTTP/Fetch | wreq・HTTP/3・proxy/retry/typed error・auto fallback/cache | 全Fetcher引数、実サイト/複雑proxy・成功率比較 |
| Browser | CDP/Shadow DOM、基本入力・network本文、Chrome155/156の限定E2E | 本番launcher、ARIA/iframe・全action、DoH/proxy/auth、難サイト比較 |
| Daemon | 認証IPC、start/stop/status、cache学習SQLite復元 | 永続browser sessionのCLI運用、複数client本番E2E・24h耐久 |
| Spider | 6テンプレート、TOML、robots、rate、GET-only custom parse、JSON item、checkpoint | 任意method/body、per-request callbacks、pipeline/LinkExtractor完全版、crash semantics |
| MCP | 本家16ツール名、stdio JSON-RPC、基本executorsとnetwork/入力 | schema・全action・永続state・実Chromeエンドツーエンド |
| Extract | Markdown/main-content/metadata/BM25、安全URLの基本実装 | 本家95%品質測定、JSON-LD/URL修正の統合、数値比較 |
| QA | `4d1ed34`でworkspace113件PASS、strict Clippy/fmt PASS、Chrome実機テスト一部別実行 | 200URL比較、1h fuzz、24h耐久、全体RSS/速度目標 |

**直近の独立作業**：`feat/extract-schema-graph`の`bcc36fa`（JSON-LD著者参照）・`b899edb`（認証情報入りprotocol-relative URL拒否）は単体9件とstrict Clippy PASS。統合HEADには**未取り込み**。VPS 1.5GiB制限の全workspaceビルドは2ジョブ・1ジョブとも依存コンパイル中にOOM終了。これはテスト失敗でも全体成功でもない。

**旧Planから残す目標の根拠**：`docs/BASELINE.md`、`docs/RESULTS.md`、`oracle/bench_appendix_a.py`、`fixtures/stage1.json`/`stage2_*.json`、`docs/QA_AUDIT.md`、`docs/UPSTREAM_PORTS.md`、`docs/CRAWL_TEMPLATES.md`、`docs/DECISIONS.md`。数値は計測環境・SHAと一緒に読むこと。

## 14. 意思決定・仕様変更・完了宣言のルール

- ユーザーの既決事項（製品名、robots既定OFF、150件/並列8、Rust主実装、Python依存なし、完全なScrapling用途）を担当者の都合で変更しない。
- 実験的な設計変更は、`docs/DECISIONS.md`に`問題 / 選択肢 / 本家との互換影響 / 安全性 / 性能証拠 / 戻し方 / 推奨 / 承認結果`を記録する。確認が必要な変更は未承認のまま既定化しない。
- Python版の仕様とRustの安全要件が衝突した場合、「互換」より安全を優先する必要があることはあり得る。ただしその差は`docs/COMPAT.md`に明示し、公開対象から削除せず対策と承認要否を判断する。
- **速い≠正しい、テストが通る≠上流互換、Chrome localhost成功≠bot対策成功、登録済み≠実装済み、PRがある≠統合済み**という区別を常に維持する。
- プロジェクトを**「完成」**と呼べるのは、`docs/compat/PARITY_MATRIX.md`の対象行がすべてverified、G-01〜G-13がすべてPASS、未修正のP0/P1欠陥が0、リリース成果物の新規環境smokeが成功したときだけ。
- この計画が完成したことと、RustScrapling本体が完成したことは別。**本書の作成をもって実装フェーズを完了扱いしない。**

## 付録A. 既存計画の詳細との対応

旧`Plan.md`（revision 2、Git履歴`3b1760a`時点に含まれる）にあった設計・前提を、Revision 3の次の節へ引き継いだ。

| 旧Plan | 現Plan |
| --- | --- |
| 1章 目的・成功基準 | 1章、3章、8章 |
| 2章 Python/既存Rust調査 | 2章、5章、13章、`docs/UPSTREAM_PORTS.md` |
| 3章 対象外 | 1.2節 |
| 4〜5章 環境・判断 | 1.3章、5章、8.1節、`docs/DECISIONS.md` |
| 6章 CLI/daemon/MCP・AI強化 | 4章、6.6/6.8/6.9節 |
| 7章 CLI/MCP/Spiderの公開機能 | 6.7/6.9節、M0 inventory |
| 8章 クレート構成 | 4章 |
| 9章 oracle/SequenceMatcher/bench | 2章、3章、8章 |
| 10章 実装段階 | 9章、10章、11章 |
| 11〜14章 互換/進め方/危険/決定 | 5章、7章、11章、14章 |
| 付録A 合成ページの計測 | `oracle/bench_appendix_a.py`を実行用正本とする |

**最後の原則**：完成を急ぐために未実装を「完成」に変更してはならない。欠けている機能を埋め、ソース/テスト/実機/比較結果で証明する。すべての担当者とAIエージェントはこのゲートを同じ意味で使う。

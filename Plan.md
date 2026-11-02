# Plan.md: ScraplingのRust再実装 RustScrapling

この文書は、ScraplingをRustで再実装する作業の計画書である。読み手は実装を担当するAIエージェント(Muse)とする。作業の前に全体を読み、「判断済みの事項」に書いた内容は変えずに進めること。判断に迷った点や、計画と事実が食い違った点は `docs/DECISIONS.md` に記録し、所有者(m24104inoue)に確認する。

ツールの名前はRustScraplingとする。コマンド名は `rustscrapling`、クレート名の接頭辞は `rustscrapling-` とする。

## 1. 目的

AIエージェントが継続的な調査に使う、Webページの取得と抽出のツールを作る。Scraplingの機能を全て備え、Rustと設計の工夫によって、取得の成功率、速さ、出力の質でScraplingを上回ることを目指す。人間が対話的に使うことは前提にしない。

### 1.1 想定する使われ方

1回の調査で約150ページを取得する。調査を繰り返す場合は、1回ごとに約50ページが加わる。複数のエージェントが並列に呼び出す。対象のサイトは特定しない。

この規模では、HTMLの解析にかかるCPU時間は全体の待ち時間のごく一部になる。150ページを1ページ20ミリ秒で解析しても合計3秒である。そのため、品質の優先順位を次のとおりとする。

1. 取得の成功率。ブロックされるサイト、取得できないサイトを減らす。
2. 待ち時間。並列化、ブラウザの再利用、キャッシュで短くする。
3. 出力の質。エージェントに渡す本文が正確で、余計な文字が少ない。
4. CPU効率とメモリ。

adaptive(サイトの構造が変わった後に、保存しておいた要素を探し直す機能)は、所有者の指定により実装の最優先とする。調査を繰り返す中で同じ情報源を再訪するときに、エージェントが一度見つけた選択条件を使い回すための機能として位置づける。

### 1.2 成功の基準

基準値は、本家Scrapling(後述の固定コミット)を同じ環境、同じ日に動かした結果と比べる。

| 項目 | 基準 |
|---|---|
| 取得成功率 | 検証用URL集(9.3節)で、本家Scraplingの成功率以上。目標は本家より5ポイント以上高いこと |
| 調査1回分の所要時間 | 同じURL150件を同じ並列数で取得し、全体の所要時間が本家以下 |
| adaptiveの一致 | 互換モードで、類似度スコアと再特定の結果が本家と100%一致 |
| adaptiveの速さ | 約1万要素のページで再特定が50ミリ秒以下(本家の実測は5853ミリ秒) |
| 本文抽出 | 本家の `main_content_only` の出力に含まれる語の95%以上を含み、出力の文字数は本家以下 |
| 安定性 | 解析器とセレクタのファジング(ランダムな入力を大量に与える試験)を各1時間行い、異常終了なし |
| メモリ | 150ページを並列数8で取得する間、Chromeのプロセスを除いたRustScrapling本体の最大使用量が256MiB以下 |

## 2. 調査で分かったこと

### 2.1 本家Scraplingの構成

対象は `https://github.com/D4Vinci/Scrapling` のコミット `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`(2026-11-01、v0.5)に固定する。Pythonで約1.6万行、テストが約2.5万行ある。ライセンスはBSD 3-Clauseである。

| 層 | 主なファイル | 依存 |
|---|---|---|
| パーサ | `scrapling/parser.py`、`core/translator.py`、`core/custom_types.py` | lxml(libxml2)、cssselect |
| adaptive | `parser.py` の `relocate` と `find_similar`、`core/storage.py`、`core/utils/_utils.py` | difflib、SQLite、tld |
| HTTP取得 | `engines/static.py`、`engines/toolbelt/` | curl_cffi |
| ブラウザ取得 | `engines/_browsers/` | patchright、browserforge |
| クローラ | `spiders/` | anyio、protego |
| AI向け機能 | `core/ai/server.py`、`core/ai/_browser_actions.py` | mcp、markdownify |
| CLIとシェル | `cli.py`、`core/shell.py` | click、IPython |

### 2.2 実測値

所有者のWindows PC(Python 3.12)で、約2000商品、約1万要素、416KBの合成ページを使って計測した。計測スクリプトは付録Aに載せる。Museの環境で測り直し、基準値として `docs/BASELINE.md` に記録すること。

| 処理 | 本家の実測 |
|---|---|
| HTML解析 `Selector(page)` | 17.8 ミリ秒 |
| `css('.price::text').getall()` | 20.6 ミリ秒 |
| `find_similar()`(2000件の兄弟要素) | 46.3 ミリ秒 |
| `find_by_text()` | 70.8 ミリ秒 |
| 構造変更後の `relocate()` | 5852.7 ミリ秒 |

`relocate` が遅い理由は、ページの全要素について、純Pythonの `difflib.SequenceMatcher` を約10回ずつ呼ぶためである。Rustでの移植と、複数コアでの並列計算で、最も大きな改善を見込める。HTML解析はすでにC言語のlibxml2で動いているため、大きな改善は見込めない。

### 2.3 既存のRust移植

ScraplingのRust移植は、2026-11-02の時点で少なくとも2件ある。どちらも本家v0.5の全機能には届いていない。

| 名前 | 内容 | 足りない点 | ライセンス |
|---|---|---|---|
| [Liohtml/RUSTScrapling](https://github.com/Liohtml/RUSTScrapling)(crates.ioでは `rust_scrapling`、最終更新2026-10-14) | scraper(html5ever)による解析、adaptive、クローラ、Markdown変換。`SequenceMatcher` をautojunkまで移植し、CPythonの結果と照合する試験がある | HTTP取得はreqwestで、TLSの偽装がない。ブラウザ取得がない | MITとApache 2.0 |
| [chrisabruce/scrapling-rs](https://github.com/chrisabruce/scrapling-rs)(最終更新2026-04-15) | wreqによるTLSの偽装、playwright-rsによるブラウザ取得、Cloudflareの突破、MCP、PyO3 | 本家v0.5で加わったクローラのテンプレートなどがない | MIT |

ツールの名前RustScraplingは、前者のRUSTScraplingと大文字と小文字の違いしかない。所有者はこれを承知のうえで名前を決めた(14章)。crates.ioには公開しない前提とし、公開する場合は名前を見直す。

### 2.4 既存の移植から採るものと補うもの

2026-11-02に両リポジトリのコードを読んで確かめた結果である。どちらの試験もまだ実行していないため、採る候補は段階0で評価してから使う。

| 領域 | 採る候補 | 既存の移植の弱点 | RustScraplingでの補い方 |
|---|---|---|---|
| adaptive | RUSTScraplingの `SequenceMatcher` の移植と、CPythonの結果と照合する試験(`src/parser/adaptive.rs`) | RUSTScraplingの `text()` は、直下のテキストを全てつなげる。lxmlの `text` は最初の子要素より前のテキストだけなので、文中に子要素を含む要素でスコアがずれる。両方ともRustの `round()`(0.5を0から遠い側に丸める)を使っており、Pythonの `round()` と境界で結果が違う。scrapling-rsは類似度にstrsimを使っており、スコアが本家と一致しない。scrapling-rsはadaptiveが `css()` と `xpath()` につながっていない | DOMにlxmlの `text` と `tail` の見え方を持たせる。Pythonと同じ丸めを実装する。9.1節の正解データで照合する。rayonで並列に採点する |
| 解析と選択 | 両者の `::text` と `::attr()` の実装、選択条件の生成 | どちらもXPathで要素を選べない(XPathの文字列を作る機能だけがある) | XPath 1.0を実装する。libxml2による互換モードを持つ |
| HTTP取得 | scrapling-rsのwreqとwreq-utilの組み込み方 | RUSTScraplingはreqwestを使っており、TLSの偽装がない。どちらもHTTP/3がない | wreqを使う。HTTP/3は段階0で調べる |
| ブラウザ取得 | scrapling-rsの起動フラグの一覧、Cloudflareの突破の処理の流れ、広告ドメインの一覧 | scrapling-rsはplaywright-rsを使っている。patchrightではないので、Runtime.enableによる検知を避けられない可能性が高い(段階0で確かめる)。通信の記録は作りかけである。RUSTScraplingにはブラウザ取得がない | chromiumoxideの上に検知回避を実装する。stealthscraper-rsのMITMプロキシの方式を検討する |
| クローラ | RUSTScraplingのクローラ(中断と再開、応答キャッシュ、リンク抽出) | 本家v0.5のテンプレートが揃っていない。メモリの扱いが決まっていない | テンプレートを全て移植する。6.3節のメモリの扱いを組み込む |
| 試験の道具 | scrapling-rsのファジングの対象(HTML解析、CSSセレクタ) | XPathとadaptiveが対象に入っていない | XPath、adaptive、Markdown変換を対象に加える |
| AI向け機能 | scrapling-rsのMCPサーバ | どちらにも、取得手段の自動切り替え、サイトごとの学習、デーモン、メタデータの抽出、関連度の順位付けがない | 6.2節の強化として作る |

### 2.5 周辺のツール

参考になる周辺のツールとして、stealthscraper-rs([GitHub](https://github.com/ypno/stealthscraper-rs))がある。wreqを使ったローカルのMITMプロキシ(通信を中継して書き換える仕組み)を通して、ブラウザのTLSの癖を指定のブラウザに揃え、ブラウザを起動し直さずに上流のプロキシを切り替える。

## 3. 対象外とするもの

1. IPythonを使った対話シェル(`scrapling shell`)。人間向けの機能のため作らない。
2. Scrapyとの連携(`integrations/scrapy.py`)とPython向けAPI。Pythonからは脱却する方針のため作らない。
3. Pythonのクラスとして書くSpider。代わりに宣言的な設定ファイルとテンプレートで同じ用途を満たす(7.3節)。
4. 本家にない新しい取得手段(有料のCAPTCHA解決サービスとの連携、ブラウザを使わずにV8などでJavaScriptだけを動かす取得手段など)。必要になったら別途計画する。
5. adaptiveのスコアの重みを結果から学習する機能。互換モードと両立せず、効果も確かめられていないため作らない。

Pythonは、本家を動かして正解データを作る試験用の道具(`oracle/`)にだけ使ってよい。製品のバイナリはPythonに依存しない。

## 4. 実行環境の前提

Museの実行環境(htch-runtime)を2026-11-02に実測した結果は次のとおりである。

| 項目 | 状態 | 計画への影響 |
|---|---|---|
| OS | Ubuntu 24.04.5、x86_64 | Linuxを主な対象にする |
| CPU | 4コア | ビルドと並列試験の速さに上限がある |
| メモリ | 7.7GiB、スワップなし、計測時に使える量は約490MiBから約530MiB | ビルドの並列数を絞る。メモリ不足による強制終了に注意する |
| ディスク | `/home/hatch` に125GiBの空き。`/tmp` は512MiB | ビルド成果物と一時ファイルは `/home/hatch` に置く |
| Rust | 未導入 | 段階0で導入する |
| Chrome | 未導入。Xvfbはある | 段階0で導入する。headedの実行は `xvfb-run` を使う |
| Python | 3.12.3、pipあり | 正解データの生成に使う |
| ネットワーク | `HTTP_PROXY` などで指定されたプロキシ経由。UDPは通らない前提 | 全ての通信をプロキシ対応にする。HTTP/3はこの環境では試験できない |
| Docker | 使えない | コンテナを前提にした手順は書かない |

ビルドは次の設定で行う。

```bash
export CARGO_TARGET_DIR=/home/hatch/rustscrapling-target
export TMPDIR=/home/hatch/tmp
export CARGO_BUILD_JOBS=2
```

`Cargo.toml` では、開発用の設定を `[profile.dev] debug = "line-tables-only"` とし、メモリの使用量を減らす。リンク時の最適化(LTO)はリリース用のビルドだけで有効にする。

取得成功率は接続元のIPアドレスの評判に左右される。この環境の接続元はデータセンターのプロキシである可能性が高い。そのため、成功率は必ず本家と同じ環境、同じ日に比べ、絶対値だけで判断しない。

## 5. 判断済みの事項

| 項目 | 決定 | 理由 |
|---|---|---|
| 提供形態 | 単一のバイナリ。1回ずつ実行するCLI、常駐するデーモン、MCPサーバ(AIエージェントにツールを提供する標準の仕組み)の3つの使い方を持つ | 6章を参照 |
| 非同期処理 | tokio | 主要なHTTP、CDP、MCPのクレートがtokioを前提にしている |
| CPU並列 | rayon | `relocate` などの全要素の走査を複数コアに分ける |
| DOM | 独自のアリーナ型DOM(要素を配列にまとめて番号で参照する木構造) | 解析器を2種類差し替えられるようにし、走査を速くするため |
| HTML解析器 | 既定はhtml5ever(HTML5仕様どおりの解析器)。互換モードとしてlibxml2の解析器も持つ | 既定ではブラウザと同じDOMを得る。互換モードでは本家と同じDOMを得て、正解データとの差を解析器の違いと論理の誤りに切り分ける |
| CSSセレクタ | servoの `selectors` クレートで照合し、`::text` と `::attr(名前)` は独自に実装する | 本家はcssselectでCSSをXPathに変換しているが、変換を挟まない方が速い |
| XPath | XPath 1.0を独自に実装する。段階0で既存クレート(xmloxide、sxd-xpath)を試し、本家の試験を全て通せるものがあれば採用してよい | Rustには、HTMLのDOMに対して完全なXPath 1.0を使える定番のクレートがない |
| 類似度の計算 | Pythonの `difflib.SequenceMatcher` を、autojunk(長い入力で頻出する要素を無視する処理)を含めて完全に移植する | 既存のクレートではスコアが一致しない |
| adaptiveの保存 | rusqlite(SQLiteをRustから使うクレート)。本家と同じ表 `storage(url, identifier, element_data)` と同じJSON形式を使う | 本家が作ったデータベースをそのまま読めるようにする |
| HTTP取得 | wreqとwreq-util(ブラウザのTLSとHTTP/2の通信の癖を再現するクレート) | curl_cffiに相当する。HTTP/3の対応は段階0で確認する |
| ブラウザ取得 | chromiumoxide(CDPでChromeを直接操作するクレート)の上に、patchright相当の検知回避を独自に実装する | Rust版のPlaywrightに定番がない。成功率が本家に届かない場合の代替は5.1節 |
| Markdown変換 | htmdを使い、本家の本文抽出と無害化の処理を移植する | turndown.jsの試験を通しているクレートである |
| MCP | rmcp(公式のRust SDK) | 公式の実装である |
| ログ | tracing | 構造化したログをJSONで出せる |
| 既存の移植の扱い | 新しく作る。ただし2.3節の2件のコードと試験は、段階0の評価で品質を確かめたものに限り流用してよい。流用した部分にはライセンスの表示を `NOTICE` に残す | 解決済みの問題を作り直さないため。品質は確かめていないので、評価を経ずに使わない |

### 5.1 ブラウザ取得の代替案

chromiumoxideで独自に実装した検知回避の成功率が、検証用URL集で本家に届かない場合は、patchrightのNode.js製ドライバを子プロセスとして起動し、Playwrightのドライバ通信で操作する方式に切り替える。切り替えるかどうかは段階4の終わりに成功率で判断し、`docs/DECISIONS.md` に記録する。どちらの方式でも、外から見える CLI と MCP の動作は変えない。

### 5.2 二つの動作モード

adaptiveと選択の処理は、次の2つのモードを持つ。

| モード | 内容 |
|---|---|
| 互換モード `--compat` | libxml2の解析器を使い、本家とスコアと結果を一致させる |
| 既定モード | html5everの解析器を使う。adaptiveは、上位の候補をスコアつきで複数返すなど、本家にない機能を加える |

## 6. 提供形態の設計

AIエージェントが使う前提で、次の3つの使い方を1つのバイナリで提供する。中身は同じライブラリのクレートを共有する。

| 種類 | 使い方 | 向いている場面 |
|---|---|---|
| CLI | `rustscrapling get URL --format md` のように1回ずつ実行する | シェルを使えるどのエージェントからでも呼べる。結果をファイルに書いて、エージェントの文脈に載せる量を絞れる |
| デーモン | `rustscrapling daemon start` で常駐させる。CLIは、デーモンが動いていれば自動でそちらに処理を頼む | ブラウザの起動(1回あたり約1秒)を省ける。並列のエージェントの間でキャッシュ、ログイン状態、サイトごとの速度制限を共有できる |
| MCPサーバ | `rustscrapling mcp` で起動する。標準入出力とHTTPに対応する | MCPに対応したエージェントが、型の決まったツールとして呼べる。本家のMCPと同じツール名にする |

CLIだけでも動くが、CLIだけでは呼び出しのたびにブラウザとセッションを作り直すため、待ち時間と成功率で不利になる。そのため、エージェントはCLIから呼び出し、状態はデーモンに持たせる。

### 6.1 AIエージェント向けの出力の決まり

1. 標準出力はJSONを既定とする。`--format json|ndjson|md|text|html` で切り替える。JSONの形式はバージョン番号つきで `docs/schema/` に定義する。
2. 本文が長いときは、`--out FILE` でファイルに書き、標準出力には保存先、文字数、見出しの一覧だけを返す。`--max-chars N` で出力の上限を決められるようにする。
3. 失敗は終了コードと、`error.kind`(`blocked`、`timeout`、`dns`、`tls`、`captcha`、`http_status`、`proxy`、`parse` など)で返す。`error.hint` に次に試すべきこと(例: `--tier browser` を付けて再実行)を書く。
4. 同じ入力には同じ出力を返す。並び順や浮動小数の表記を固定する。
5. 取得した本文に含まれる指示文でエージェントが操作されないよう、本家のMCPサーバと同じ無害化(隠し要素の除去など)を既定で行う。

### 6.2 本家にない強化

| 強化 | 内容 |
|---|---|
| 取得手段の自動切り替え | `--tier auto` で、HTTP、別のブラウザの癖を使ったHTTP、ステルスブラウザ、headedのブラウザの順に試す。ブロックを判定したら次の手段に進む |
| サイトごとの学習 | どの取得手段で成功したかをサイトごとにデーモンの状態データベースに保存し、次回は成功した手段から始める。記録には有効期限(既定は24時間)を付け、期限が切れたら最も軽い手段から試し直す |
| 条件付きの再取得 | ETagとLast-Modifiedを保存し、調査の繰り返しで同じページを取るときは、変更がなければ保存済みの本文を返す |
| 並列エージェント間の調整 | 同じURLへの同時の取得をまとめる。サイトごとの速度制限を全エージェントで共有する |
| adaptiveの強化 | 既定モードでは、上位k件の候補とスコア、どの特徴が一致したかを返す。保存する指紋の履歴を残す |
| メタデータの抽出 | JSON-LD、Open Graph、`<meta>` の題名、著者、公開日、正規URLを、JSON出力の `metadata` に入れる |
| 関連度の順位付け | `--query 文字列` を指定すると、本文の段落とクロール結果のページを、BM25(検索エンジンで使われる関連度の計算方法)で順位付けして返す |

### 6.3 メモリの扱い

実行環境の空きメモリが少ないため(4章)、次のとおりにする。

1. 取得した本文は、届いた順に解析と出力を済ませ、全ページをメモリに溜めない。
2. 1ページの本文が一定の大きさ(既定は8MiB)を超えたら、ディスクに書き出して扱う。
3. クローラの訪問済みURLの集合は、URLそのものではなく固定長の指紋で持つ。大きくなったらSQLiteに移す。
4. ブラウザの同時起動数とタブの数に上限を設け、上限に達したら待たせる。

## 7. 機能の対応表

### 7.1 CLIのコマンド

| コマンド | 本家の対応 | 内容 |
|---|---|---|
| `rustscrapling get/post/put/delete URL` | `scrapling extract get` など、`Fetcher` | HTTP取得 |
| `rustscrapling fetch URL` | `scrapling extract fetch`、`DynamicFetcher` | ブラウザ取得 |
| `rustscrapling stealth URL` | `scrapling extract stealthy-fetch`、`StealthyFetcher` | ステルスブラウザ取得。Cloudflareの突破を含む |
| `rustscrapling auto URL` | なし | 6.2節の自動切り替え |
| `rustscrapling select [URL\|FILE] --css/--xpath/--text/--regex` | `Selector.css` など | 選択と抽出。`--id`、`--adaptive`、`--auto-save`、`--percentage` を持つ |
| `rustscrapling similar` | `find_similar` | 似た要素の検索 |
| `rustscrapling relocate --id ID` | `relocate` | 保存した要素の再特定 |
| `rustscrapling selector-gen` | `SelectorsGeneration` | 要素からCSSとXPathの選択条件を作る |
| `rustscrapling md [URL\|FILE]` | `Response.markdown()` | Markdownへの変換 |
| `rustscrapling crawl SPEC.toml` | `Spider` とテンプレート | クロール。結果はNDJSONで逐次出力する |
| `rustscrapling session open/close/list/save-state/load-state` | MCPの `browser_open` など | ブラウザのセッション管理 |
| `rustscrapling network search` | `session.network.search` | 記録した通信の検索 |
| `rustscrapling daemon start/stop/status` | なし | デーモンの管理 |
| `rustscrapling mcp` | `scrapling mcp` | MCPサーバ |
| `rustscrapling install` | `scrapling install` | ブラウザの導入 |

### 7.2 MCPのツール

本家の `core/ai/server.py` にある全てのツールを、同じ名前と同じ引数で提供する。対象は `make_request`、`browser_fetch`、`browser_fetch_once`、`session_make_request`、`open_request_session`、`browser_open`、`browser_save_state`、`browser_load_state`、`close_session`、`list_sessions`、`browser_network_requests`、`browser_network_request`、`browser_extract`、`browser_actions`、`browser_evaluate`、`browser_screenshot` である。ツールの説明文も本家に合わせ、`tests/ai/test_tool_descriptions.py` に相当する試験を書く。

### 7.3 クロール

本家のSpiderはPythonのクラスで書く。RustScraplingでは、TOMLの設定ファイルで開始URL、たどるリンクの規則、抽出する項目(CSS、XPath、adaptiveの識別子)を書く。テンプレートは本家の `CrawlSpider`、`SitemapSpider`、`XMLFeedSpider`、`CSVFeedSpider`、`ShopifySpider`、`ContentSpider`(サイトをMarkdownの集まりにする)を全て移植する。

並列数、ドメインごとの速度制限、AutoThrottle、robots.txtの順守(既定は無効とし、`--obey-robots` で有効にする。本家と同じ既定値である)、ブロックの判定と再試行、中断と再開(`crawldir`)、開発用の応答キャッシュ、JSON、JSONL、CSV、XMLへの書き出しを移植する。ブロックの判定は本家の `spiders/spider.py` の `BLOCKED_CODES` と `is_blocked` を出発点にする。

## 8. 構成

Cargoのワークスペースを次のクレートに分ける。並列で作業するときは、エージェントごとに担当のクレートを分ける。

| クレート | 内容 | 依存先 |
|---|---|---|
| `rustscrapling-dom` | アリーナ型DOM、html5everとlibxml2からの木の構築、lxmlの `text` と `tail` に相当する見え方 | なし |
| `rustscrapling-select` | CSS、XPath、疑似要素、テキスト検索、正規表現、選択条件の生成 | dom |
| `rustscrapling-adaptive` | 要素の指紋、SequenceMatcherの移植、`relocate`、`find_similar`、保存 | dom、select |
| `rustscrapling-extract` | Markdown変換、本文抽出、無害化、メタデータの抽出、BM25による順位付け | dom、select |
| `rustscrapling-http` | wreqによる取得、通信の癖の再現、プロキシの切り替え、再試行 | なし |
| `rustscrapling-browser` | CDPによる操作、検知回避、Cloudflareの突破、通信の記録、ブラウザ操作 | なし |
| `rustscrapling-fetch` | 取得手段の共通の型、自動切り替え、応答の型 | http、browser |
| `rustscrapling-spider` | スケジューラ、速度制御、robots.txt、中断と再開、テンプレート | fetch、select、adaptive |
| `rustscrapling-daemon` | 常駐処理、共有の状態データベース、CLIとの通信 | fetch、spider |
| `rustscrapling-mcp` | MCPサーバ | fetch、select、extract |
| `rustscrapling-cli` | バイナリ本体 | 全て |
| `oracle/`(Rustではない) | 本家を動かして正解データを作るPythonのスクリプト | 本家Scrapling |

## 9. 検証の方法

### 9.1 正解データ

`oracle/` に本家の固定コミットを導入した仮想環境を作り、次の正解データをJSONで `fixtures/` に保存する。

1. 入力HTMLごとの、選択の結果(要素の位置、テキスト、属性)。
2. 要素ごとの `element_to_dict` の出力と、候補の要素ごとの類似度スコア。
3. `find_similar`、`find_by_text`、`find_by_regex` の結果。
4. 本家が書いたadaptiveのSQLiteファイルと、それを読んだ `relocate` の結果。
5. Markdown変換と本文抽出の出力。

入力HTMLは、本家の `tests/` にあるHTMLと、検証用URL集から保存した実ページを使う。本家の `tests/parser/` と `tests/spiders/` の試験は、意味のあるものを全てRustの試験に書き直す。

### 9.2 SequenceMatcherの一致試験

本家の結果と1件でも異なれば、adaptiveのスコアがずれる。次の3種類の入力について、Pythonの `difflib.SequenceMatcher(None, a, b).ratio()` の値を `oracle/` で1万件ずつ作り、Rust版と完全に一致することを確かめる。

1. 文字列。Pythonはコードポイント(Unicodeの1文字)単位で比べるため、Rust版も `char` 単位で比べる。
2. 文字列のタプル(要素の経路、兄弟要素のタグ)。
3. 長さ200以上の入力。autojunkが働く境界を必ず含める。

スコアの丸めは、Pythonの `round(x, 2)` と同じ結果になる実装にする。属性は、HTMLに書かれた順序を保つ。

### 9.3 検証用URL集

`bench/corpus.toml` に約200件のURLを置く。ニュース、通販、技術文書、官公庁、学術、JavaScriptで描画するサイト、Cloudflareなどのボット対策があるサイトを含める。各URLについて、成功の条件(HTTP 200、ブロック画面でない、本文が一定の文字数以上)を決める。本家とRustScraplingを同じ日に同じ環境で動かし、成功率と所要時間を `docs/RESULTS.md` に記録する。サイトは日々変わるため、別の日の結果どうしを比べない。

### 9.4 性能の測定

criterion(Rustの計測用クレート)で計測する。付録Aと同じ合成ページを使い、本家の値と並べて記録する。結果には、CPU、メモリ、ビルドの設定、試行回数を必ず書く。

## 10. 段階

各段階は、完了の条件を満たしてから次に進む。段階1と段階3と段階4の下調べは、担当を分ければ並行して進めてよい。

### 段階0 環境の準備と下調べ

1. rustupでRustの安定版を導入する。crates.ioのインデックス(`index.crates.io`)と配布元(`static.crates.io`)にプロキシ経由でつながることを確かめる。
2. Chrome for Testingを `/home/hatch` 以下に導入し、`xvfb-run` でheadedの起動を確かめる。
3. `oracle/` に本家の固定コミットを `scrapling[fetchers]` 付きで導入し、付録Aの計測を行って `docs/BASELINE.md` に記録する。
4. 次の3点を試し、結果を `docs/DECISIONS.md` に記録する。XPathの既存クレートが本家の試験を通せるか。wreqがHTTP/3に対応するか(対応しない場合はquinnとh3で補う案を書く)。chromiumoxideでRuntime.enableを送らずに要素を操作できるか。
5. libxml2をビルドに含められるか確かめる。システムに開発用の部品がなければ、ソースから静的にビルドする。
6. 2.3節の既存の移植2件を評価する。2.4節の表の「採る候補」を1つずつ確かめる。各リポジトリの試験を実行し、RUSTScraplingの `SequenceMatcher` と `relocate` が9.2節の一致試験を通すかを確かめる。scrapling-rsのTLSの偽装とブラウザ取得の作りも読む。流用するもの、参考にするだけのもの、使わないものを分け、理由とともに `docs/DECISIONS.md` に記録する。stealthscraper-rsのMITMプロキシの方式を、段階4で採るかどうかもここで判断する。

完了の条件は、上の結果が全て記録され、`cargo build` が空のワークスペースで通ることである。

### 段階1 DOMと選択

`rustscrapling-dom` と `rustscrapling-select` を作る。lxmlの `text` と `tail` の見え方(子要素の前の直接のテキストを `text`、要素の後ろのテキストを `tail` とする)を再現する。本家の解析の設定(空白だけのテキストの除去、コメントの除去、NUL文字の除去)を移植する。

完了の条件は、互換モードで正解データの選択結果と100%一致し、既定モードとの違いが `docs/COMPAT.md` に分類されていることである。付録Aのページで、解析は15ミリ秒以下、`css('.price::text')` は5ミリ秒以下とする。

### 段階2 adaptive(最優先)

`rustscrapling-adaptive` を作り、`rustscrapling select`、`rustscrapling similar`、`rustscrapling relocate` をCLIから使えるようにする。

1. `element_to_dict` を同じキーと同じ値で移植する。属性値の前後の空白の除去、空の属性の除外、子要素のタグからコメントを除くことを含む。
2. SequenceMatcherを移植し、9.2節の試験を通す。
3. `relocate` を移植する。互換モードでは全要素を採点して本家と同じ結果を返す。採点はrayonで並列に行う。
4. `find_similar`、`find_by_text`、`find_by_regex` を移植する。
5. SQLiteの保存を移植する。サイトの区別には、本家と同じく登録可能なドメイン(`tld` の `fld` に相当する値)を小文字で使う。Public Suffix Listのクレートで計算し、本家と同じ値になることを試験する。

完了の条件は、正解データのスコアと結果が互換モードで100%一致すること、本家が書いたデータベースを読んで同じ要素を再特定できること、付録Aのページで `relocate` が50ミリ秒以下、`find_by_text` が5ミリ秒以下、`find_similar` が3ミリ秒以下であることである。

### 段階3 HTTP取得とCLIの基本

`rustscrapling-http`、`rustscrapling-extract`、CLIの `get`、`post`、`put`、`delete`、`md` を作る。本家の `Fetcher` の引数(`impersonate`、`stealthy_headers`、`proxy`、`proxies`、`retries`、`timeout`、`follow_redirects`、`http3` など)を全て対応させる。プロキシの切り替えは本家の `ProxyRotator` と同じ方式を持つ。環境変数のプロキシ設定に従う。

完了の条件は、6.1節の出力の決まりを満たすこと、検証用URL集のうちブラウザが不要なサイトで、本家の `Fetcher` 以上の成功率であることである。

### 段階4 ブラウザ取得

`rustscrapling-browser` を作る。本家の `StealthyFetcher` と `DynamicFetcher` の引数、ページの使い回し、通信の記録、広告ドメインの遮断(`ad_domains.py` の一覧を移す)、DNS over HTTPS、`cdp_url` による外部ブラウザへの接続、ログイン状態の保存と読み込み、Shadow DOMの読み取り、Cloudflareの突破(`_stealth.py` の `_cloudflare_solver`)を移植する。

完了の条件は、検証用URL集のうちブラウザが必要なサイトで、本家の `StealthyFetcher` 以上の成功率であることである。届かない場合は5.1節の代替案に切り替える。

### 段階5 自動切り替えとデーモン

`rustscrapling-fetch` の自動切り替え、`rustscrapling-daemon`、6.2節の強化を作る。

完了の条件は、検証用URL集の全体で、本家の手段を手で選んだ場合の最良の成功率以上であること、150件のURLを並列数8で取得する所要時間が本家以下であること、その間のメモリが1.2節の基準を満たすこと、デーモンがある場合とない場合で同じ出力になることである。

### 段階6 クロール

`rustscrapling-spider` と `rustscrapling crawl` を作る。

完了の条件は、本家の `tests/spiders/` に相当する試験が通ること、中断と再開で取得の重複と取りこぼしがないことである。

### 段階7 MCPサーバ

`rustscrapling-mcp` を作り、7.2節のツールを提供する。

完了の条件は、本家の `tests/ai/` に相当する試験が通ることである。

### 段階8 仕上げ

ファジング、24時間の連続稼働試験(メモリの増加がないこと)、検証用URL集での最終の比較、`README.md` と `docs/` の整備を行う。1.2節の基準を全て満たしたら完了とする。

## 11. 互換性の方針

| 対象 | 方針 |
|---|---|
| adaptiveのデータベース | 本家のファイルを読み書きできる。RustScraplingが加える情報は、本家が無視するJSONのキーとして足す |
| adaptiveのスコア | 互換モードで完全に一致させる |
| 選択の結果 | 互換モードで一致させる。既定モードとの違いは `docs/COMPAT.md` に記録する |
| CLIとMCPの名前と引数 | 本家の名前を使う。CLIは7.1節の新しい体系にし、本家のコマンドとの対応を `docs/COMPAT.md` に書く |
| 既定値 | 本家と同じにする。変える場合は `docs/DECISIONS.md` に理由を書く |

## 12. 作業の進め方

1. 1つの段階、または1つのクレートを1つの作業の単位とし、ブランチを分ける。
2. 試験を先に書き、正解データと比べて通るまで実装する。
3. 判断済みの事項を変えたくなったら、作業を止めずに `docs/DECISIONS.md` に案を書き、所有者に確認する。
4. 性能の値を報告するときは、測った条件を必ず書く。測っていない値を推定で書くときは、推定であることを明記する。
5. 本家のコードを移植した部分には、BSD 3-Clauseの著作権表示を `NOTICE` に残す。

## 13. 危険と対策

| 危険 | 影響 | 対策 |
|---|---|---|
| 独自の検知回避がpatchrightに届かない | 取得成功率が下がる | 5.1節の代替案。段階4の終わりに判断する |
| html5everとlibxml2でDOMが違う | 本家のデータベースのスコアが既定モードでずれる(表の `tbody` の補完など) | 互換モードを持つ。指紋にどの解析器で作ったかを記録する |
| 環境のメモリ不足 | ビルドや試験が強制終了する | ビルドの並列数を2にする。ブラウザの同時起動数に上限を設ける |
| HTTP/3を試験できない | 機能の確認が不十分になる | 単体試験はローカルのサーバで行い、実サイトでの確認は環境が整ってから行う |
| 接続元IPの評判 | 成功率が環境に左右される | 本家と同じ環境、同じ日に比べる |
| XPathの独自実装の量 | 段階1が長引く | 本家の試験とW3Cの試験を使い、段階0の下調べで既存クレートを先に評価する |

## 14. 所有者が決めた事項

2026-11-02に所有者が次のとおり決めた。

1. ツールの名前はRustScraplingとする。既存のRUSTScrapling(2.3節)と名前が近いことを承知のうえで決めた。
2. robots.txtの順守は既定で無効とする。
3. 段階5の性能の基準で使う並列数は8とする。

## 付録A 計測スクリプト

本家のリポジトリのパスを第1引数に渡して実行する。

```python
import sys, time, tempfile, os
sys.path.insert(0, sys.argv[1])
from scrapling import Selector
from lxml import html as lh

items = "".join(
    f'<div class="product p{i%7}" data-id="{i}"><h3 class="title"><a href="/p/{i}">Product number {i} name</a></h3>'
    f'<span class="price">${i}.99</span><p class="desc">Description text for item {i} lorem ipsum</p></div>'
    for i in range(2000))
page = f"<html><body><div id='list'>{items}</div></body></html>"

def t(label, f, n=5):
    best = min((lambda s: (f(), time.perf_counter() - s)[1])(time.perf_counter()) for _ in range(n))
    print(f"{label:<38} {best*1000:9.2f} ms")

t("raw lxml parse", lambda: lh.fromstring(page))
t("Scrapling Selector() parse", lambda: Selector(page))
s = Selector(page)
root = lh.fromstring(page)
t("raw lxml cssselect .price text", lambda: [e.text for e in root.cssselect(".price")])
t("Scrapling css('.price::text').getall", lambda: s.css(".price::text").getall())
first = s.css(".product")[0]
t("find_similar (2000 siblings)", lambda: first.find_similar())
t("find_by_text", lambda: s.find_by_text("Product number 1500 name", first_match=True))

db = os.path.join(tempfile.mkdtemp(), "a.db")
args = {"storage_file": db, "url": "https://example.com"}
sa = Selector(page, url="https://example.com", adaptive=True, storage_args=args)
sa.save(sa.css(".product")[1500], "target")
changed = page.replace('class="product', 'class="item').replace("Product number", "Item no.")
sb = Selector(changed, url="https://example.com", adaptive=True, storage_args=args)
data = sb.retrieve("target")
t("relocate after layout change (~10k nodes)", lambda: sb.relocate(data, percentage=0), n=1)
```

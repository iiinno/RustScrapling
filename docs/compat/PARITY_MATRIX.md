# RustScrapling v0.5 機能対応台帳（M0・暫定）

> **これはAPIの実装完了証明ではない。** `unverified` は「未実装」と同義ではなく、Rustの対応入口・試験・本家との同等性がまだ証明されていないことを示す。
> 生成: `python oracle/inventory_upstream.py --upstream PATH --out docs/compat` と `python oracle/audit_parity.py`。手で編集せず、検証済みの実装対応・test IDは別途監査してから反映する。

- 上流固定SHA: `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`
- 主要公開クラスの直接定義メソッド宣言: **196件**（overload宣言を別行で保持。継承・module-level APIは別途棚卸し）
- 選定クラス間で静的に解決できた継承メソッド: **130件**（外部・組込み基底クラスは未監査）
- `scrapling/`全Pythonモジュール内の非privateトップレベル関数: **32件**（パッケージ直下からのimportを保証しない）
- export alias: **1件**
- CLIのコマンド関数: **10件**（既知の共通decoratorはAST展開済み。Click実行時のschemaは未検証）
- MCP: **16ツール**、BrowserAction: **12種**
- Rust公開宣言の同名候補がある直接メソッド: **48件**（同名候補であり実装・互換の証明ではない）
- 本家のメソッド/引数/デコレーターは `UPSTREAM_API_V05.json` と `MCP_ACTIONS_V05.json` に固定。ここではRust側の名前・静的schemaのみ比較。

## 1. MCPツールの静的schema差分

| Tool | 上流引数 | Rust属性 | Rustにない引数 | Rust固有引数 | 既定値差 | required差 | Field制約差 | 説明一致 | 実動検証 |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- |
| `browser_open` | 14 | 14 | — | — | — | — | — | Yes | 未検証 |
| `browser_save_state` | 2 | 2 | — | — | — | — | — | Yes | 未検証 |
| `browser_load_state` | 2 | 2 | — | — | — | — | — | Yes | 未検証 |
| `open_request_session` | 3 | 3 | — | — | — | — | — | Yes | 未検証 |
| `close_session` | 1 | 1 | — | — | — | — | — | Yes | 未検証 |
| `list_sessions` | 0 | 0 | — | — | — | — | — | Yes | 未検証 |
| `make_request` | 22 | 22 | — | — | — | — | — | Yes | 未検証 |
| `browser_fetch_once` | 27 | 27 | — | — | — | — | — | Yes | 未検証 |
| `browser_fetch` | 17 | 17 | — | — | — | — | — | Yes | 未検証 |
| `session_make_request` | 20 | 20 | — | — | — | — | — | Yes | 未検証 |
| `browser_extract` | 7 | 7 | — | — | — | — | — | Yes | 未検証 |
| `browser_actions` | 3 | 3 | — | — | — | — | — | Yes | 未検証 |
| `browser_evaluate` | 4 | 4 | — | — | — | — | — | Yes | 未検証 |
| `browser_network_requests` | 5 | 5 | — | — | — | — | — | Yes | 未検証 |
| `browser_network_request` | 3 | 3 | — | — | — | — | — | Yes | 未検証 |
| `browser_screenshot` | 7 | 7 | — | — | — | — | — | Yes | 未検証 |

## 2. BrowserAction静的差分

| Action | Rust schema登録 | 欠落フィールド | 必須指定差 | default差 | Rust実装分岐 | 実機E2E |
| --- | --- | --- | --- | --- | --- | --- |
| `move` | Yes | — | — | — | なし・unsupported | 未検証 |
| `click` | Yes | — | — | — | あり・部分対応 | 未検証 |
| `wheel` | Yes | — | — | — | なし・unsupported | 未検証 |
| `textbox` | Yes | — | — | — | あり・部分対応 | 未検証 |
| `checkbox` | Yes | — | — | — | なし・unsupported | 未検証 |
| `radio` | Yes | — | — | — | なし・unsupported | 未検証 |
| `combobox` | Yes | — | — | — | なし・unsupported | 未検証 |
| `press_key` | Yes | — | — | — | あり・部分対応 | 未検証 |
| `dialog` | Yes | — | — | — | なし・unsupported | 未検証 |
| `wait_time` | Yes | — | — | — | あり・部分対応 | 未検証 |
| `wait_element` | Yes | — | — | — | なし・unsupported | 未検証 |
| `wait_load` | Yes | — | — | — | なし・unsupported | 未検証 |

## 3. CLI公開コマンド

| ID | 上流出典 | 静的option/argument数 | 共通decorator展開 | Rust対応・E2E |
| --- | --- | ---: | --- | --- |
| `CLI.install` | `scrapling/cli.py:120` | 1 | — | 未検証 |
| `CLI.mcp` | `scrapling/cli.py:189` | 7 | — | 未検証 |
| `CLI.shell` | `scrapling/cli.py:219` | 2 | — | 未検証 |
| `CLI.extract` | `scrapling/cli.py:229` | 0 | — | 未検証 |
| `CLI.get` | `scrapling/cli.py:403` | 13 | `_common_http_options` | 未検証 |
| `CLI.post` | `scrapling/cli.py:439` | 15 | `_data_options`, `_common_http_options` | 未検証 |
| `CLI.put` | `scrapling/cli.py:478` | 15 | `_data_options`, `_common_http_options` | 未検証 |
| `CLI.delete` | `scrapling/cli.py:516` | 13 | `_common_http_options` | 未検証 |
| `CLI.fetch` | `scrapling/cli.py:609` | 22 | `_common_browser_options` | 未検証 |
| `CLI.main` | `scrapling/cli.py:666` | 1 | — | 未検証 |

## 4. 公開クラスのメソッド別追跡

| 上流API | ソース | 対応クレート候補 | Rustの同名候補 | 実動テスト |
| --- | --- | --- | --- | --- |
| `TextHandler.__getitem__` | `scrapling/core/custom_types.py:34` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.split` | `scrapling/core/custom_types.py:38` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.strip` | `scrapling/core/custom_types.py:41` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.lstrip` | `scrapling/core/custom_types.py:44` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.rstrip` | `scrapling/core/custom_types.py:47` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.capitalize` | `scrapling/core/custom_types.py:50` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.casefold` | `scrapling/core/custom_types.py:53` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.center` | `scrapling/core/custom_types.py:56` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.expandtabs` | `scrapling/core/custom_types.py:59` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.format` | `scrapling/core/custom_types.py:62` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.format_map` | `scrapling/core/custom_types.py:65` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.join` | `scrapling/core/custom_types.py:68` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.ljust` | `scrapling/core/custom_types.py:71` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.rjust` | `scrapling/core/custom_types.py:74` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.swapcase` | `scrapling/core/custom_types.py:77` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.title` | `scrapling/core/custom_types.py:80` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.translate` | `scrapling/core/custom_types.py:83` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.zfill` | `scrapling/core/custom_types.py:86` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.replace` | `scrapling/core/custom_types.py:89` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.upper` | `scrapling/core/custom_types.py:92` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.lower` | `scrapling/core/custom_types.py:95` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.sort` | `scrapling/core/custom_types.py:100` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.clean` | `scrapling/core/custom_types.py:104` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.get` | `scrapling/core/custom_types.py:112` | `rustscrapling-dom` | `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34`, `crates/rustscrapling-http/src/lib.rs:237` (ほか) | 未検証 |
| `TextHandler.getall` | `scrapling/core/custom_types.py:115` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.json` | `scrapling/core/custom_types.py:121` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.re@overload:128` | `scrapling/core/custom_types.py:128` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.re@overload:139` | `scrapling/core/custom_types.py:139` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.re` | `scrapling/core/custom_types.py:148` | `rustscrapling-dom` | — | 未検証 |
| `TextHandler.re_first` | `scrapling/core/custom_types.py:184` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.__getitem__@overload:218` | `scrapling/core/custom_types.py:218` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.__getitem__@overload:222` | `scrapling/core/custom_types.py:222` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.__getitem__` | `scrapling/core/custom_types.py:225` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.re` | `scrapling/core/custom_types.py:231` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.re_first` | `scrapling/core/custom_types.py:249` | `rustscrapling-dom` | — | 未検証 |
| `TextHandlers.get` | `scrapling/core/custom_types.py:272` | `rustscrapling-dom` | `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34`, `crates/rustscrapling-http/src/lib.rs:237` (ほか) | 未検証 |
| `TextHandlers.extract` | `scrapling/core/custom_types.py:278` | `rustscrapling-dom` | `crates/rustscrapling-extract/src/lib.rs:40`, `crates/rustscrapling-fetch/src/content.rs:106` | 未検証 |
| `AttributesHandler.__init__` | `scrapling/core/custom_types.py:292` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.get` | `scrapling/core/custom_types.py:307` | `rustscrapling-dom` | `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34`, `crates/rustscrapling-http/src/lib.rs:237` (ほか) | 未検証 |
| `AttributesHandler.search_values` | `scrapling/core/custom_types.py:311` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.json_string` | `scrapling/core/custom_types.py:325` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.__getitem__` | `scrapling/core/custom_types.py:329` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.__iter__` | `scrapling/core/custom_types.py:332` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.__len__` | `scrapling/core/custom_types.py:335` | `rustscrapling-dom` | — | 未検証 |
| `AttributesHandler.__contains__` | `scrapling/core/custom_types.py:344` | `rustscrapling-dom` | — | 未検証 |
| `SelectorsGeneration.generate_css_selector` | `scrapling/core/mixins.py:66` | `rustscrapling-select` | `crates/rustscrapling-select/src/generate.rs:81` | 未検証 |
| `SelectorsGeneration.generate_full_css_selector` | `scrapling/core/mixins.py:73` | `rustscrapling-select` | `crates/rustscrapling-select/src/generate.rs:85` | 未検証 |
| `SelectorsGeneration.generate_xpath_selector` | `scrapling/core/mixins.py:80` | `rustscrapling-select` | `crates/rustscrapling-select/src/generate.rs:89` | 未検証 |
| `SelectorsGeneration.generate_full_xpath_selector` | `scrapling/core/mixins.py:87` | `rustscrapling-select` | `crates/rustscrapling-select/src/generate.rs:93` | 未検証 |
| `StealthySession.__init__` | `scrapling/engines/_browsers/_stealth.py:39` | `rustscrapling-browser` | — | 未検証 |
| `StealthySession.start` | `scrapling/engines/_browsers/_stealth.py:83` | `rustscrapling-browser` | `crates/rustscrapling-daemon/src/service.rs:500` | 未検証 |
| `StealthySession.fetch` | `scrapling/engines/_browsers/_stealth.py:231` | `rustscrapling-browser` | `crates/rustscrapling-daemon/src/service.rs:852`, `crates/rustscrapling-fetch/src/lib.rs:211`, `crates/rustscrapling-http/src/quic.rs:70` | 未検証 |
| `AsyncStealthySession.__init__` | `scrapling/engines/_browsers/_stealth.py:356` | `rustscrapling-browser` | — | 未検証 |
| `AsyncStealthySession.start` | `scrapling/engines/_browsers/_stealth.py:401` | `rustscrapling-browser` | `crates/rustscrapling-daemon/src/service.rs:500` | 未検証 |
| `AsyncStealthySession.fetch` | `scrapling/engines/_browsers/_stealth.py:553` | `rustscrapling-browser` | `crates/rustscrapling-daemon/src/service.rs:852`, `crates/rustscrapling-fetch/src/lib.rs:211`, `crates/rustscrapling-http/src/quic.rs:70` | 未検証 |
| `FetcherSession.__init__` | `scrapling/engines/static.py:678` | `rustscrapling-http` | — | 未検証 |
| `Response.__init__` | `scrapling/engines/toolbelt/custom.py:44` | `rustscrapling-fetch` | — | 未検証 |
| `Response.body` | `scrapling/engines/toolbelt/custom.py:88` | `rustscrapling-fetch` | `crates/rustscrapling-http/src/lib.rs:254` | 未検証 |
| `Response.markdown` | `scrapling/engines/toolbelt/custom.py:92` | `rustscrapling-fetch` | `crates/rustscrapling-extract/src/lib.rs:80` | 未検証 |
| `Response.follow` | `scrapling/engines/toolbelt/custom.py:108` | `rustscrapling-fetch` | — | 未検証 |
| `BaseFetcher.__init__` | `scrapling/engines/toolbelt/custom.py:193` | `rustscrapling-fetch` | — | 未検証 |
| `BaseFetcher.display_config` | `scrapling/engines/toolbelt/custom.py:206` | `rustscrapling-fetch` | — | 未検証 |
| `BaseFetcher.configure` | `scrapling/engines/toolbelt/custom.py:217` | `rustscrapling-fetch` | — | 未検証 |
| `Fetcher.get` | `scrapling/fetchers/requests.py:32` | `rustscrapling-http` | `crates/rustscrapling-http/src/lib.rs:237`, `crates/rustscrapling-http/src/lib.rs:430`, `crates/rustscrapling-browser/src/lib.rs:446` (ほか) | 未検証 |
| `Fetcher.post` | `scrapling/fetchers/requests.py:36` | `rustscrapling-http` | — | 未検証 |
| `Fetcher.put` | `scrapling/fetchers/requests.py:40` | `rustscrapling-http` | — | 未検証 |
| `Fetcher.delete` | `scrapling/fetchers/requests.py:44` | `rustscrapling-http` | — | 未検証 |
| `AsyncFetcher.get` | `scrapling/fetchers/requests.py:52` | `rustscrapling-http` | `crates/rustscrapling-http/src/lib.rs:237`, `crates/rustscrapling-http/src/lib.rs:430`, `crates/rustscrapling-browser/src/lib.rs:446` (ほか) | 未検証 |
| `AsyncFetcher.post` | `scrapling/fetchers/requests.py:56` | `rustscrapling-http` | — | 未検証 |
| `AsyncFetcher.put` | `scrapling/fetchers/requests.py:60` | `rustscrapling-http` | — | 未検証 |
| `AsyncFetcher.delete` | `scrapling/fetchers/requests.py:64` | `rustscrapling-http` | — | 未検証 |
| `StealthyFetcher.fetch` | `scrapling/fetchers/stealth_chrome.py:14` | `rustscrapling-browser` | `crates/rustscrapling-daemon/src/service.rs:852`, `crates/rustscrapling-fetch/src/lib.rs:211`, `crates/rustscrapling-http/src/quic.rs:70` | 未検証 |
| `StealthyFetcher.async_fetch` | `scrapling/fetchers/stealth_chrome.py:67` | `rustscrapling-browser` | — | 未検証 |
| `Selector.__init__` | `scrapling/parser.py:92` | `rustscrapling-select` | — | 未検証 |
| `Selector.__getitem__` | `scrapling/parser.py:191` | `rustscrapling-select` | — | 未検証 |
| `Selector.__contains__` | `scrapling/parser.py:196` | `rustscrapling-select` | — | 未検証 |
| `Selector.tag` | `scrapling/parser.py:265` | `rustscrapling-select` | — | 未検証 |
| `Selector.text` | `scrapling/parser.py:274` | `rustscrapling-select` | `crates/rustscrapling-dom/src/lib.rs:247` | 未検証 |
| `Selector.get_all_text` | `scrapling/parser.py:284` | `rustscrapling-select` | — | 未検証 |
| `Selector.urljoin` | `scrapling/parser.py:334` | `rustscrapling-select` | — | 未検証 |
| `Selector.attrib` | `scrapling/parser.py:339` | `rustscrapling-select` | — | 未検証 |
| `Selector.html_content` | `scrapling/parser.py:348` | `rustscrapling-select` | — | 未検証 |
| `Selector.body` | `scrapling/parser.py:358` | `rustscrapling-select` | `crates/rustscrapling-http/src/lib.rs:254` | 未検証 |
| `Selector.prettify` | `scrapling/parser.py:364` | `rustscrapling-select` | — | 未検証 |
| `Selector.has_class` | `scrapling/parser.py:379` | `rustscrapling-select` | — | 未検証 |
| `Selector.parent` | `scrapling/parser.py:389` | `rustscrapling-select` | `crates/rustscrapling-dom/src/lib.rs:195` | 未検証 |
| `Selector.below_elements` | `scrapling/parser.py:395` | `rustscrapling-select` | — | 未検証 |
| `Selector.children` | `scrapling/parser.py:403` | `rustscrapling-select` | `crates/rustscrapling-dom/src/lib.rs:199` | 未検証 |
| `Selector.siblings` | `scrapling/parser.py:414` | `rustscrapling-select` | — | 未検証 |
| `Selector.iterancestors` | `scrapling/parser.py:420` | `rustscrapling-select` | — | 未検証 |
| `Selector.find_ancestor` | `scrapling/parser.py:427` | `rustscrapling-select` | — | 未検証 |
| `Selector.path` | `scrapling/parser.py:438` | `rustscrapling-select` | — | 未検証 |
| `Selector.next` | `scrapling/parser.py:444` | `rustscrapling-select` | — | 未検証 |
| `Selector.previous` | `scrapling/parser.py:456` | `rustscrapling-select` | — | 未検証 |
| `Selector.get` | `scrapling/parser.py:467` | `rustscrapling-select` | `crates/rustscrapling-select/src/lib.rs:23`, `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34` (ほか) | 未検証 |
| `Selector.getall` | `scrapling/parser.py:476` | `rustscrapling-select` | — | 未検証 |
| `Selector.relocate@overload:513` | `scrapling/parser.py:513` | `rustscrapling-select` | `crates/rustscrapling-adaptive/src/scoring.rs:332` | 未検証 |
| `Selector.relocate@overload:518` | `scrapling/parser.py:518` | `rustscrapling-select` | `crates/rustscrapling-adaptive/src/scoring.rs:332` | 未検証 |
| `Selector.relocate` | `scrapling/parser.py:522` | `rustscrapling-select` | `crates/rustscrapling-adaptive/src/scoring.rs:332` | 未検証 |
| `Selector.css` | `scrapling/parser.py:571` | `rustscrapling-select` | `crates/rustscrapling-select/src/css.rs:92` | 未検証 |
| `Selector.xpath` | `scrapling/parser.py:631` | `rustscrapling-select` | `crates/rustscrapling-select/src/xpath.rs:839` | 未検証 |
| `Selector.find_all` | `scrapling/parser.py:701` | `rustscrapling-select` | — | 未検証 |
| `Selector.find` | `scrapling/parser.py:799` | `rustscrapling-select` | — | 未検証 |
| `Selector.save` | `scrapling/parser.py:888` | `rustscrapling-select` | — | 未検証 |
| `Selector.retrieve` | `scrapling/parser.py:909` | `rustscrapling-select` | `crates/rustscrapling-adaptive/src/storage.rs:87` | 未検証 |
| `Selector.json` | `scrapling/parser.py:924` | `rustscrapling-select` | — | 未検証 |
| `Selector.re` | `scrapling/parser.py:940` | `rustscrapling-select` | — | 未検証 |
| `Selector.re_first` | `scrapling/parser.py:956` | `rustscrapling-select` | — | 未検証 |
| `Selector.find_similar` | `scrapling/parser.py:1022` | `rustscrapling-select` | `crates/rustscrapling-adaptive/src/scoring.rs:432` | 未検証 |
| `Selector.find_by_text@overload:1084` | `scrapling/parser.py:1084` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:116` | 未検証 |
| `Selector.find_by_text@overload:1094` | `scrapling/parser.py:1094` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:116` | 未検証 |
| `Selector.find_by_text` | `scrapling/parser.py:1103` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:116` | 未検証 |
| `Selector.find_by_regex@overload:1152` | `scrapling/parser.py:1152` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:171` | 未検証 |
| `Selector.find_by_regex@overload:1161` | `scrapling/parser.py:1161` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:171` | 未検証 |
| `Selector.find_by_regex` | `scrapling/parser.py:1169` | `rustscrapling-select` | `crates/rustscrapling-select/src/text_search.rs:171` | 未検証 |
| `Selectors.__getitem__@overload:1217` | `scrapling/parser.py:1217` | `rustscrapling-select` | — | 未検証 |
| `Selectors.__getitem__@overload:1221` | `scrapling/parser.py:1221` | `rustscrapling-select` | — | 未検証 |
| `Selectors.__getitem__` | `scrapling/parser.py:1224` | `rustscrapling-select` | — | 未検証 |
| `Selectors.xpath` | `scrapling/parser.py:1231` | `rustscrapling-select` | `crates/rustscrapling-select/src/xpath.rs:839` | 未検証 |
| `Selectors.css` | `scrapling/parser.py:1262` | `rustscrapling-select` | `crates/rustscrapling-select/src/css.rs:92` | 未検証 |
| `Selectors.re` | `scrapling/parser.py:1290` | `rustscrapling-select` | — | 未検証 |
| `Selectors.re_first` | `scrapling/parser.py:1308` | `rustscrapling-select` | — | 未検証 |
| `Selectors.search` | `scrapling/parser.py:1330` | `rustscrapling-select` | — | 未検証 |
| `Selectors.filter` | `scrapling/parser.py:1340` | `rustscrapling-select` | — | 未検証 |
| `Selectors.get@overload:1348` | `scrapling/parser.py:1348` | `rustscrapling-select` | `crates/rustscrapling-select/src/lib.rs:23`, `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34` (ほか) | 未検証 |
| `Selectors.get@overload:1351` | `scrapling/parser.py:1351` | `rustscrapling-select` | `crates/rustscrapling-select/src/lib.rs:23`, `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34` (ほか) | 未検証 |
| `Selectors.get` | `scrapling/parser.py:1353` | `rustscrapling-select` | `crates/rustscrapling-select/src/lib.rs:23`, `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34` (ほか) | 未検証 |
| `Selectors.getall` | `scrapling/parser.py:1361` | `rustscrapling-select` | — | 未検証 |
| `Selectors.first` | `scrapling/parser.py:1369` | `rustscrapling-select` | — | 未検証 |
| `Selectors.last` | `scrapling/parser.py:1374` | `rustscrapling-select` | — | 未検証 |
| `Selectors.length` | `scrapling/parser.py:1379` | `rustscrapling-select` | — | 未検証 |
| `CrawlerEngine.__init__` | `scrapling/spiders/engine.py:32` | `rustscrapling-spider` | — | 未検証 |
| `CrawlerEngine.request_pause` | `scrapling/spiders/engine.py:280` | `rustscrapling-spider` | — | 未検証 |
| `CrawlerEngine.crawl` | `scrapling/spiders/engine.py:356` | `rustscrapling-spider` | `crates/rustscrapling-spider/src/lib.rs:818` | 未検証 |
| `CrawlerEngine.items` | `scrapling/spiders/engine.py:456` | `rustscrapling-spider` | — | 未検証 |
| `LinkExtractor.__init__` | `scrapling/spiders/links.py:194` | `rustscrapling-spider` | — | 未検証 |
| `LinkExtractor.extract` | `scrapling/spiders/links.py:226` | `rustscrapling-spider` | `crates/rustscrapling-extract/src/lib.rs:40`, `crates/rustscrapling-fetch/src/content.rs:106` | 未検証 |
| `LinkExtractor.matches` | `scrapling/spiders/links.py:272` | `rustscrapling-spider` | — | 未検証 |
| `Request.__init__` | `scrapling/spiders/request.py:34` | `rustscrapling-spider` | — | 未検証 |
| `Request.copy` | `scrapling/spiders/request.py:55` | `rustscrapling-spider` | — | 未検証 |
| `Request.domain` | `scrapling/spiders/request.py:69` | `rustscrapling-spider` | — | 未検証 |
| `Request.update_fingerprint` | `scrapling/spiders/request.py:72` | `rustscrapling-spider` | — | 未検証 |
| `CrawlResult.completed` | `scrapling/spiders/result.py:207` | `rustscrapling-spider` | — | 未検証 |
| `CrawlResult.__len__` | `scrapling/spiders/result.py:211` | `rustscrapling-spider` | — | 未検証 |
| `CrawlResult.__iter__` | `scrapling/spiders/result.py:214` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.__init__` | `scrapling/spiders/scheduler.py:21` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.enqueue` | `scrapling/spiders/scheduler.py:41` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.dequeue` | `scrapling/spiders/scheduler.py:58` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.complete` | `scrapling/spiders/scheduler.py:64` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.__len__` | `scrapling/spiders/scheduler.py:74` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.is_empty` | `scrapling/spiders/scheduler.py:78` | `rustscrapling-spider` | `crates/rustscrapling-http/src/lib.rs:173` | 未検証 |
| `Scheduler.snapshot` | `scrapling/spiders/scheduler.py:81` | `rustscrapling-spider` | — | 未検証 |
| `Scheduler.restore` | `scrapling/spiders/scheduler.py:87` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.__init__` | `scrapling/spiders/session.py:15` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.add` | `scrapling/spiders/session.py:22` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.remove` | `scrapling/spiders/session.py:43` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.pop` | `scrapling/spiders/session.py:50` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.default_session_id` | `scrapling/spiders/session.py:68` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.session_ids` | `scrapling/spiders/session.py:74` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.get` | `scrapling/spiders/session.py:77` | `rustscrapling-spider` | `crates/rustscrapling-browser/src/lib.rs:446`, `crates/rustscrapling-daemon/src/protocol.rs:34`, `crates/rustscrapling-http/src/lib.rs:237` (ほか) | 未検証 |
| `SessionManager.start` | `scrapling/spiders/session.py:83` | `rustscrapling-spider` | `crates/rustscrapling-daemon/src/service.rs:500` | 未検証 |
| `SessionManager.close` | `scrapling/spiders/session.py:94` | `rustscrapling-spider` | `crates/rustscrapling-browser/src/lib.rs:1421` | 未検証 |
| `SessionManager.fetch` | `scrapling/spiders/session.py:103` | `rustscrapling-spider` | `crates/rustscrapling-daemon/src/service.rs:852`, `crates/rustscrapling-fetch/src/lib.rs:211`, `crates/rustscrapling-http/src/quic.rs:70` | 未検証 |
| `SessionManager.__contains__` | `scrapling/spiders/session.py:146` | `rustscrapling-spider` | — | 未検証 |
| `SessionManager.__len__` | `scrapling/spiders/session.py:150` | `rustscrapling-spider` | — | 未検証 |
| `Spider.__init__` | `scrapling/spiders/spider.py:106` | `rustscrapling-spider` | — | 未検証 |
| `Spider.start_requests` | `scrapling/spiders/spider.py:155` | `rustscrapling-spider` | — | 未検証 |
| `Spider.parse` | `scrapling/spiders/spider.py:173` | `rustscrapling-spider` | — | 未検証 |
| `Spider.on_start` | `scrapling/spiders/spider.py:178` | `rustscrapling-spider` | — | 未検証 |
| `Spider.on_close` | `scrapling/spiders/spider.py:188` | `rustscrapling-spider` | — | 未検証 |
| `Spider.on_error` | `scrapling/spiders/spider.py:192` | `rustscrapling-spider` | — | 未検証 |
| `Spider.on_scraped_item` | `scrapling/spiders/spider.py:200` | `rustscrapling-spider` | — | 未検証 |
| `Spider.is_blocked` | `scrapling/spiders/spider.py:204` | `rustscrapling-spider` | — | 未検証 |
| `Spider.retry_blocked_request` | `scrapling/spiders/spider.py:210` | `rustscrapling-spider` | — | 未検証 |
| `Spider.configure_sessions` | `scrapling/spiders/spider.py:218` | `rustscrapling-spider` | — | 未検証 |
| `Spider.pause` | `scrapling/spiders/spider.py:232` | `rustscrapling-spider` | — | 未検証 |
| `Spider.start` | `scrapling/spiders/spider.py:278` | `rustscrapling-spider` | `crates/rustscrapling-daemon/src/service.rs:500` | 未検証 |
| `Spider.stream` | `scrapling/spiders/spider.py:304` | `rustscrapling-spider` | — | 未検証 |
| `Spider.stats` | `scrapling/spiders/spider.py:326` | `rustscrapling-spider` | — | 未検証 |
| `CrawlSpider.rules` | `scrapling/spiders/templates/crawler.py:63` | `rustscrapling-spider` | — | 未検証 |
| `CrawlSpider.parse` | `scrapling/spiders/templates/crawler.py:67` | `rustscrapling-spider` | — | 未検証 |
| `XMLFeedSpider.parse` | `scrapling/spiders/templates/feed.py:48` | `rustscrapling-spider` | — | 未検証 |
| `XMLFeedSpider.parse_node` | `scrapling/spiders/templates/feed.py:67` | `rustscrapling-spider` | — | 未検証 |
| `CSVFeedSpider.parse` | `scrapling/spiders/templates/feed.py:120` | `rustscrapling-spider` | — | 未検証 |
| `CSVFeedSpider.parse_row` | `scrapling/spiders/templates/feed.py:136` | `rustscrapling-spider` | — | 未検証 |
| `ShopifySpider.__init__` | `scrapling/spiders/templates/shopify.py:39` | `rustscrapling-spider` | — | 未検証 |
| `ShopifySpider.start_requests` | `scrapling/spiders/templates/shopify.py:47` | `rustscrapling-spider` | — | 未検証 |
| `ShopifySpider.parse` | `scrapling/spiders/templates/shopify.py:54` | `rustscrapling-spider` | — | 未検証 |
| `ShopifySpider.parse_collection` | `scrapling/spiders/templates/shopify.py:96` | `rustscrapling-spider` | — | 未検証 |
| `ContentSpider.__init__` | `scrapling/spiders/templates/site_to_markdown.py:51` | `rustscrapling-spider` | — | 未検証 |
| `ContentSpider.rules` | `scrapling/spiders/templates/site_to_markdown.py:60` | `rustscrapling-spider` | — | 未検証 |
| `ContentSpider.parse` | `scrapling/spiders/templates/site_to_markdown.py:64` | `rustscrapling-spider` | — | 未検証 |
| `ContentSpider.on_scraped_item` | `scrapling/spiders/templates/site_to_markdown.py:78` | `rustscrapling-spider` | — | 未検証 |
| `SitemapSpider.rules` | `scrapling/spiders/templates/sitemap.py:61` | `rustscrapling-spider` | — | 未検証 |
| `SitemapSpider.start_requests` | `scrapling/spiders/templates/sitemap.py:65` | `rustscrapling-spider` | — | 未検証 |
| `SitemapSpider.parse` | `scrapling/spiders/templates/sitemap.py:73` | `rustscrapling-spider` | — | 未検証 |

## 5. 継承・エクスポートalias

| 継承API | 宣言元 | 対応クレート候補 | 実動作 |
| --- | --- | --- | --- |
| `AsyncFetcher.__init__` | `BaseFetcher.__init__` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `AsyncFetcher.configure` | `BaseFetcher.configure` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `AsyncFetcher.display_config` | `BaseFetcher.display_config` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `CSVFeedSpider.__init__` | `Spider.__init__` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.configure_sessions` | `Spider.configure_sessions` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.is_blocked` | `Spider.is_blocked` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.on_close` | `Spider.on_close` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.on_error` | `Spider.on_error` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.on_scraped_item` | `Spider.on_scraped_item` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.on_start` | `Spider.on_start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.pause` | `Spider.pause` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.retry_blocked_request` | `Spider.retry_blocked_request` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.start` | `Spider.start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.start_requests` | `Spider.start_requests` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.stats` | `Spider.stats` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CSVFeedSpider.stream` | `Spider.stream` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.configure_sessions` | `Spider.configure_sessions` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.is_blocked` | `Spider.is_blocked` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.on_close` | `Spider.on_close` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.on_error` | `Spider.on_error` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.on_start` | `Spider.on_start` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.pause` | `Spider.pause` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.retry_blocked_request` | `Spider.retry_blocked_request` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.start` | `Spider.start` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.start_requests` | `Spider.start_requests` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.stats` | `Spider.stats` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `ContentSpider.stream` | `Spider.stream` via `CrawlSpider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.__init__` | `Spider.__init__` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.configure_sessions` | `Spider.configure_sessions` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.is_blocked` | `Spider.is_blocked` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.on_close` | `Spider.on_close` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.on_error` | `Spider.on_error` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.on_scraped_item` | `Spider.on_scraped_item` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.on_start` | `Spider.on_start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.pause` | `Spider.pause` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.retry_blocked_request` | `Spider.retry_blocked_request` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.start` | `Spider.start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.start_requests` | `Spider.start_requests` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.stats` | `Spider.stats` via `Spider` | `rustscrapling-spider` | 未検証 |
| `CrawlSpider.stream` | `Spider.stream` via `Spider` | `rustscrapling-spider` | 未検証 |
| `Fetcher.__init__` | `BaseFetcher.__init__` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `Fetcher.configure` | `BaseFetcher.configure` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `Fetcher.display_config` | `BaseFetcher.display_config` via `BaseFetcher` | `rustscrapling-http` | 未検証 |
| `Response.__contains__` | `Selector.__contains__` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.__getitem__` | `Selector.__getitem__` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.attrib` | `Selector.attrib` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.below_elements` | `Selector.below_elements` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.children` | `Selector.children` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.css` | `Selector.css` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find` | `Selector.find` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_all` | `Selector.find_all` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_ancestor` | `Selector.find_ancestor` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_regex` | `Selector.find_by_regex` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_regex@overload:1152` | `Selector.find_by_regex@overload:1152` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_regex@overload:1161` | `Selector.find_by_regex@overload:1161` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_text` | `Selector.find_by_text` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_text@overload:1084` | `Selector.find_by_text@overload:1084` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_by_text@overload:1094` | `Selector.find_by_text@overload:1094` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.find_similar` | `Selector.find_similar` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.generate_css_selector` | `SelectorsGeneration.generate_css_selector` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.generate_full_css_selector` | `SelectorsGeneration.generate_full_css_selector` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.generate_full_xpath_selector` | `SelectorsGeneration.generate_full_xpath_selector` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.generate_xpath_selector` | `SelectorsGeneration.generate_xpath_selector` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.get` | `Selector.get` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.get_all_text` | `Selector.get_all_text` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.getall` | `Selector.getall` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.has_class` | `Selector.has_class` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.html_content` | `Selector.html_content` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.iterancestors` | `Selector.iterancestors` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.json` | `Selector.json` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.next` | `Selector.next` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.parent` | `Selector.parent` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.path` | `Selector.path` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.prettify` | `Selector.prettify` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.previous` | `Selector.previous` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.re` | `Selector.re` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.re_first` | `Selector.re_first` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.relocate` | `Selector.relocate` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.relocate@overload:513` | `Selector.relocate@overload:513` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.relocate@overload:518` | `Selector.relocate@overload:518` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.retrieve` | `Selector.retrieve` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.save` | `Selector.save` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.siblings` | `Selector.siblings` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.tag` | `Selector.tag` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.text` | `Selector.text` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.urljoin` | `Selector.urljoin` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Response.xpath` | `Selector.xpath` via `Selector` | `rustscrapling-fetch` | 未検証 |
| `Selector.generate_css_selector` | `SelectorsGeneration.generate_css_selector` via `SelectorsGeneration` | `rustscrapling-select` | 未検証 |
| `Selector.generate_full_css_selector` | `SelectorsGeneration.generate_full_css_selector` via `SelectorsGeneration` | `rustscrapling-select` | 未検証 |
| `Selector.generate_full_xpath_selector` | `SelectorsGeneration.generate_full_xpath_selector` via `SelectorsGeneration` | `rustscrapling-select` | 未検証 |
| `Selector.generate_xpath_selector` | `SelectorsGeneration.generate_xpath_selector` via `SelectorsGeneration` | `rustscrapling-select` | 未検証 |
| `ShopifySpider.configure_sessions` | `Spider.configure_sessions` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.is_blocked` | `Spider.is_blocked` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.on_close` | `Spider.on_close` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.on_error` | `Spider.on_error` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.on_scraped_item` | `Spider.on_scraped_item` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.on_start` | `Spider.on_start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.pause` | `Spider.pause` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.retry_blocked_request` | `Spider.retry_blocked_request` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.start` | `Spider.start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.stats` | `Spider.stats` via `Spider` | `rustscrapling-spider` | 未検証 |
| `ShopifySpider.stream` | `Spider.stream` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.__init__` | `Spider.__init__` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.configure_sessions` | `Spider.configure_sessions` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.is_blocked` | `Spider.is_blocked` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.on_close` | `Spider.on_close` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.on_error` | `Spider.on_error` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.on_scraped_item` | `Spider.on_scraped_item` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.on_start` | `Spider.on_start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.pause` | `Spider.pause` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.retry_blocked_request` | `Spider.retry_blocked_request` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.start` | `Spider.start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.stats` | `Spider.stats` via `Spider` | `rustscrapling-spider` | 未検証 |
| `SitemapSpider.stream` | `Spider.stream` via `Spider` | `rustscrapling-spider` | 未検証 |
| `StealthyFetcher.__init__` | `BaseFetcher.__init__` via `BaseFetcher` | `rustscrapling-browser` | 未検証 |
| `StealthyFetcher.configure` | `BaseFetcher.configure` via `BaseFetcher` | `rustscrapling-browser` | 未検証 |
| `StealthyFetcher.display_config` | `BaseFetcher.display_config` via `BaseFetcher` | `rustscrapling-browser` | 未検証 |
| `XMLFeedSpider.__init__` | `Spider.__init__` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.configure_sessions` | `Spider.configure_sessions` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.is_blocked` | `Spider.is_blocked` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.on_close` | `Spider.on_close` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.on_error` | `Spider.on_error` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.on_scraped_item` | `Spider.on_scraped_item` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.on_start` | `Spider.on_start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.pause` | `Spider.pause` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.retry_blocked_request` | `Spider.retry_blocked_request` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.start` | `Spider.start` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.start_requests` | `Spider.start_requests` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.stats` | `Spider.stats` via `Spider` | `rustscrapling-spider` | 未検証 |
| `XMLFeedSpider.stream` | `Spider.stream` via `Spider` | `rustscrapling-spider` | 未検証 |

| 公開alias | 実体 | 出典 |
| --- | --- | --- |
| `SiteToMarkdownSpider` | `ContentSpider` | `scrapling/spiders/templates/site_to_markdown.py:96` |

## 6. モジュールレベル関数

| 関数 | 参照元 | `__all__`明示 | Rust対応・テスト |
| --- | --- | --- | --- |
| `scrapling.cli.install` | `scrapling/cli.py:120` | No | 未検証 |
| `scrapling.cli.mcp` | `scrapling/cli.py:189` | No | 未検証 |
| `scrapling.cli.shell` | `scrapling/cli.py:219` | No | 未検証 |
| `scrapling.cli.extract` | `scrapling/cli.py:229` | No | 未検証 |
| `scrapling.cli.get` | `scrapling/cli.py:403` | No | 未検証 |
| `scrapling.cli.post` | `scrapling/cli.py:439` | No | 未検証 |
| `scrapling.cli.put` | `scrapling/cli.py:478` | No | 未検証 |
| `scrapling.cli.delete` | `scrapling/cli.py:516` | No | 未検証 |
| `scrapling.cli.fetch` | `scrapling/cli.py:609` | No | 未検証 |
| `scrapling.cli.main` | `scrapling/cli.py:666` | No | 未検証 |
| `scrapling.core.shell.show_page_in_browser` | `scrapling/core/shell.py:416` | No | 未検証 |
| `scrapling.core.translator.css_to_xpath` | `scrapling/core/translator.py:132` | No | 未検証 |
| `scrapling.core.utils._utils.setup_logger` | `scrapling/core/utils/_utils.py:20` | No | 未検証 |
| `scrapling.core.utils._utils.set_logger` | `scrapling/core/utils/_utils.py:51` | No | 未検証 |
| `scrapling.core.utils._utils.reset_logger` | `scrapling/core/utils/_utils.py:56` | No | 未検証 |
| `scrapling.core.utils._utils.flatten` | `scrapling/core/utils/_utils.py:61` | No | 未検証 |
| `scrapling.core.utils._utils.clean_spaces` | `scrapling/core/utils/_utils.py:118` | No | 未検証 |
| `scrapling.engines._browsers._validators.validate_fetch` | `scrapling/engines/_browsers/_validators.py:179` | No | 未検証 |
| `scrapling.engines._browsers._validators.validate` | `scrapling/engines/_browsers/_validators.py:242` | No | 未検証 |
| `scrapling.engines._browsers._validators.validate` | `scrapling/engines/_browsers/_validators.py:246` | No | 未検証 |
| `scrapling.engines._browsers._validators.validate` | `scrapling/engines/_browsers/_validators.py:249` | No | 未検証 |
| `scrapling.engines.toolbelt.fingerprints.get_os_name` | `scrapling/engines/toolbelt/fingerprints.py:25` | No | 未検証 |
| `scrapling.engines.toolbelt.fingerprints.driven_browser_version` | `scrapling/engines/toolbelt/fingerprints.py:46` | No | 未検証 |
| `scrapling.engines.toolbelt.fingerprints.generate_headers` | `scrapling/engines/toolbelt/fingerprints.py:66` | No | 未検証 |
| `scrapling.engines.toolbelt.navigation.create_intercept_handler` | `scrapling/engines/toolbelt/navigation.py:43` | No | 未検証 |
| `scrapling.engines.toolbelt.navigation.create_async_intercept_handler` | `scrapling/engines/toolbelt/navigation.py:70` | No | 未検証 |
| `scrapling.engines.toolbelt.navigation.construct_proxy_dict` | `scrapling/engines/toolbelt/navigation.py:97` | No | 未検証 |
| `scrapling.engines.toolbelt.proxy_rotation.is_proxy_error` | `scrapling/engines/toolbelt/proxy_rotation.py:27` | No | 未検証 |
| `scrapling.engines.toolbelt.proxy_rotation.cyclic_rotation` | `scrapling/engines/toolbelt/proxy_rotation.py:33` | No | 未検証 |
| `scrapling.integrations.scrapy.convert_response` | `scrapling/integrations/scrapy.py:24` | Yes | 未検証 |
| `scrapling.integrations.scrapy.scrapling_response` | `scrapling/integrations/scrapy.py:59` | Yes | 未検証 |
| `scrapling.spiders.throttle.parse_retry_after` | `scrapling/spiders/throttle.py:10` | No | 未検証 |

## 7. 残作業（重要）

1. `Unpack[TypedDict]`と共通CLIデコレーターのAST抽出結果、外部基底クラスの継承API、module-level公開関数を本家実行時schemaと照合する。
2. Rustの実entrypoint・引数の既定値・結果/エラーにunit/fixture/E2Eを紐付ける。名前やschema一致だけでverifiedにしない。
3. 特にBrowserActionはschema登録済みでも各executorが動く保証はない。`docs/COMPAT.md`のunsupportedを実機テストで解消する。
4. 最新の統合HEADで再監査し、上流とRustを同一環境で比較する。

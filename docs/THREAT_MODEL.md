# THREAT_MODEL.md — Plan 7.1 / G-11 対応表

対象：Plan 7.1 の攻撃面 (a)〜(f) と重要資産（cookie/token/proxy認証、ホストファイル、ブラウザprofile、ログ、AIエージェントの判断）。
各行は「脅威 → 実装上の対策 → 回帰テスト（ファイル::テスト名）」。テストは特記なき限り `cargo test --workspace` で実行される決定的テスト（ネットワーク不要、localhostのみ）。
**この表はG-11のPASS宣言ではない。** 状態は `docs/GA_GATES.md` が正。未対策・部分対策は末尾「既知の残余リスク」に列挙する。

## (a) 不正HTML/JSとページ本文

| 脅威 | 対策 | 回帰テスト |
|---|---|---|
| 抽出結果で`<script>`等の実行要素が復活／隠し要素の命令がAIに渡る | 本文抽出はnoise/hidden要素を除去、Markdown化で実行要素を出さない。取得本文は命令権限を持たない（ツール出力はデータとして返す） | `extract/tests/stage3_fixtures.rs::malicious_markup_is_inert_and_hidden_instructions_are_removed`、`extract/tests/compat_oracle.rs`（本家Convertorと240/240一致） |
| 抽出メタデータに資格情報付きURL・内部IDが混入 | canonical/記事URLの資格情報を出力しない、JSON-LD循環参照を打ち切る | `stage3_fixtures.rs::credential_bearing_urls_are_not_emitted_in_article_or_canonical_metadata`、`::jsonld_author_cycle_or_unresolved_reference_does_not_leak_ids`、`::protocol_relative_credentials_and_backslash_authorities_are_rejected` |
| パーサ/セレクタ/adaptive/Markdownのクラッシュ・ハング（DoS） | libFuzzer 4ターゲット×1時間（G-09）、発見不具合は回帰テスト化 | `select/tests/fuzz_regressions.rs::trailing_backslash_in_css_identifier_is_not_a_panic`、`adaptive/tests/fuzz_regressions.rs`（relocateデッドロック） |
| ブラウザページから自動化を検知・妨害 | `Runtime.enable`を自動送信しない（D-007）、隔離worldでのみ評価 | `browser/tests/actions_chrome_e2e.rs::real_chrome_click_and_unicode_input_without_implicit_runtime`（実Chrome、opt-in） |

## (b) HTTP応答・redirect・URL

| 脅威 | 対策 | 回帰テスト |
|---|---|---|
| 資格情報付きURL・protocol-relative・backslash authority・制御文字 | URL受理を明示schemeに限定し拒否、エラーに秘密値を出さない | `http/tests/http_local.rs::embedded_url_credentials_are_rejected_without_leaking_secret_values`、`spider/src/lib.rs::rejects_unsafe_urls_and_dedupes_fragments` |
| 資格情報付きLocation・内部アドレスへのredirect（SSRF） | redirectを手動処理、資格情報付きLocationは接続前に拒否。`safe`モードは全hopを名前解決しloopback/private/link-local/CGNAT等を拒否、cross-originでAuthorization/Cookieを落とす | `http_local.rs::redirects_with_embedded_credentials_never_contact_destination`、`mcp/tests/http_options.rs::http_tools_honour_upstream_arguments`（follow_redirects=safe） |
| TLS検証の無効化 | 既定で検証、無効化は明示opt-in（本家`verify`）、独自CAは`--ca-bundle` | `http/tests/tls_local.rs::tls_rejects_untrusted_cert_and_accepts_explicit_ca_bundle` |
| 巨大本文・無限再試行 | 本文上限（既定32MiB）、再試行回数の上限、Retry-Afterの上限 | `http_local.rs::limits_work_with_content_length_and_streamed_body`、`::excessive_retry_count_is_rejected_before_requests`、`spider/src/lib.rs::retry_after_seconds_and_http_date_are_bounded` |
| クロールの無制限拡大 | frontier/リンク抽出/ペイロード予算を上限化、危険な正規表現を事前拒否 | `spider/src/lib.rs::large_link_pages_do_not_grow_queue_beyond_max_pages`、`::frontier_payload_budget_is_bounded_and_preserves_state_on_failure`、`::unsafe_link_regex_is_rejected_before_fetch` |

## (c) CDP・ブラウザ状態

| 脅威 | 対策 | 回帰テスト |
|---|---|---|
| 任意の外部CDP endpointへの接続 | `ws`/`wss`のみ、URL内資格情報・制御文字を拒否、loopback以外は`RUSTSCRAPLING_CDP_ALLOWED_HOSTS`の明示allowlistが必要（D-012） | `browser/src/lib.rs::cdp_endpoint_tests::only_loopback_or_allowlisted_websocket_endpoints` |
| 利用者の通常Chromeプロファイルの使用・改変 | 自前起動は常に起動ごとの一時プロファイル（終了時削除）、`--password-store=basic`はこの使い捨てプロファイルに限る（D-010） | `browser/tests/local_chrome.rs`（opt-in） |
| 悪意ある状態ファイル（symlink、巨大、深いネスト、型不正、IndexedDB） | 状態ファイル名は単純名のみ（パス区切り不可）、状態ディレクトリはsymlink不可・0700必須、読込は通常ファイル・4MiB以下・`O_NOFOLLOW`、`deny_unknown_fields`、IndexedDBは明示拒否 | `browser/src/lib.rs::state_file_tests::malicious_state_files_are_refused`、`mcp/tests/runtime.rs`（`invalid_state_path`） |
| 状態ファイル書込で既存資格情報の上書き・symlink追従 | `create_new`（既存・symlinkに書かない）、0600、失敗時削除 | `browser/src/lib.rs::state_file_tests::state_writes_are_private_and_never_overwrite_or_follow_links` |
| ネットワーク履歴・本文・URL・ヘッダーの漏えい | 本文はXHR/Fetchテキストのみ・件数/総量上限・session-local ID、URLのquery/機密JSONキーを伏せる（ヒューリスティックで万能ではない） | `browser/src/lib.rs::credential_values_in_json_and_urls_are_redacted`、`::network_history_is_bounded_and_old_ids_are_not_reused`、`browser/tests/network_bodies_mock.rs::text_api_bodies_are_bounded_and_scoped_to_the_page_session`、`mcp/tests/network_cdp.rs::response_bodies_are_redacted_and_ids_survive_evictions`、`mcp/src/backend.rs::total_body_limit_is_enforced_without_reassigning_ids` |
| CDP応答待ちの無限待機 | 遷移・CDPコマンドに期限 | `browser/tests/navigation_timeout.rs::navigation_deadline_also_bounds_the_cdp_command_ack` |

## (d) MCPクライアント引数

| 脅威 | 対策 | 回帰テスト |
|---|---|---|
| スキーマ外の引数・境界外の数値・型不正 | 本家と同一のinputSchemaで検証（enum/型/範囲/必須）、outputSchemaも検証 | `mcp/src/lib.rs::validates_required_enums_types_and_bounds`、`mcp/src/backend.rs::numeric_network_arguments_require_exact_bounded_integers`、`mcp/tests/actions_runtime.rs::actions_are_validated_like_upstream_before_the_session_is_used` |
| 存在しないsessionでの操作・偽成功 | 実sessionが無ければtyped error、IDを捏造しない | `mcp/tests/runtime.rs::browser_session_operations_require_a_real_session` |
| 未実装機能の偽成功 | `unsupported` typed error（認証付きブラウザproxy、`additional_args`等） | `mcp/tests/browser_options.rs::browser_session_and_fetch_options_take_effect`（opt-in Chrome） |

## (e) daemon IPCとstate file

| 脅威 | 対策 | 回帰テスト |
|---|---|---|
| 未認証の呼び出し（tool/fetch/stop） | loopback bindのみ、32バイト乱数トークン、定数時間比較。誤り・空・前方一致・欠落トークンはtool hostにもfetch backendにも到達しない | `daemon/tests/ipc_security.rs::unauthenticated_malformed_oversized_and_stalled_requests_are_refused`、`daemon/tests/local_e2e.rs::localhost_authenticated_ipc_singleflight_revalidation_rate_and_shutdown` |
| 不正・巨大・深いネスト・送信途中で止まるフレーム | 16MiB上限、JSON再帰上限、要求タイムアウトで切断、daemonは継続稼働 | `daemon/tests/ipc_security.rs`（同上） |
| 改ざんされたdiscovery state（非loopback宛） | 非loopbackアドレスを拒否し、トークンを出力しない | `cli/tests/daemon_commands.rs::daemon_rejects_non_loopback_state_without_echoing_token` |
| state fileの乗っ取り・権限 | create-new（既存を上書きしない）、0600、共有ディレクトリをchmodしない | `local_e2e.rs::existing_shared_directory_is_not_chmodded` |
| キャッシュ/学習の永続化で資格情報が残る・破損行の復元 | 資格情報付きは永続化しない、期限切れ・破損行は復元しない | `local_e2e.rs::cache_and_learning_survive_restart_but_credentials_do_not`、`::expired_and_corrupted_persistent_rows_are_not_restored` |
| Spider checkpointの秘密保存・symlink | checkpointは所有者限定、symlinkを読まない、body/header付きRequestは明示opt-in | `spider/src/lib.rs::checkpoint_is_private_and_never_reads_a_state_symlink`、`::sensitive_requests_require_explicit_checkpoint_opt_in`、`::header_case_identity_and_debug_redaction` |
| CLIの一時出力（`shell view`）が他ユーザーに読まれる | 0600・create-new | `cli/tests/shell_command.rs::errors_are_typed_and_scripts_from_stdin_work` |

## (f) 第三者依存・配布物

| 脅威 | 対策 | 回帰テスト／証跡 |
|---|---|---|
| 依存のライセンス違反・不明 | `Cargo.lock`固定、CycloneDX SBOM、copyleft/ライセンス欠落でCI失敗 | `scripts/sbom.py`（CI step）、`SBOM.cdx.json` |
| 改ざんされたブラウザ配布物 | `install`は固定SHA-256・サイズ照合、staging展開、パス走査・逸脱symlink拒否、失敗時cleanup | `cli/src/install.rs::tests::extraction_rejects_traversal_and_keeps_modes`、`cli/tests/install_command.rs::pinned_checksum_mismatch_installs_nothing` |
| 移植コードの出典・ライセンス | NOTICE / `licenses/` に記録（Scrapling BSD-3、patchright Apache-2.0、markdownify MIT） | `NOTICE` |

## 既知の残余リスク（G-11をPASSにしない理由）

- 依存脆弱性スキャン（`cargo audit`等のRustSec照合）がCI未導入。
- 外部公開MCP HTTP transportは未実装のため、そのSSRF allowlist/認証の試験は対象外（実装時に追加必須）。
- ネットワーク秘匿はヒューリスティック（URL query・既知キー名）であり、アプリ固有の秘密を完全には伏せない。
- 認証付きブラウザproxyは`unsupported`（資格情報をCDP経由で扱う設計が未完）。
- Chrome サンドボックス：root実行環境ではChromeが`--no-sandbox`無しで起動しない。テスト用ラッパーのみで付与し、本番起動では付与しない（Playwright/本家は既定で`--no-sandbox`）。
- 24時間連続稼働での資源回収（G-10）は未実施。

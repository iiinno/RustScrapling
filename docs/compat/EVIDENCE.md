# EVIDENCE.md: 本家公開機能 → Rust入口 → 試験（G-01）

`docs/compat/EVIDENCE.json`から`oracle/check_evidence.py`で生成。各行のテスト関数の実在は機械検査済み。
テストの合否はCI（参照テストを全て実行）で確認する：**緑のSHAでのみ証跡**。`unmapped`は未紐付け（未検証）であり、PASSではない。

| 種別 | 総数 | verified | partial | unsupported | unmapped |
| --- | ---: | ---: | ---: | ---: | ---: |
| mcp_tool | 16 | 9 | 7 | 0 | 0 |
| action | 12 | 12 | 0 | 0 | 0 |
| cli | 10 | 7 | 3 | 0 | 0 |
| module_function | 30 | 1 | 1 | 0 | 28 |
| method | 196 | 6 | 0 | 0 | 190 |
| inherited_method | 130 | 0 | 0 | 0 | 130 |
| **合計** | 394 | 35 | 11 | 0 | 348 |

| 種別 | ID | 判定 | Rust | テスト | 備考 |
| --- | --- | --- | --- | --- | --- |
| mcp_tool | `browser_actions` | verified | BrowserPage::run_actions | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome`<br>`actions_runtime.rs::actions_are_validated_like_upstream_before_the_session_is_used`<br>`actions_runtime.rs::browser_actions_chain_on_real_chrome_if_available` |  |
| mcp_tool | `browser_evaluate` | verified | BrowserPage::evaluate_expression | `browser_options.rs::browser_session_and_fetch_options_take_effect`<br>`chrome_data_e2e.rs::chrome_data_url_dom_screenshot_and_explicit_evaluation` |  |
| mcp_tool | `browser_extract` | verified | RuntimeBackend::browser_extract (ARIA snapshot port) | `aria_snapshot_chrome_e2e.rs::aria_snapshots_match_pinned_upstream_fixtures`<br>`runtime.rs::browser_get_and_session_if_chrome_is_available` | Snapshots byte-identical to upstream fixtures (6/6). |
| mcp_tool | `browser_fetch` | partial | RuntimeBackend::browser_fetch / fetch_on_page | `browser_options.rs::browser_session_and_fetch_options_take_effect`<br>`runtime.rs::browser_get_and_session_if_chrome_is_available` | solve_cloudflare=true is 'unsupported'. |
| mcp_tool | `browser_fetch_once` | partial | RuntimeBackend::browser_once | `browser_options.rs::browser_session_and_fetch_options_take_effect` | solve_cloudflare and additional_args are 'unsupported'. |
| mcp_tool | `browser_load_state` | partial | BrowserClient::restore_state_file_for_page | `mock_cdp.rs::state_roundtrip_and_screenshots_via_mock_cdp`<br>`lib.rs::malicious_state_files_are_refused` | IndexedDB unsupported. |
| mcp_tool | `browser_network_request` | partial | RuntimeBackend::network_request | `network_cdp.rs::response_bodies_are_redacted_and_ids_survive_evictions`<br>`network_bodies_chrome_e2e.rs::real_chrome_captures_sanitized_xhr_json_body` |  |
| mcp_tool | `browser_network_requests` | partial | RuntimeBackend::network_list | `network_cdp.rs::response_bodies_are_redacted_and_ids_survive_evictions` | 128 records / 1 MiB bodies (upstream 1000 / 20 MiB); URLs redacted. |
| mcp_tool | `browser_open` | partial | RuntimeBackend::browser_open | `browser_options.rs::browser_session_and_fetch_options_take_effect`<br>`session_reuse_chrome_e2e.rs::headed_session_is_reused_across_rounds` | additional_args and authenticated proxies return 'unsupported'. |
| mcp_tool | `browser_save_state` | partial | BrowserClient::save_state_file_for_page | `mock_cdp.rs::state_roundtrip_and_screenshots_via_mock_cdp`<br>`lib.rs::state_writes_are_private_and_never_overwrite_or_follow_links` | Cookies and active-origin localStorage; IndexedDB unsupported. |
| mcp_tool | `browser_screenshot` | verified | RuntimeBackend::browser_screenshot | `mock_cdp.rs::state_roundtrip_and_screenshots_via_mock_cdp`<br>`runtime.rs::browser_get_and_session_if_chrome_is_available` |  |
| mcp_tool | `close_session` | verified | RuntimeBackend::close_session | `runtime.rs::real_http_and_http_session_round_trip`<br>`browser_options.rs::browser_session_and_fetch_options_take_effect` |  |
| mcp_tool | `list_sessions` | verified | RuntimeBackend::list_sessions | `browser_options.rs::browser_session_and_fetch_options_take_effect`<br>`mcp_command.rs::mcp_streamable_http_requires_token_and_checks_hosts` |  |
| mcp_tool | `make_request` | verified | rustscrapling-mcp RuntimeBackend::http_request | `http_options.rs::http_tools_honour_upstream_arguments`<br>`runtime.rs::real_http_and_http_session_round_trip`<br>`extract_command.rs::extract_matches_pinned_upstream_cli_outputs` | Schema identical to the live upstream tools/list (MCP_RUNTIME_SCHEMA_LIVE_V05); content via the Convertor port (240/240). |
| mcp_tool | `open_request_session` | verified | RuntimeBackend::open_http | `runtime.rs::real_http_and_http_session_round_trip`<br>`http_options.rs::http_tools_honour_upstream_arguments` |  |
| mcp_tool | `session_make_request` | verified | RuntimeBackend::http_request (session cookie jar) | `http_options.rs::http_tools_honour_upstream_arguments`<br>`runtime.rs::real_http_and_http_session_round_trip` |  |
| action | `checkbox` | verified | BrowserAction::Checkbox | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `click` | verified | BrowserAction::Click | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome`<br>`session_reuse_chrome_e2e.rs::headed_session_is_reused_across_rounds` |  |
| action | `combobox` | verified | BrowserAction::Combobox | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `dialog` | verified | BrowserAction::Dialog | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `move` | verified | BrowserAction::Move | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `press_key` | verified | BrowserAction::PressKey | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `radio` | verified | BrowserAction::Radio | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `textbox` | verified | BrowserAction::Textbox | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `wait_element` | verified | BrowserAction::WaitElement | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `wait_load` | verified | BrowserAction::WaitLoad | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `wait_time` | verified | BrowserAction::WaitTime | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| action | `wheel` | verified | BrowserAction::Wheel | `all_actions_chrome_e2e.rs::every_upstream_action_variant_runs_on_real_chrome` |  |
| cli | `CLI.delete` | verified | rustscrapling extract delete | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs` |  |
| cli | `CLI.extract` | verified | rustscrapling extract | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs`<br>`extract_command.rs::extract_rejects_unknown_extensions_and_bodies_on_get_without_writing` |  |
| cli | `CLI.fetch` | partial | rustscrapling extract fetch | `extract_command.rs::extract_fetch_renders_javascript_with_real_chrome`<br>`extract_command.rs::extract_rejects_unknown_extensions_and_bodies_on_get_without_writing` | --block-ads, --dns-over-https and --solve-cloudflare are 'unsupported'. |
| cli | `CLI.get` | verified | rustscrapling extract get | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs` |  |
| cli | `CLI.install` | partial | rustscrapling install | `install_command.rs::pinned_checksum_mismatch_installs_nothing`<br>`install_command.rs::user_pinned_version_installs_records_and_is_used`<br>`install_command.rs::dry_run_reports_plan_without_side_effects` | Chrome for Testing with SHA-256 pins instead of playwright install; no OS packages (install-deps). |
| cli | `CLI.main` | verified | rustscrapling (clap root) | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs`<br>`mcp_command.rs::mcp_subcommand_lists_the_upstream_tools` |  |
| cli | `CLI.mcp` | verified | rustscrapling mcp | `mcp_command.rs::mcp_subcommand_lists_the_upstream_tools`<br>`mcp_command.rs::mcp_streamable_http_requires_token_and_checks_hosts` |  |
| cli | `CLI.post` | verified | rustscrapling extract post | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs` |  |
| cli | `CLI.put` | verified | rustscrapling extract put | `extract_command.rs::extract_matches_pinned_upstream_cli_outputs` |  |
| cli | `CLI.shell` | partial | rustscrapling shell | `shell_command.rs::uncurl_matches_upstream_curl_parser_oracle`<br>`shell_command.rs::fetch_select_extract_and_history_loop`<br>`shell_command.rs::errors_are_typed_and_scripts_from_stdin_work` | Command language instead of IPython (no arbitrary Python evaluation). |
| module_function | `scrapling.core.shell.show_page_in_browser` | partial | shell `view` | `shell_command.rs::errors_are_typed_and_scripts_from_stdin_work` | Writes a private temp file; opens $BROWSER only when set. |
| module_function | `scrapling.spiders.throttle.parse_retry_after` | verified | rustscrapling-spider Retry-After parsing | `lib.rs::retry_after_seconds_and_http_date_are_bounded` |  |
| method | `Selector.css` | verified | rustscrapling_select::css::css | `stage1_fixtures.rs::stage1_compat_fixtures` |  |
| method | `Selector.find_by_regex` | verified | rustscrapling_select::text_search::find_by_regex | `stage2_fixtures.rs::find_results_match_oracle` |  |
| method | `Selector.find_by_text` | verified | rustscrapling_select::text_search::find_by_text | `stage2_fixtures.rs::find_results_match_oracle` |  |
| method | `Selector.find_similar` | verified | rustscrapling_adaptive::find_similar | `stage2_fixtures.rs::find_results_match_oracle`<br>`stage2_commands.rs::select_similar_relocate_and_adaptive_fallback_match_oracle` |  |
| method | `Selector.relocate` | verified | rustscrapling_adaptive::relocate | `stage2_fixtures.rs::oracle_database_relocates`<br>`stage2_commands.rs::select_similar_relocate_and_adaptive_fallback_match_oracle` |  |
| method | `Selector.xpath` | verified | rustscrapling_select::xpath::xpath | `stage1_fixtures.rs::stage1_compat_fixtures` |  |

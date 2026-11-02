# RustScrapling 引き継ぎ監査（2026-11-02）

## 基準と検証の区別

- 引き継ぎ時点の保存コミット: `e75ca8c`（Muse のローカル作業一式を保全）。
- `docs/BASELINE.md`、`docs/RESULTS.md` の数値は Muse VM 上の記録であり、PC の Windows/WSL で独立再測定した値ではない。
- Windows 11 の `cargo test --workspace --locked` は `libxml 0.3.23` がシステムの libxml2 を見つけられずビルド段階で停止した。テスト失敗の証拠ではない。
- 別途構築した WSL Ubuntu 24.04 は rustup Rust 1.99.1、`libxml2-dev` 2.9.14。Muse VM の libxml2 2.14.6 と異なるため、同じ環境・同じ日のベンチ比較には利用できない。
- WSL には古いRustも併存するため、scripts/verify.shはrustup側の実行ファイルをPATH先頭に置く。ただし、PATH統一後もこの環境でrustdocの依存解決エラーE0463が再現した。原因は未確定。通常のユニット・fixture・CLIテスト合格とdoctest合格は区別する。

## Plan.md の完了条件に対する保留事項

1. **段階0、10章の項目6:** 既存Rust移植2件のテストを実行し、SequenceMatcher/relocate の一致試験を行った記録、および stealthscraper-rs の MITM 方式採否の記録が `docs/DECISIONS.md` に見当たらない。これらを実測・記録するまで「段階0を全項目完了」と判定しない。
2. **段階1、性能:** 既定 html5ever パーサの 15 ms 以下の目標は `docs/RESULTS.md` で未達（16.5〜17.0 ms）。互換モードの成績と混同しない。
3. **段階2、正解データ:** 引き継ぎ時点では `stage2_*` の fixtures と Rust 実装があるが、`docs/RESULTS.md` に検証完了の記録がない。独立した oracle 照合と速度計測を要求する。
4. **段階3以降:** 11クレートの構成が存在することだけでは機能実装を示さない。APIと統合テストで判定する。

## 既存Rust移植の評価（2026-11-02）

本番リポジトリの外で公開ソースを取得し、固定SHAを確認した。Liohtml/RUSTScrapling (2dd9a5e)、chrisabruce/scrapling-rs (0d61c3e)、ypno/stealthscraper-rs (7615439)。

- RUSTScrapling: Ubuntu 24.04、Rust 1.99.1でcargo test --locked --lib parser::adaptiveを実行。adaptive関連6テスト合格、失敗0。autojunkとSequenceMatcherの既存テストを含む。これだけでは本家との全件同一性を証明できない。
- scrapling-rs: Cargo定義からHTTPにwreq 6.0.0-rc.28とwreq-util 3.0.0-rc.10、ブラウザにplaywright-rsを用いることを確認。テスト結果は未記録。
- stealthscraper-rs: src/proxy.rs、src/ca.rs、src/cdp/launch.rs等で、ローカルMITMとプロセス単位のCAを使う通信経路を確認。現段階では流用しない。CA管理による追加の信頼境界を正当化する性能・取得成功率が未実測のため。CDP経路の成功率が本家を下回った場合の再検討候補とする。

これらは参考評価であり、既存移植の全テスト実行や全Adaptiveケースの完全互換を満たした証拠とはしない。既存ライセンスへの依存やコードコピーをしていない。

## 検証手順

実行には libxml2 のヘッダ・ライブラリ、Rust stable、clang の環境を必要とする。コマンドは `scripts/verify.sh`。性能の判定はこのテストとは別に、同一マシン・同一日の本家との比較で行う。


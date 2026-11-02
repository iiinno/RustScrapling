# RustScrapling 統合検証

この文書は Plan.md の達成条件に対する**検証の入口**であり、完了宣言ではない。

## フォールバックと動作保証

- 本家と RustScrapling の成功率比較には、同一環境・同一日・同じ検証 URL と成功条件を使う。単一の Cloudflare ページや User-Agent の変化はステルス成功率の証拠にはならない。
- CDP のステルス経路では D-007 に従い、`Runtime.enable` の自動送信を禁止する。ブラウザ操作に必要な明示的 JavaScript 評価がある場合は別経路として記録する。
- `tests/e2e_local.py` は外部ネットワークを使わない、CLI・HTTP・Markdown・JSON の接続検査。ブロック突破やプロキシの実サイト成功率は検査しない。

## ローカル E2E

WSL Ubuntu 上で Rust stable と `libxml2-dev` を使う。

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=2
cargo build --workspace --locked
python3 tests/e2e_local.py ./target/debug/rustscrapling
```

`tests/e2e_local.py` の GET/Markdownテストは段階3のCLI取得コマンドを統合した後に有効になる。スタンドアロンの extract クレートだけが成功しても、このテストは成功しない。

## 検証結果の記載ルール

各段階について、コンパイル・単体試験・E2E・本家比較・性能の順に区別し、失敗時は機能欠落、環境障害、契約不一致を区別して記録する。計測には CPU、メモリ、試行数、ビルドモード、基準となる本家の版を添える。

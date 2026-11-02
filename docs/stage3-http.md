# 段階3: HTTP取得層の実装と検証

`rustscrapling-http` は `wreq = 0.16.3` と `wreq-util = 0.2.0` を利用する非同期HTTPクライアントである。HTTP/3は `quinn` + `h3` + `h3-quinn` による独立したQUIC経路で実装する。TLS証明書検証は両経路で有効のまま維持する。

`wreq-util 0.2.0` のクレートメタデータ記載ライセンスは **Apache-2.0**。別バージョンの `3.0.0-rc` 系のライセンスと混同しない。wreq / wreq-util のバージョンを `Cargo.toml` で固定する。依存ライセンスを配布する際には、最終バイナリ全体のNOTICEと依存ライセンス情報も確認する。

## 統合API

CLI担当が利用する例（CLI自体はこの変更では編集しない）:

```rust
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest, Protocol, ProxyRotator};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let options = HttpOptions {
    impersonate: Some("chrome_149".to_string()),
    stealthy_headers: true,
    retries: 2,
    protocol: Protocol::Http3Preferred,
    ..HttpOptions::default()
};
let client = HttpClient::new(options)?;
let response = client.request(
    HttpRequest::new("POST", "https://example.com/api")
        .header("Content-Type", "application/json")
        .body(br#"{"ok":true}"#.to_vec()),
).await?;
response.require_success()?;
let bytes = response.body;
# Ok(())
# }
```

`HttpClient::get(url)` も使用可能。`HttpResponse` は実際の最終URL、ステータス、重複を保持するヘッダーの一覧、本文バイト、実際のHTTPバージョン、試行回数、HTTP/3試行有無を返す。`require_success()` を呼ぶと403/429は `blocked`、その他の4xx/5xxは `http_status` として扱う。通常の `request()` ではHTTPステータスを保持したまま返すため、利用側で条件付き応答を扱える。

## 動作・制限

- wreqによりHTTP/1.1とHTTP/2の接続を再利用。`impersonate` はwreq-util 0.2.0のプロファイル名（`chrome_149`, `firefox_151` 等）とブラウザ別名（`chrome`, `firefox`, `safari`, `edge`, `opera`）を指定できる。認識しない値は明示的にエラーになる。`stealthy_headers = false` は該当プロファイルのTLS/HTTP2設定を保ち、プリセットヘッダーを外す。
- 明示的proxyは単一URL `proxy`、`proxies`（`http` / `https`ごとのURL）、または `ProxyRotator` のいずれかを使用する。プロキシURLのユーザー名・パスワードはBasic proxy認証に変換する。`ProxyRotator` はクライアントのclone間で共有するラウンドロビンで、各試行ごとに選択する。明示指定なしはwreqの環境変数proxy/NO_PROXY処理に委ねる。`direct = true` のとき環境変数も含めproxyを無効化する。
- `timeout`、`connect_timeout`、`read_timeout` を別々に指定できる。既定の再試行は2回、対象は接続エラー／タイムアウトおよびHTTP 408/429/5xxの一部。POST等の非冪等メソッドは既定では再試行しない。HTTPレスポンスはContent-Lengthと実ストリームの両方でサイズを監視し、既定上限は32MiB。
- `Http3Preferred` で **HTTPSかつproxyを使わない場合** にQUICを試行する。最大プローブ時間を超える／失敗した場合はwreqでHTTP/2またはHTTP/1.1へフォールバックする。HTTP/3要求であっても実通信がHTTP/2なら `http_version` に正直にHTTP/2を返す。UDPを直接送れない明示proxy・環境proxy設定下ではQUICを試行しない。HTTP/3の追加試行時間は `http3_probe_timeout` で制限する。
- CA証明書はwreqの検証ストア、QUIC側はOSの信頼ストアを利用し、`ca_bundle` または `SSL_CERT_FILE` のPEM証明書も取り込む。証明書の検証を無効化する設定はない。
- URLはHTTP/HTTPSのみ、ヘッダー名と値は送信前に検証する。自動的に実ブラウザのCookieセッションを取得する機能やJavaScript実行はこの層の対象外。

## 検証

```sh
cargo fmt -p rustscrapling-http -- --check
cargo check -p rustscrapling-http --tests
cargo test -p rustscrapling-http
```

`tests/http_local.rs` はローカルTCP HTTPサーバでGET/POST/PUT/DELETE、バイナリ、ヘッダー、認証proxy回転、リダイレクト、再試行、上限超過、タイムアウト、設定エラーを試す。`tests/http3_local.rs` は生成したローカル認証局を信頼するQUICテストサーバで、HTTP/3を**実際にネゴシエートできた場合**のバージョンを検証する。

Windows/MSVC上でBoringSSLのビルドにNASMが必要（この環境は未導入）だったため、段階3のビルドとテストはLinux/WSL環境でも実施する。Plan 1.2の本家Scraplingとのサイト別成功率比較は別途同一IP・同一日・同一条件で測り、この文書内のローカルテストだけをもって成功率の達成は主張しない。

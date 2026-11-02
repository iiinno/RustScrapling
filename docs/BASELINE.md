# BASELINE.md: 本家Scraplingの基準値（Muse VMで実測）

計測日: 2026-11-02（JST）。本家は固定コミット `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`（v0.5）。

## 計測条件

| 項目 | 値 |
|---|---|
| OS | Ubuntu 24.04.5 LTS、x86_64 |
| CPU | 4コア |
| メモリ | 7.7GiB、スワップなし。計測直後の available は約707MiB |
| Python | 3.12.3（venv: `oracle/.venv`） |
| scrapling | 0.5（`oracle/scrapling-src` を editable install、`scrapling[fetchers]` 付き） |
| 主要依存 | lxml 6.1.3（libxml2 2.14.6同梱）、cssselect 1.6.0、curl_cffi 0.17.0、patchright 1.64.3、playwright 1.64.1、tld 0.13.2、orjson 3.13.0 |
| 計測スクリプト | Plan 付録Aそのまま（`oracle/bench_appendix_a.py`）。ただし adaptive 用 SQLite の置き場所だけ `TMPDIR=/home/hatch/tmp` に変更（/tmp が512MiB のため。計測対象の処理には影響しない） |
| 計測方法 | スクリプト内蔵の best-of-5（relocate のみ n=1）を、プロセスを変えて3回実行 |

## 実測結果

合成ページは 416,497 バイト（約2000商品、約1万要素）。Plan 2.2節の「416KB」と一致する。

| 処理 | run1 | run2 | run3 | 本家PC実測（Plan 2.2） |
|---|---|---|---|---|
| raw lxml parse | 8.28 ms | 8.38 ms | 8.43 ms | （なし） |
| Scrapling Selector() parse | 8.48 ms | 8.60 ms | 8.52 ms | 17.8 ms |
| raw lxml cssselect .price text | 6.91 ms | 7.01 ms | 6.94 ms | （なし） |
| Scrapling css('.price::text').getall() | 10.34 ms | 10.58 ms | 10.44 ms | 20.6 ms |
| find_similar (2000 siblings) | 42.50 ms | 42.93 ms | 43.01 ms | 46.3 ms |
| find_by_text | 69.05 ms | 70.03 ms | 67.84 ms | 70.8 ms |
| relocate after layout change (~10k nodes) | 5305.36 ms | 5367.47 ms | 5676.61 ms | 5852.7 ms |

## 数字の読み方（Explain the Number）

- relocate は n=1 の3試行で 5305〜5677 ms、ばらつきは約7%。支配要因は Plan 2.2 のとおり全要素に対する純Python `difflib.SequenceMatcher` の反復呼び出しであり、I/Oは含まない（DB読み出しは計測外で事前に完了）。
- このVMは所有者PCより parse と css が約2倍速い。シングルスレッド性能差とみられる。比較は必ず同一環境・同一日の相対値で行う（Plan 4章）。
- RustScrapling の段階目標（Plan 10章）: parse 15 ms以下、css('.price::text') 5 ms以下（段階1）。relocate 50 ms以下、find_by_text 5 ms以下、find_similar 3 ms以下（段階2）。

## 再現手順

```bash
export TMPDIR=/home/hatch/tmp
cd ~/workspace/rustscrapling
oracle/.venv/bin/python oracle/bench_appendix_a.py oracle/scrapling-src
```

# 測試

測試分成三層：

| 測試 | 內容 | 怎麼執行 |
|---|---|---|
| 單元測試（`src/` 各檔案內） | 設定、格式、速率計算、輪替 | 建置映像檔時自動執行，沒通過就建不出映像檔 |
| 端到端測試（`tests/e2e.rs`） | 實際執行編譯出來的程式，檢查輸出、結束方式和錯誤處理 | 同上 |
| 映像檔檢查（`tests/image/e2e.py`） | 用 podman 執行映像檔，量測速率準確度，並模擬磁碟寫滿、暫停、檔案被刪除、強制終止等狀況 | 發佈映像檔之前手動執行，約 7 分鐘 |

下面的指令都在 repo 的根目錄執行。電腦上只需要 podman 和 Python，不需要安裝 Rust。

## 單元測試和端到端測試

建置映像檔的時候，這兩種測試會一起執行：

```bash
podman build -t rust-log-generator:dev .
```

只想跑測試、不建置映像檔的話，用和 `Dockerfile` 相同的 Rust 映像檔：

```bash
podman run --rm -v "$PWD":/src -w /src docker.io/library/rust:1.99.1-alpine3.24 cargo test --locked
```

兩種測試都通過時，輸出的結尾像這樣：

```text
test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.64s
```

第二個指令會在 repo 裡留下一個 `target` 目錄，放的是編譯的中間產物。它已經列在 `.gitignore` 裡，可以直接刪除。

端到端測試會實際啟動程式、寫入暫存目錄，再送出訊號讓它結束。每個測試都會等程式印出第一行才開始計時，所以不受機器速度影響。其中一個測試需要 `mkfifo` 指令來建立 FIFO，系統上沒有的話會自動略過。寫入失敗（磁碟寫滿）由下面的映像檔檢查負責。

## 映像檔檢查

對一個建好的映像檔執行：

```bash
python3 tests/image/e2e.py rust-log-generator:dev
```

映像檔的名稱可以是本機的 tag，也可以是 registry 上的位址。執行前要準備：

- rootless 的 podman，4.3 版以上。
- Python 3.11 以上，不需要另外安裝套件。
- 一台沒有在忙的機器。速率的檢查會受其他工作影響。
- 暫存目錄要有幾百 MiB 的空間，檢查結束後會自動清掉。
- 能從 `docker.io` 下載 `busybox` 映像檔，磁碟寫滿的檢查會用到。

檢查分成七組，可以只跑其中幾組，把組名接在映像檔名稱後面即可：

```bash
python3 tests/image/e2e.py rust-log-generator:dev config stop
```

| 組名 | 檢查什麼 | 大約時間 |
|---|---|---|
| `config` | 預設值、各種不合法的設定值、磁碟用量上限 | 15 秒 |
| `rate` | 設定 0.001、1、100、1000 MiB/s 時實際寫了多少，以及回報的間隔 | 3 分鐘 |
| `content` | JSON 和純文字兩種格式的每一行 | 30 秒 |
| `rotate` | 目錄自動建立、接著既有的檔案寫、輪替後的檔案數、啟動時的清理 | 40 秒 |
| `stop` | 收到 SIGTERM 和 SIGINT 後正常結束，磁碟上的資料和回報的總量一致 | 10 秒 |
| `leak` | 以 200 MiB/s 連續輪替 50 秒（約一萬次）之後，記憶體和開啟的檔案數沒有增加 | 1 分鐘 |
| `faults` | 唯讀目錄、暫停後恢復、檔案被刪除或換掉、強制終止、磁碟寫滿 | 1.5 分鐘 |

每一項檢查印出一行，開頭是 `[PASS]` 或 `[FAIL]`。`[INFO]` 是單純的觀察記錄，不算通過也不算失敗。最後一行是總結：

```text
== rust-log-generator:dev: 29 checks, 0 failed, 385 s
```

有任何一項失敗時，指令的 exit code 是 1。

log 的內容由 `tests/image/check_logs.py` 判斷。它和程式本身沒有共用任何程式碼，只用 Python 內建的 JSON 和日期解析器，逐行檢查長度、格式、時間、`level` 和 `seq` 是否連續。這樣檢查的程式和被檢查的程式不會犯同一個錯。

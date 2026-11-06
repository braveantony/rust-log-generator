# rust-log-generator

rust-log-generator 是用 Rust 寫的 log 產生器，會依照你設定的速率（每秒幾 MiB，1 MiB 是 1024 × 1024 bytes）持續把 log 寫進檔案，格式可以選 JSON 或純文字。

適合用來測試需要穩定 log 流量的情境，例如 log 收集管線的處理量，或儲存空間的寫入效能。

這個專案是和 Claude Code 一問一答開發出來的，開發過程記錄在 [docs/user-questions.md](docs/user-questions.md)。

## Key Features

- **自訂寫入速率**：設定每秒要寫幾 MiB，程式就穩定照這個速率輸出。
- **JSON 或純文字**：改一個設定就能切換 log 格式。
- **磁碟用量有上限**：log 檔寫滿就輪替，只保留最近幾個檔案。
- **檢查得出掉行**：每一行都有編號，接收端可以確認 log 有沒有漏掉。
- **一行指令部署到 Kubernetes**：附上現成的設定檔，符合 Kubernetes 內建最嚴格的安全等級。

## Quick Start

### 部署到 Kubernetes

先準備好：

- 執行指令的電腦上要有 `kubectl` 和 `git`。`kubectl` 會用 `git` 把這個 repo 抓下來。
- `kubectl` 已經連上叢集，而且有權限建立 namespace（叢集裡用來分隔資源的空間）。執行 `kubectl get nodes` 列得出節點（叢集裡的機器），就表示已經連上。
- 叢集要有預設的 StorageClass（動態配置儲存空間的設定）。用 `kubectl get storageclass` 查，名稱後面有 `(default)` 的就是。
- 節點要能連到 `registry.gitlab.com` 下載映像檔。映像檔是公開的，不需要帳號密碼。

已在 Kubernetes v1.37 測試過。

一行指令部署 0.1.2 版：

```bash
kubectl apply -k 'https://gitlab.com/kubeantony/rust-log-generator.git//k8s/overlay/prod?ref=v0.1.2'
```

這會建立 `rust-log-generator` 這個 namespace，以及裡面存放設定值的 ConfigMap、1 GiB 的 PersistentVolumeClaim（PVC，向叢集申請的一塊儲存空間）、Deployment（負責建立 Pod 並讓它持續執行），以及禁止所有網路連線的 NetworkPolicy。Pod（Kubernetes 執行 container 的最小單位）啟動後以 1 MiB/s 寫入 JSON log，檔案最多占 500 MiB。

確認已經在執行：

```bash
kubectl -n rust-log-generator rollout status deployment/rust-log-generator
kubectl -n rust-log-generator logs deployment/rust-log-generator | head -n 3
```

程式啟動時會在 stdout（程式輸出一般訊息的管道）印出目前的設定，之後大約每秒回報一次實際的寫入速率：

```text
rust-log-generator: run 604f8f26: writing 1 MiB/s of JSON logs to /logs/app.log (rotate at 100 MiB, keep 5 files)
rust-log-generator: 1.00 MiB/s (total 1.0 MiB)
rust-log-generator: 1.00 MiB/s (total 2.1 MiB)
```

產生的 log 寫在 PVC 裡的 `/logs/app.log`，不是 stdout。

修改設定、查看 log 檔的內容和移除部署的步驟，都在 [docs/install.md](docs/install.md)。

### 用 podman 在本機執行

不需要 Kubernetes 也能執行。下面的指令以 5 MiB/s 寫入純文字 log，檔案存在名為 `rust-log-generator-logs` 的 volume（podman 管理的儲存空間）裡。這個 volume 不存在時，podman 會自動建立。按 Ctrl+C 停止：

```bash
podman run --rm -e LOG_RATE_MIB=5 -e LOG_JSON=false -v rust-log-generator-logs:/logs \
  registry.gitlab.com/kubeantony/rust-log-generator:0.1.2
```

停止後 log 檔還留在 volume 裡，不需要時用 `podman volume rm rust-log-generator-logs` 刪除。

## Configuration

所有設定都用環境變數，沒設定的就用預設值。設定值不合法時，程式會印出原因並結束（exit code 1）。訊息印在 stderr（程式輸出錯誤訊息的管道），在 Kubernetes 上用 `kubectl logs` 就看得到。

| 環境變數 | 預設值 | 說明 |
|---|---|---|
| `LOG_RATE_MIB` | `1` | 每秒寫入幾 MiB，可以用小數。最小是每秒 1 byte（約 `0.00000096`），再小就寫不出完整的一行 |
| `LOG_JSON` | `true` | `true` 輸出 JSON，`false` 輸出純文字，不分大小寫 |
| `LOG_DIR` | `/logs` | log 檔所在的目錄，不存在時會自動建立 |
| `LOG_MAX_FILE_MIB` | `100` | 單一檔案的大小上限（MiB），正整數 |
| `LOG_MAX_FILES` | `5` | 最多保留幾個檔案，包含正在寫入的那一個，正整數 |
| `LOG_MAX_TOTAL_MIB` | `1024` | `LOG_MAX_FILE_MIB` × `LOG_MAX_FILES` 的上限（MiB），正整數。超過時程式會拒絕啟動 |

在 Kubernetes 上怎麼改這些設定，見 [docs/install.md](docs/install.md)。

## How It Works

```mermaid
flowchart LR
    env["環境變數"] --> app["rust-log-generator"]
    app -- "每 100 ms 寫一批" --> active["/logs/app.log"]
    active -- "寫滿就輪替" --> rotated["app.log.1、app.log.2 …"]
    app -- "大約每秒回報速率" --> stdout["stdout"]
```

程式只有一個執行緒，啟動後不斷重複控制速率、產生 log、輪替檔案這三件事，直到收到 SIGTERM 或 SIGINT（Kubernetes 停止 Pod 或按下 Ctrl+C 時送出的訊號）才結束。

**控制速率。** 程式每 100 ms 醒來一次，算出「從啟動到現在總共應該寫多少 bytes」，再補寫到這個數字為止。因為比對的是累計值而不是每一輪的量，某一輪寫多或寫少都會在下一輪修正，長時間的平均速率不會偏移。如果落後太多（例如磁碟卡住），最多只補 1 秒份的量，超出的部分直接放棄，不會事後一口氣補寫。

**產生 log。** 每一行固定 256 bytes（含換行），所以 1 MiB 剛好是 4,096 行。不夠的長度用重複的填充文字補滿，下面的範例省略了後段的填充文字。JSON 格式：

```json
{"ts":"2026-11-05T12:21:20.693Z","level":"INFO","run":"5d8280fd","seq":0,"msg":"the quick brown fox jumps over the lazy dog the quick …"}
```

純文字格式：

```text
2026-11-05T12:21:36.415Z INFO run=eb17a6fb seq=0 the quick brown fox jumps over the lazy dog the quick …
```

| 欄位 | 內容 |
|---|---|
| `ts` | UTC 時間，精確到毫秒 |
| `level` | 每 10 行有 7 行 `INFO`，`DEBUG`、`WARN`、`ERROR` 各 1 行 |
| `run` | 每次啟動隨機產生的 8 位十六進位代號 |
| `seq` | 行號，每次啟動從 0 開始，每行加 1 |
| `msg` | 填充文字，用來把每一行補到 256 bytes |

純文字格式的欄位順序相同，只有 `run` 和 `seq` 帶有欄位名稱。

程式重新啟動後 `seq` 會從 0 重新算，所以要用 `run` 加 `seq` 才能唯一識別一行。接收 log 的那一端可以檢查 `seq` 有沒有連續，判斷中間有沒有掉行。

**輪替檔案。** 下一行會讓 `app.log` 超過 `LOG_MAX_FILE_MIB` 時，`app.log` 改名為 `app.log.1`，原本的 `app.log.1` 變成 `app.log.2`，依此類推，最舊的那個檔案被刪除。連同 `app.log` 在內最多保留 `LOG_MAX_FILES` 個檔案，所以編號最大是 `LOG_MAX_FILES` − 1。以預設值來說，就是 `app.log` 到 `app.log.4` 這 5 個檔案、每個 100 MiB，最多占 500 MiB。

執行中如果 `app.log` 被刪除，或被換成另一個檔案，程式會在下一輪（100 ms 內）發現，改寫到路徑上現有的 `app.log`；檔案或目錄不存在就重新建立。如果路徑上的東西打不開（例如沒有寫入權限），程式會繼續寫原本的檔案，等下一次輪替再換新檔。

**結束。** 收到 SIGTERM 或 SIGINT 後，程式把還在記憶體裡的資料寫進檔案，然後正常結束（exit code 0）。

Kubernetes 設定檔的結構和加上的安全設定，見 [docs/install.md](docs/install.md)。

測試分成哪幾層、各自怎麼執行，見 [tests/README.md](tests/README.md)。

## Known Limitations

- **同一個目錄只能有一個程式在寫。** 程式不會鎖定檔案。兩個程式寫同一個 `LOG_DIR` 時，行號會錯亂，檔案也會被對方輪替掉。Kubernetes 的設定用 `Recreate` 策略避開這種情況：舊的 Pod 結束之後，新的才會啟動。
- **被強制終止或斷電時會掉資料。** 程式每 100 ms 把資料交給作業系統，但不會要求立刻寫進磁碟（沒有呼叫 fsync）。process 被強制終止時，例如收到 SIGKILL，還在程式緩衝區裡的資料會消失，最多 256 KiB，也就是 1,024 行；已經在檔案裡的仍然是完整的行。主機斷電或當機時，作業系統還沒寫進磁碟的部分也會遺失。
- **既有的檔案不會被縮小。** 沿用舊的 volume 並調低 `LOG_MAX_FILE_MIB` 時，原本比新上限大的 `app.log` 會原封不動變成 `app.log.1`，直到被輪替掉為止。這段期間磁碟用量可能超過設定的上限。
- **`ts` 取自系統時鐘。** 主機校時的時候，後面的行的時間可能比前面的早，或突然往前跳。速率控制用的是不受校時影響的時鐘，所以寫入速率不會跟著變。
- **`LOG_DIR` 不要指到別人也能寫的目錄。** 程式開啟 `app.log` 時不會檢查它是不是符號連結（指向另一個檔案的捷徑）。能寫這個目錄的人可以把 `app.log` 換成連結，讓 log 寫進別的檔案。Kubernetes 的設定裡，volume 只有這個 Pod 在用，不會有這個問題。
- **映像檔只提供 `linux/amd64`。**

## License

以 [Apache License 2.0](LICENSE) 授權。

# 安裝與設定

這份文件說明怎麼把 rust-log-generator 部署到 Kubernetes、修改設定後重新套用、查看產生的 log，以及移除部署，最後說明設定檔的結構和加上的安全設定。

只想用預設值快速試用的話，看 [README 的 Quick Start](../README.md#quick-start) 就夠了。

所有指令都在你自己的電腦上執行，也就是裝有 `kubectl` 的那一台，不需要登入叢集的節點。文件裡的步驟和輸出都在 Kubernetes v1.37 上實際執行過。

## 事前準備

- 電腦上要有 `kubectl` 和 `git`。
- `kubectl` 已經連上叢集，而且有權限建立 namespace（叢集裡用來分隔資源的空間）。執行 `kubectl get nodes` 列得出節點（叢集裡的機器），就表示已經連上。
- 叢集要有預設的 StorageClass（動態配置儲存空間的設定）。用 `kubectl get storageclass` 查，名稱後面有 `(default)` 的就是。
- 節點要能連到 `registry.gitlab.com` 下載映像檔。映像檔是公開的，不需要帳號密碼。

## 步驟 1：取得設定檔

把 0.1.3 版的 repo 抓下來：

```bash
git clone --branch v0.1.3 https://gitlab.com/kubeantony/rust-log-generator.git
cd rust-log-generator
```

`--branch v0.1.3` 指定固定的版本。`git` 會印出一段關於 detached HEAD 的提示，這是正常的。

之後的指令都在 `rust-log-generator` 這個目錄裡執行。會用到的設定檔有這些：

| 檔案 | 內容 |
|---|---|
| `k8s/base/kustomization.yaml` | 寫入速率、格式、單一檔案的大小、保留幾個檔案 |
| `k8s/base/deployment.yaml` | 映像檔，以及磁碟用量的上限 `LOG_MAX_TOTAL_MIB` |
| `k8s/base/pvc.yaml` | PersistentVolumeClaim（PVC，向叢集申請的一塊儲存空間）的大小 |
| `k8s/overlay/prod/` | namespace、安全設定和 NetworkPolicy，一般不需要修改 |

## 步驟 2：修改設定

打開 `k8s/base/kustomization.yaml`，設定值在 `literals` 底下：

```yaml
configMapGenerator:
  - name: rust-log-generator-config
    literals:
      - LOG_RATE_MIB=1
      - LOG_JSON=true
      - LOG_MAX_FILE_MIB=100
      - LOG_MAX_FILES=5
```

| 設定 | 意思 |
|---|---|
| `LOG_RATE_MIB` | 每秒寫入幾 MiB，可以用小數 |
| `LOG_JSON` | `true` 輸出 JSON，`false` 輸出純文字 |
| `LOG_MAX_FILE_MIB` | 單一檔案寫到幾 MiB 就輪替 |
| `LOG_MAX_FILES` | 最多保留幾個檔案，包含正在寫入的那一個 |

每個設定的完整說明在 [README 的 Configuration](../README.md#configuration)。

舉例來說，要改成每秒 5 MiB、純文字格式，就把前兩行改成：

```yaml
      - LOG_RATE_MIB=5
      - LOG_JSON=false
```

有一條限制：`LOG_MAX_FILE_MIB` × `LOG_MAX_FILES` 不能超過 900。這是 log 檔可以占用的磁碟空間上限，略小於 1 GiB 的 PVC。需要更大的話，見後面的「需要超過 900 MiB 的時候」。

## 步驟 3：套用前先確認

先看 Kustomize（`kubectl` 內建的設定檔組合工具）實際會產生的設定值：

```bash
kubectl kustomize k8s/overlay/prod | grep -A5 '^data:'
```

```text
data:
  LOG_JSON: "false"
  LOG_MAX_FILE_MIB: "100"
  LOG_MAX_FILES: "5"
  LOG_RATE_MIB: "5"
kind: ConfigMap
```

如果叢集上已經有舊的部署，可以再用 `kubectl diff` 看這次會改到什麼。前面是 `-` 的行是現在的內容，`+` 是套用後的內容：

```bash
kubectl diff -k k8s/overlay/prod
```

這兩個指令都只是查看，不會改動叢集。

## 步驟 4：套用

```bash
kubectl apply -k k8s/overlay/prod
```

第一次部署會建立五樣東西：

```text
namespace/rust-log-generator created
configmap/rust-log-generator-config-btm8mggk74 created
persistentvolumeclaim/rust-log-generator-logs created
deployment.apps/rust-log-generator created
networkpolicy.networking.k8s.io/deny-all created
```

已經部署過、這次只改設定值的話，會看到新的 ConfigMap 和更新過的 Deployment，其他不變：

```text
namespace/rust-log-generator unchanged
configmap/rust-log-generator-config-5bck52876k created
persistentvolumeclaim/rust-log-generator-logs unchanged
deployment.apps/rust-log-generator configured
networkpolicy.networking.k8s.io/deny-all unchanged
```

改設定值之後會發生這些事：

- ConfigMap 的名稱結尾是設定內容算出來的代碼，所以設定一改，名稱就跟著變，Pod（Kubernetes 執行 container 的最小單位）也會跟著重建。
- 舊的 Pod 先結束，新的才啟動，中間會有幾秒沒有在寫 log。
- PVC 和裡面既有的 log 檔會保留，新的 Pod 接著寫同一個 `app.log`。`run` 會換成新的代號，`seq` 從 0 重新開始。
- 如果改了格式，同一個 `app.log` 裡前面會是舊格式、後面是新格式。
- 舊的 ConfigMap 會留在 namespace 裡，不影響運作，移除部署時會一起刪掉。

## 步驟 5：確認結果

等部署完成，再看 Pod 的狀態：

```bash
kubectl -n rust-log-generator rollout status deployment/rust-log-generator
kubectl -n rust-log-generator get pods
```

`STATUS` 是 `Running`、`RESTARTS` 是 `0` 就表示正常。

再看程式印出的設定和寫入速率：

```bash
kubectl -n rust-log-generator logs deployment/rust-log-generator | head -n 3
```

```text
rust-log-generator: run 132a33fb: writing 5 MiB/s of text logs to /logs/app.log (rotate at 100 MiB, keep 5 files)
rust-log-generator: 4.99 MiB/s (total 5.0 MiB)
rust-log-generator: 5.00 MiB/s (total 10.0 MiB)
```

第一行是程式實際採用的設定，之後大約每秒回報一次實際的寫入速率。

### 設定值不合法的時候

設定值不合法時，程式一啟動就會結束，Pod 的 `STATUS` 會變成 `Error` 或 `CrashLoopBackOff`。這種情況下 `rollout status` 有可能先回報成功，所以一定要用 `kubectl get pods` 再確認一次。

原因用 `kubectl logs` 看。例如把 `LOG_MAX_FILE_MIB` 設成 300 而超過上限時：

```bash
kubectl -n rust-log-generator logs deployment/rust-log-generator
```

```text
rust-log-generator: LOG_MAX_FILE_MIB (300) x LOG_MAX_FILES (5) exceeds LOG_MAX_TOTAL_MIB (900), the cap on disk space used by log files
```

把設定值改回合法的範圍，重新執行步驟 4 就會恢復。

## 查看 log 檔的內容

產生的 log 寫在 PVC 裡的 `/logs/app.log`，不是 `kubectl logs` 看到的那些。映像檔裡只有程式本身，沒有 shell 也沒有 `cat` 這類工具，所以不能用 `kubectl exec` 進去看。

要看檔案內容，可以用 `kubectl debug` 在 Pod 裡臨時加一個帶有工具的 container：

```bash
POD=$(kubectl -n rust-log-generator get pod -l app.kubernetes.io/name=rust-log-generator -o name)
kubectl -n rust-log-generator debug "$POD" -i --image=docker.io/library/busybox:1.37 \
  --target=rust-log-generator --profile=restricted -- tail -n 2 /proc/1/root/logs/app.log
```

這會先印出幾行 `kubectl` 的提示，接著是 `app.log` 的最後兩行。有幾件事要知道：

- `/proc/1/root` 是從臨時 container 看到的程式檔案系統，所以 log 檔的路徑是 `/proc/1/root/logs/`。
- 節點要能從 `docker.io` 下載 `busybox` 映像檔。
- 每執行一次，Pod 裡就會留下一個已結束的臨時 container，Pod 重建後才會消失。

## 需要超過 900 MiB 的時候

預設的 PVC 是 1 GiB，所以 Deployment 把 `LOG_MAX_TOTAL_MIB` 設為 900，略小於 PVC。不是每種儲存空間都會強制執行 PVC 的容量限制，所以由程式自己把關，超過時 Pod 會啟動失敗。

要保留更多 log，三個地方要一起改。下面以 2 GiB 的 PVC、每個檔案 300 MiB、保留 5 個（共 1500 MiB）為例：

1. `k8s/base/pvc.yaml`：把 `storage: 1Gi` 改成 `storage: 2Gi`。
2. `k8s/base/deployment.yaml`：把 `LOG_MAX_TOTAL_MIB` 的 `value: "900"` 改成 `value: "1900"`。這個值要比 PVC 小一些。
3. `k8s/base/kustomization.yaml`：把 `LOG_MAX_FILE_MIB=100` 改成 `LOG_MAX_FILE_MIB=300`。

還沒部署過的話，直接執行步驟 4 就可以。

已經部署過的話，要先確認 PVC 能不能放大：

```bash
kubectl get storageclass
```

`ALLOWVOLUMEEXPANSION` 那一欄是 `true`，才能直接套用。是 `false` 的話，`kubectl apply` 會被拒絕，訊息的結尾像這樣：

```text
persistentvolumeclaims "rust-log-generator-logs" is forbidden: only dynamically provisioned pvc can be resized and the storageclass that provisions the pvc must support resize
```

這時只能先照下面的方式移除，再重新部署。原本的 log 檔會消失。

## 設定檔的結構和安全設定

設定檔用 Kustomize 分成兩層：

| 目錄 | 內容 |
|---|---|
| `k8s/base` | Deployment（負責建立 Pod 並讓它持續執行）、PVC，以及存放設定值的 ConfigMap |
| `k8s/overlay/prod` | 在 base 之上加入 namespace、Pod Security Standards `restricted`（Kubernetes 內建最嚴格的 Pod 安全等級）要求的安全設定、唯讀的根檔案系統，以及禁止所有網路連線的 NetworkPolicy |

`k8s/overlay/prod` 加上的安全設定：

- namespace 啟用 Pod Security Standards `restricted`，不符合的 Pod 會被拒絕。
- container 以非 root 身分執行（UID 65532）、不能提升權限、移除所有 Linux capabilities（從 root 權限拆出來的各項特權）、啟用預設的 seccomp 設定（限制 process 能用的系統呼叫）。
- 根檔案系統唯讀，只有 `/logs` 可以寫入。
- NetworkPolicy 禁止 namespace 裡所有 Pod 的對內和對外連線，包含 DNS。程式只寫檔案，不需要網路。叢集的網路外掛（CNI）要支援 NetworkPolicy，這項限制才會生效。

另外，映像檔用 digest（內容的雜湊值）指定，不是用 tag，所以部署的一定是同一份映像檔。

## 移除

```bash
kubectl delete -k k8s/overlay/prod
```

這會連同 namespace 和 PVC 一起刪除。大多數 StorageClass 會在 PVC 刪除時清掉資料，所以寫好的 log 檔也會消失。

沒有把 repo 抓下來的話，也可以直接用網址移除：

```bash
kubectl delete -k 'https://gitlab.com/kubeantony/rust-log-generator.git//k8s/overlay/prod?ref=v0.1.3'
```

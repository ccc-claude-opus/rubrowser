# 上傳與下載

rubrowser 依 Web 標準支援選檔、上傳（`multipart/form-data`）與下載，網頁不需要為它特別處理：照一般瀏覽器的寫法即可。

```mermaid
flowchart LR
  U["使用者"] -->|"點 &lt;input type=file&gt;"| D["原生選檔對話框<br/>(rfd)"]
  D -->|"位元組"| F["FormState<br/>(每個 input 的檔案)"]
  F -->|"files / file_data"| J["JS：FileList、File、FileReader"]
  F -->|"表單送出"| M["multipart/form-data"]
  J -->|"FormData → fetch / XHR"| M
  M --> S["伺服器"]
  S -->|"Content-Disposition: attachment<br/>或無法顯示的型別"| DL["下載資料夾"]
  J -->|"&lt;a download&gt; + blob: 網址"| DL
```

## 1. 選檔：`<input type=file>`

- 點 file input（或它的 `<label>`、腳本呼叫 `input.click()` / `input.showPicker()`）會開作業系統的選檔對話框：macOS 是 NSOpenPanel，Linux 經由 XDG desktop portal（D-Bus）。實作用 [rfd](https://crates.io/crates/rfd)（MIT）。
- `multiple`：可以選多個檔案；沒有 `multiple` 時只保留第一個。
- `accept`：`.png`、`image/*`、`text/csv` 等轉成對話框的副檔名篩選；有不認得的 MIME type 時不篩選（`accept` 只是提示）。
- 選好後檔案內容讀進記憶體，input 顯示檔名（多個時顯示「N files」），依序觸發 `input` 與 `change`（會冒泡）；使用者取消時觸發 `cancel`。
- **沒有視窗時**（`--screenshot`、測試）不開對話框，input 保持空的，也不觸發事件。
- `input.value` 是 `C:\fakepath\檔名`（HTML 標準）；腳本只能把它設成空字串來清除選擇。表單 reset 也會清除。

## 2. JavaScript API（File API）

| API | 說明 |
|---|---|
| `input.files` | `FileList`；同一次選擇回傳同一個物件 |
| `Blob` | 建構子接受字串（UTF-8）、`ArrayBuffer`、typed array、`Blob`；`size`、`type`、`slice(start, end, type)`、`text()`、`arrayBuffer()`、`bytes()`、`stream()` |
| `File` | `Blob` 加上 `name`、`lastModified`、`webkitRelativePath` |
| `FileReader` | `readAsText(blob, encoding)`（UTF-8，及 latin1 / windows-1252）、`readAsDataURL`、`readAsArrayBuffer`、`readAsBinaryString`、`abort()`；事件 `loadstart`、`progress`、`load`、`loadend`、`abort` |
| `URL.createObjectURL(blob)` | 回傳 `blob:<origin>/<uuid>`；內容同時登記在瀏覽器，所以 `<img src>`、`<a href>`、`<a download>`、`fetch` 都能用 |
| `URL.revokeObjectURL(url)` | 撤銷；之後 `fetch` 失敗、圖片載入失敗 |
| `FormData` | `new FormData(form, submitter)` 依 HTML 的 entry list 規則（略過未勾選、停用與未按下的按鈕；file input 每個檔案一筆，沒選檔時是一個空的 `File`）；`append(name, blob, filename)`、`set`、`get`、`getAll`、`has`、`delete`、`entries`、`keys`、`values`、`forEach` |
| `TextEncoder` / `TextDecoder` | 由 runtime 以 Rust 編解碼 UTF-8（快，且不合法的位元組換成 U+FFFD） |

檔案與 Blob 的內容是 runtime 記憶體裡的 `Uint8Array`；每頁 JS 記憶體上限 512 MiB，非常大的檔案會受限於此。

## 3. 上傳：`multipart/form-data`

### 表單送出

`<form method=post enctype="multipart/form-data">` 依 HTML 標準的 form submission 演算法（[RFC 7578](https://www.rfc-editor.org/rfc/rfc7578)）編碼：

```text
--<boundary>
Content-Disposition: form-data; name="title"

文字（換行統一為 CRLF）
--<boundary>
Content-Disposition: form-data; name="pic"; filename="cat.png"
Content-Type: image/png

<檔案位元組>
--<boundary>--
```

- 欄位名稱與檔名裡的 `"`、CR、LF 換成 `%22`、`%0D`、`%0A`；檔案的 `Content-Type` 依副檔名，未知時為 `application/octet-stream`。
- boundary 是隨機的，並確認不出現在內容裡。
- 也支援 `enctype="text/plain"`；其他值（含預設）為 `application/x-www-form-urlencoded`，此時 file input 送出檔名。
- 送出按鈕的 `formaction`、`formmethod`、`formenctype` 優先於表單的屬性；`form.submit()` 用同樣的演算法（不觸發 `submit` 事件）。

### fetch / XMLHttpRequest

body 依 Fetch 標準的「extract a body」：`FormData` 以 `multipart/form-data; boundary=…` 送出（JS 端組成位元組），`Blob` 用它的 `type`，`URLSearchParams` 為 urlencoded，字串為 `text/plain;charset=UTF-8`，`ArrayBuffer` / typed array 原樣送出。腳本自己設的 `Content-Type` 優先。runtime 與瀏覽器之間以位元組傳遞 body 與回應，二進位內容不會被改動。

## 4. 下載

以下情況把回應存到下載資料夾，而不是顯示：

| 情況 | 檔名 |
|---|---|
| 換頁的回應有 `Content-Disposition: attachment`（RFC 6266） | `filename*`（RFC 5987，UTF-8）優先，其次 `filename`，再其次網址最後一段 |
| 換頁的回應是無法顯示的 MIME type（例如 `application/zip`、`application/pdf`、`application/octet-stream`） | 同上 |
| 點 `<a download="名稱">`（同源、`blob:` 或 `data:` 網址；跨來源的連結照常換頁，同 HTML 標準） | 伺服器的 `Content-Disposition` 優先，其次 `download` 屬性，再其次網址 |

- 換頁變成下載時，目前的頁面留著，歷史紀錄不增加。
- 下載資料夾：環境變數 `RUBROWSER_DOWNLOAD_DIR`；否則 Linux 為 `XDG_DOWNLOAD_DIR` 或 `~/.config/user-dirs.dirs` 的設定；否則 `~/Downloads`。資料夾不存在時會建立。
- 檔名去掉路徑、控制字元與檔案系統不接受的字元，不以 `.` 開頭；同名時存成 `名稱 (1).ext`，不覆蓋既有檔案。
- 視窗下方的下載列顯示「Downloaded 名稱 to 路徑」（失敗時顯示原因），按 ✕ 關閉；訊息也寫到主控台（stderr）。

## 5. 測試

- 單元測試：`cargo test`（`forms.rs` 的 multipart 編碼、`net` 的 `Content-Disposition` 解析與檔名、`browser` 的選檔、表單送出、fetch / XHR 的 multipart、`blob:` 網址與下載）。
- 端對端：

  ```bash
  cargo build && scripts/file-transfer-test.py
  ```

  它啟動本機 HTTP 伺服器，用 `rubrowser --screenshot` 開它的頁面：fetch 上傳 `FormData`（非 ASCII 檔名、含 CR/LF/NUL 的檔案、0–255 全部位元組的 Blob）、`form.submit()` 送出 multipart 表單（沒有視窗，所以 file input 是空的），再下載 `blob:` 網址、`<a download>` 與 `Content-Disposition: attachment` 的檔案；伺服器逐一檢查收到的內容，腳本檢查下載資料夾裡的檔案。

## 6. 相依套件

| 套件 | 授權 | 用途 |
|---|---|---|
| [rfd](https://github.com/PolyMeilex/rfd) 0.18 | MIT | 原生選檔對話框（只在 `native` feature） |

Linux 上 rfd 經由 XDG desktop portal 開對話框，需要桌面環境提供 portal（`xdg-desktop-portal` 與 GTK / KDE 後端）；沒有時對話框無法開啟，input 保持空的。

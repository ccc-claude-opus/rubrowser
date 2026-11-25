依這段時間看過的程式碼，加上剛才實際查證，我把 rubrowser 的不足依影響程度排列如下。第一組我已在原始碼裡確認，而且會直接讓 ruwebos 的應用出錯。

## 1. 會直接造成錯誤（建議優先修）

- **非 GET 的請求一律以 POST 送出。**`platform/native.rs` 的 `http()` 只要方法不是 GET 就呼叫 `.post()`，所以 `fetch` 的 PUT、DELETE、PATCH 都會變成 POST。用 REST 介面的應用（例如通訊錄的刪除）會出錯，HEAD 也一樣。
- **腳本設的請求標頭沒有送出。**`HttpCall` 只帶 `Content-Type`，`Authorization`、`X-Requested-With` 和自訂標頭都被丟掉。需要 token 驗證的 API 無法使用。
- **`localStorage` 只存在於這一頁。**`prelude.js` 的註解寫著 "nothing is written to disk"：重新載入或換頁就全部消失，`sessionStorage` 也不跟分頁走。應用的設定和草稿都存不住。
- **兩套 cookie 互不相通。**
  - `document.cookie` 是 JS 裡自己的一串字串，伺服器 `Set-Cookie` 設的 cookie 由 ureq 管理，兩邊看不到彼此。
  - ureq 的 cookie 也沒寫到磁碟，關掉瀏覽器登入狀態就沒了。
  - `navigator.cookieEnabled` 回傳 false，有些網站會因此拒絕登入。
- **`MutationObserver` 什麼都不做。**它的 `observe()` 是空的，依賴它的框架和元件（不少 UI 函式庫、編輯器）不會更新。`IntersectionObserver` 把所有元素都當成可見，`ResizeObserver` 也不會通知。

## 2. 標準實作不完整

- **CORS**：腳本只能存取同源網址。要呼叫別的服務（第三方 API、CDN 上的 JSON）就做不到。
- **Web Worker / SharedWorker**：物件存在，但不會執行。
- **Shadow DOM 不繪製**：使用 Web Components 的網站會顯示不完整。
- **`load` 事件太早**：它在腳本跑完後就觸發，不等圖片和字型載入。CDP 的 `loadEventFired` 也因此偏早。
- **換行沒有轉成 CRLF**：urlencoded 表單裡的換行留成 LF，標準要求轉成 CRLF。上一輪我只修了 multipart 和 text/plain，為了不動既有測試沒改這裡。
- **沒有 HTTP 快取**：沒有處理 `Cache-Control`、`ETag`，每次重新載入都重抓全部資源。
- **IME 不支援**（README 已註明）：無法用注音、倉頡等輸入法直接打中文，對繁中使用者影響很大。

## 3. 效能與資源

- **JS 速度**：boa 是直譯器，再包在 wasm 裡執行，大型框架（React、大量 DOM 操作）會明顯變慢。Performance 面板現在可以量出瓶頸，但根本的速度差距仍在。
- **整頁重新計算樣式**：DOM 一有變動就常常要算整頁樣式。這是我在 devtools 計時時觀察到的，例如 `update 106 ms`；不過那是 debug build，數字只能參考。
- **檔案整個讀進記憶體**：選檔、上傳、下載都把整個檔案放進記憶體，回應大小上限 32 MB，JS 記憶體上限 512 MiB，大影片無法上傳。

## 4. 瀏覽器本身的功能

- 頁內搜尋（Cmd+F）、縮放（Cmd +/−）、列印或存成 PDF。
- 書籤、可瀏覽的歷史紀錄，以及下載清單（目前只有一行通知）。
- 無障礙：沒有 accessibility tree，螢幕閱讀器讀不到內容。

## 5. 開發人員工具剩下的限制

- 沒有中斷點和逐步執行，因為 boa 沒有除錯介面。
- CDP 沒有 remote object，Puppeteer、Playwright 的完整流程跑不起來。

## 建議順序

1. **先修第一組**：HTTP 方法、請求標頭、storage 寫入磁碟、cookie 統一並保存。這些改動不大，而且直接影響 ruwebos 的應用。
2. **再做 `MutationObserver` 等 observer 和 CORS**：讓真實網站與框架能正常運作。
3. **之後是 IME**：對中文使用者最有感，但牽涉各平台的輸入法介面，工程較大。
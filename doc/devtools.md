# 開發人員工具

rubrowser 的開發人員工具提供 Elements、Console 與 Network 面板，可檢查頁面 DOM、執行 JavaScript，並查看網路請求。

## 1. 開啟與操作

| 操作 | 快捷鍵／按鈕 |
|---|---|
| 開啟或關閉開發人員工具 | `F12` |
| macOS 開啟或關閉 | `Cmd+Opt+I`（`⌥⌘I`） |
| 其他平台開啟或關閉 | `Ctrl+Shift+I` |
| 進入「選取元素」模式 | macOS `Cmd+Shift+C`、其他平台 `Ctrl+Shift+C`，或工具列的 `⬚` 按鈕 |
| 離開「選取元素」模式 | `Escape` |
| 關閉開發人員工具 | 工具列的 `✕` 按鈕 |
| 清除記錄 | 工具列的 `⊘` 按鈕，可清除 Console 或 Network 記錄 |

頁面右鍵選單中的「檢查」會選取游標下的元素。

開發人員工具停靠於頁面下方，頁面區域會隨之縮短。可拖曳上緣調整高度，最低為 80px，預設高度為 300px。

工具是瀏覽器自身 chrome document 的一部分（`src/browser/chrome.html`），內容由 Rust 根據瀏覽器狀態產生（`src/browser/devtools.rs`）。面板本身不使用 JavaScript，因此即使頁面腳本卡住，仍可開啟使用。Console 與 Network 記錄各自屬於目前分頁，並在每次導覽後重新開始。此功能沒有新增相依套件。

```mermaid
flowchart LR
  S["頁面腳本"] --> C["Console 記錄"]
  F["頁面的請求"] --> N["Network 記錄"]
  D["DOM／樣式"] --> E["Elements"]
  C --> P["chrome.html 中的開發人員工具面板"]
  N --> P
  E --> P
```

![Elements 面板：選取的元素在頁面上標出 margin、border、padding 與內容](devtools-elements.png)

## 2. Elements

Elements 面板顯示頁面的 DOM 樹：

- 初始時展開 `html` 與 `body`。
- 使用 `▸`／`▾` 展開或收合元素。
- 若元素只有一個短文字子節點，文字會直接顯示在元素列上；純空白文字節點則隱藏。
- 最多顯示 3000 列；頁面變更 DOM 時，樹最多每 300 毫秒重新整理一次。
- 游標停在元素列上時，會在頁面上標示其盒模型：邊距為橙色、邊框為黃色、內距為綠色、內容為藍色。

所選元素的側欄包含：

| 區段 | 內容 |
|---|---|
| 元素摘要 | `tag#id.class`、屬性 |
| Box Model | 邊距、邊框、內距的數值與內容尺寸 |
| Styles | 先顯示 `element.style`，再顯示相符的 CSS 規則；勝出的規則排在前面，使用者代理規則另有標記 |
| Computed | `display`、`position`、尺寸、邊距、內距、色彩、字體、flex、overflow、opacity、z-index 等常用屬性 |

側欄下方的編輯行可修改元素。輸入 `name=value` 會設定屬性，輸入 `-name` 會移除屬性；選取文字節點時，則會以輸入內容取代其文字。按 `Enter` 套用變更。

## 3. Console

Console 記錄頁面腳本、錯誤及失敗的請求：

| 訊息來源 | 顯示方式 |
|---|---|
| `console.log`、`console.info`、`console.debug` | 一般訊息 |
| `console.warn` | 黃色訊息 |
| `console.error`、未捕捉例外 | 紅色訊息，並顯示 stack |
| 失敗的請求、腳本錯誤 | 列入 Console |

重複的相同訊息會合併計數，並以徽章顯示次數。可使用 `All`、`Warnings`、`Errors` 篩選器。

Console 底部的輸入行會在頁面中以全域腳本方式執行 JavaScript，輸出格式如下：

- 程式碼顯示為 `› code`。
- 結果顯示為 `← result`。
- 字串加上引號，物件以 JSON 顯示。
- 元素顯示為 `<tag> tag#id.class`，函式顯示為 `ƒ name()`。
- 例外顯示為 `Uncaught ...`。
- 最後一個結果存放在 `$_`。

使用 `Up`／`Down` 方向鍵可叫出先前輸入。若已使用 `--no-js` 停用網頁 JavaScript，Console 會顯示相關提示。

![Console 面板：頁面的訊息與輸入行的執行結果](devtools-console.png)

## 4. Network

Network 面板記錄頁面的每個請求，包括文件、樣式表、腳本與模組、圖片、字型、`fetch`／`XMLHttpRequest` 及下載。

| 欄位 | 顯示內容 |
|---|---|
| Name | URL 最後一段路徑與查詢字串 |
| Status | HTTP 狀態碼、`(failed)`，或等待中的 `…` |
| Type | 請求類型 |
| Size | 大小 |
| Time | 毫秒，依 wall clock 計算 |

失敗的請求與 HTTP 狀態碼大於或等於 400 的列會以紅色顯示；被封鎖的跨來源請求會顯示為 failed。

點選任一列可查看詳細資料：

| 詳細資料 | 內容 |
|---|---|
| General | URL、方法、狀態與類型 |
| Response Headers | 回應標頭 |
| Request Payload | 請求內容類型與內容 |
| Response | 文字回應預覽最多 20000 個字元；二進位內容顯示類型與大小 |

面板最多保留 1000 個請求，每個 body 最多保留 256 KB。

## 5. 截圖與測試

開發人員工具可透過命令列指定面板：

```bash
rubrowser --screenshot out.png --devtools console URL
```

| 選項 | 行為 |
|---|---|
| `--devtools console` | 開啟 Console 面板 |
| `--devtools elements:.card` | 開啟 Elements 面板，並選取第一個符合 `.card` CSS selector 的元素 |
| `--devtools network` | 開啟 Network 面板 |

`RUBROWSER_INPUT` 接受 `console CODE`（在 Console 執行 `CODE`）與 `key f12` 等輸入。例如：

```bash
RUBROWSER_INPUT="console document.title" rubrowser --screenshot out.png --devtools console https://example.com/
```

執行開發人員工具測試：

```bash
cargo test devtools
```

`src/browser/mod.rs` 中共有 5 項測試，涵蓋開啟／關閉／調整大小、Console、Elements、選取元素模式與右鍵選單，以及 Network。

## 6. 尚未支援

以下功能留待後續階段：

- Sources
- Application（儲存空間）
- 效能分析
- 就地編輯 CSS 屬性
- Chrome DevTools Protocol
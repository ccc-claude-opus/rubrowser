# rubrowser 第四階段規劃：大部分網站

目標：一般網站都能**正常呈現與瀏覽**，畫面與 Chrome 相近。做法是一版一版地：用一組代表性網站截圖、與 Chrome 比對、修正差最多的地方。

## 1. 測試網站

`scripts/survey.sh` 對下列網站各產生一張 1280×822 的截圖（`--screenshot`），並拼成總覽圖，方便與 Chrome 對照。

| 類型 | 網站 |
|---|---|
| 百科、文件 | Wikipedia（英、中）、MDN、docs.rs、python.org、rust-lang.org |
| 程式碼 | GitHub |
| 新聞、論壇 | BBC News、Hacker News、old.reddit.com、Stack Overflow |
| 搜尋 | DuckDuckGo（HTML 版） |
| 社群 | X |
| 產品頁 | apple.com |
| 最小頁面 | example.com |

## 2. 第一次調查（2026-11-01）

| 網站 | 狀況 |
|---|---|
| Wikipedia、GitHub、X、MDN、docs.rs、DuckDuckGo、rust-lang.org | 版面與 Chrome 相近 |
| Hacker News | 標題被置中：quirks 模式下表格不繼承 `<center>` 的 `text-align` |
| BBC News | 文章圖片都是灰色方塊（`srcset` / `<picture>` 未支援） |
| python.org | 顯示「scripts did not run」提示：頁面的 JS 執行失敗 |
| old.reddit.com | 被導向登入頁（不認得的 User-Agent） |
| Stack Overflow | Cloudflare 的「Just a moment…」檢查頁（需要 JS；不嘗試繞過） |
| apple.com | 導覽列文字幾乎看不見 |
| example.com | 阿拉伯文沒有由右至左、字母沒有連寫；缺圖示 |
| docs.rs、BBC、Apple | 網頁字型（`@font-face`）未載入，改用系統字型 |

## 3. 里程碑

| 階段 | 內容 |
|---|---|
| **X0** 工具 | `scripts/survey.sh`：批次截圖與總覽圖 |
| **X1** HTML 相容 | quirks 模式（無 doctype）的 UA 規則；`<center>`、`align`、`<font>` 等舊屬性 |
| **X2** 網路相容 | 與主流瀏覽器相容的 User-Agent（保留 rubrowser 字樣）、`Accept`、`Accept-Language`、cookie |
| **X3** 圖片 | `srcset`（`w` / `x`）、`<picture>` / `<source>`、`loading=lazy` |
| **X4** 背景與裝飾 | `linear-gradient` / `radial-gradient`、`box-shadow`、多層背景 |
| **X5** 網頁字型 | `@font-face`（TTF / OTF / WOFF / WOFF2）下載與使用 |
| **X6** 文字 | 雙向文字（`dir=rtl`、Unicode bidi）、阿拉伯文等的字形連寫（shaping）、emoji |
| **X7** JS 相容 | 常見函式庫（Modernizr、jQuery）能跑；`fetch` / `XMLHttpRequest`；錯誤不影響呈現 |
| **X8** 瀏覽 | 表單 POST、cookie 登入狀態、`target=_blank`、下載提示 |

每完成一個里程碑：重跑調查、更新本文件的表格、commit。

## 4. 不做的事

- 不繞過機器人檢查或 CAPTCHA（Cloudflare 等）。
- DRM、WebGL、影片播放留到之後。

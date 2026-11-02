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

## 4. 進度（2026-11-02）

| 階段 | 狀態 | 內容 |
|---|---|---|
| X0 | 完成 | `scripts/survey.sh` |
| X1 | 完成 | quirks 模式 UA 規則、`-webkit-center`、`align` 屬性、固定寬度的表格欄 |
| X2 | 完成 | 相容的 User-Agent、`Accept`、`Accept-Language`、cookie、brotli |
| X3 | 完成 | `srcset` / `sizes` / `<picture>`；`<noscript>` 的後備樣式 |
| X4 | 完成 | 線性 / 放射 / 重複漸層、`box-shadow`、圓角背景裁切 |
| X5 | 完成 | `@font-face`（WOFF / WOFF2 以 `wuff` 解開），依字重挑選 |
| X6 | 完成 | `direction` / `dir`、bidi 重排、`rustybuzz` 字形整形（阿拉伯文、圖示字型連字）、中日韓斷行 |
| X7 | 完成 | ES 模組與 import map、`fetch` / `XMLHttpRequest`（同源）、custom elements、`URL` 等大量 Web API；載入時的腳本時間預算；DOM 沒變就不重算樣式 |
| X8 | 完成 | 表單 POST、`target=_blank` |

其他修正：`translate` / `transform: translate()`、`aspect-ratio`、`@property`、具名 grid 線、`[hidden]`、`clip` / `clip-path` 的隱藏寫法、SVG 的 CSS `fill` / `stroke` 與屬性中的 `var()`、絕對定位元素以最近的定位祖先為包含區塊、CSS 已定尺寸的圖片載入時不重新排版、hover 的增量樣式計算。

### 第三輪：腳本插入的資源

- 腳本插入的 `<script>` 與 `<link rel=stylesheet>` 會載入、執行並觸發 `load` / `error`（許多網站用載入器這樣載入 JS 與 CSS）；`document.currentScript`、元素的 `on…` 屬性、反映屬性的 property。
- 一個腳本超過時間上限時只停止它，不再讓整頁的 JS 停擺。
- Wikipedia（英、中）與 GitHub 仍與 Chrome 相近，且 Wikipedia 的 ResourceLoader 模組現在會執行。
- craigslist：已能跑到自己的載入流程，但主程式（1.2 MB）在 boa 上太慢而逾時；效能留到最後處理。

### 第四輪：標準功能補齊

依「對多數網站的影響」排序，每項一版、一個 commit：

| 版 | 內容 |
|---|---|
| 1 | `getComputedStyle` 回傳 cascade 算出的值（CSSOM 序列化；`display: none` 的元素也有值）；`matchMedia` 依目前 viewport 判斷 |
| 2 | 完整的 2D `transform`（`rotate`、`scale`、`skew`、`matrix`、`matrix3d` 的 2D 部分）、`rotate` / `scale` 屬性、`transform-origin`；非平移的變換以離屏圖層繪製後雙線性重取樣 |
| 3 | `text-overflow: ellipsis`：裁切的方塊中超出內容框的行在結尾截斷並加上 `…`（區塊的字型） |
| 4 | `object-fit`（`fill` / `contain` / `cover` / `none` / `scale-down`）與 `object-position`，內容超出內容框的部分裁掉 |
| 5 | `<iframe>`：`src` / `srcdoc` 載入巢狀文件（自己的 CSS、圖片、字型），預設 300×150、在 iframe 的內容框中繪製；滾輪捲動 frame；frame 內的連結在 frame 內載入（`_top` / `_parent` 換整頁、其他名稱開新分頁）。frame 內不執行 JS，也不支援 frame 中的 frame |
| 6 | 混合單位的 `calc()` / `min()` / `max()` / `clamp()` 依實際的包含區塊大小計算（以前用 viewport 近似；前一次排版量到的大小不對時再排一次）；`@container` 查詢（`container-type` / `container-name` / `container`，排版後容器大小改變時重算一次樣式）；`:lang()`、`:dir()`；`color-mix()`（預乘 alpha） |
| 7 | CSS 動畫與轉場：`@keyframes`、`animation-*` / `transition-*` 與簡寫、`cubic-bezier` / `steps` 等時間函式、`fill-mode` / `direction` / 次數 / 暫停；動畫值（opacity、transform、顏色、visibility）放在樣式覆蓋層，只影響繪製不重排；截圖會等執行中的動畫最多 2 秒。另外：`border-radius` 百分比、各邊寬度與顏色不同的圓角邊框（轉圈圖示） |
| 8 | `document.write` / `writeln`：解析器找到的腳本寫入的內容在該腳本結束時插在它後面（分次寫入的標記會合併），寫入的 `<script>` 會執行；非同步插入的腳本與載入後的寫入照 Chrome 忽略並警告 |
| 9 | JS 相容：腳本讀取尺寸 / 位置 / 計算樣式時先排版（同瀏覽器的強制排版）；無效選擇器（含不認得的偽類）讓 `querySelector` / `matches` 丟出 `SyntaxError`，選擇器只解析一次；`document.implementation.createHTMLDocument`、`importNode` / `adoptNode`；`<template>.content`；複製 fragment；模組錯誤附堆疊。jQuery 3.7 完整可用，Wikipedia 的「外觀」面板出現 |
| 10 | 表單控制項：`range`（滑桿，可點擊與拖曳，觸發 `input` / `change`，值依 min / max / step 清理）、`color`（色塊）、`file`（「Choose File / No file chosen」）、`<progress>`、`<meter>`（依 low / high / optimum 顯示綠、黃、紅）；fieldset 的 legend 跨在上框線上並留缺口 |
| 11 | 彩色 emoji：系統的彩色 emoji 字型（macOS Apple Color Emoji、Linux Noto Color Emoji，以 `memmap2` 映射不整檔讀入），以 rustybuzz 整形（國旗、ZWJ 組合、膚色），畫出字型裡的 PNG 點陣圖；文字依 emoji / 一般字分段 |
| 12 | 多欄排版：`column-count` / `column-width` / `columns`、`column-gap`（`normal` 為 1em）、`column-rule`；先以單欄寬排版，再求平衡欄高（不小於最高的不可分段），最後把行與子方塊移到各欄（排版多跑兩次） |
| 13 | 直排 `writing-mode: vertical-rl / vertical-lr`（只含行內內容的元素）：行變成欄（右到左或左到右），欄長為元素高度；中日韓字直立，其他文字轉 90°（`text-orientation: mixed`）。還不支援直排中的區塊排版與標點的直排位置 |
| 14 | 第三批調查的修正：繪製順序照 CSS 2.1 附錄 E（負 z-index、一般流、定位元素依樹順序與 z-index；相對定位也算定位元素）；行內模組腳本原本不會執行（排隊時用錯索引）已修正，模組與 `defer` / `async` 腳本延到一般腳本之後；boa 開啟 Annex B（`substr`、`escape` 等瀏覽器都有的功能）；`createElement` 依 DOM 標準檢查名稱；`window.frames` / `top` / `parent` / `length`、`window.postMessage` |

### 第二批調查

| 網站 | 狀況 |
|---|---|
| The Guardian | 版面與 Chrome 相近（具名 grid 線、SVG 標誌） |
| go.dev、Arch Linux、iThome、mozilla.org、wikipedia.org | 相近 |
| CNN | 文章與圖片正常；頁首導覽列要等它自己的 JS 顯示（目前 JS 執行失敗） |
| YouTube、craigslist | 整頁由 JS 產生，尚未能顯示 |
| Amazon、w3.org、npm、Stack Overflow | 機器人檢查頁（Akamai / Cloudflare），不嘗試通過 |

### 第三批調查（2026-11-02）

| 網站 | 狀況 |
|---|---|
| gov.uk、dev.to、ja.wikipedia、kernel.org、docs.python.org、wordpress.org、The Verge | 與 Chrome 相近 |
| python.org | 首頁的程式碼輪播（JS）現在也會出現 |
| Cloudflare | 主視覺正確（原本被一個應在下層的白色面板蓋住）；背景影片不播放；頁首選單由它的 JS 隱藏 |
| w3schools | JS 錯誤已排除；主視覺區的腳本被 Akamai 拒絕（機器人防護，不處理） |
| NPR | 標誌過大蓋到選單 |
| NASA | 頁首文字重疊 |
| microsoft.com | 以 User-Agent 判定為自動程式（保留 rubrowser 字樣，不處理） |

## 5. 不做的事

- 不繞過機器人檢查或 CAPTCHA（Cloudflare、Akamai 等）。
- 跨來源的 `fetch` 要等 CORS 檢查做好才開放。
- DRM、WebGL、影片播放留到之後。

# rubrowser 第三階段規劃：真實網站

目標：讓排版較素雅的大型網站能正確呈現，並**能實際瀏覽 Wikipedia**，畫面盡量接近一般瀏覽器。

## 1. 現況調查（2026-11-01）

| 網站 | 伺服器端 HTML | CSS | 結論 |
|---|---|---|---|
| Wikipedia（Vector 2022） | 完整文章，約 1 萬個元素 | 221 KB：`@media` 72、`@supports` 76、`var()` 550、`:not()` 391、`::before/::after` 130、`mask-image` 359（圖示）、grid、`sticky`、`calc()` | **主要目標**，不需 JS 就能完整呈現 |
| GitHub（repo 頁） | 完整內容，184 個內嵌 `<svg>` 圖示 | **4.8 MB**（37 個檔案）：`var()` 3.9 萬、`:is/:where` 4.8 千、`@layer`、`<link media>`、主題切換 | 不需 JS 就能讀；CSS 效能是關鍵 |
| X（個人頁） | 個人資料文字與 SVG 已在 HTML 中 | 1 個檔案 | 能呈現靜態內容；互動部分需要它的 React 程式 |
| Threads | `<body>` 只有一個空的 `<div>` | 86 KB 內嵌 | **全部由 JS 產生**，第三階段不處理（只保證不當掉） |
| Facebook | — | — | 依指示先不處理 |

第三階段開始時的 CSS 引擎（`simplecss`）無法處理以上大部分語法：它會跳過 `@media`、`@supports` 和其中的所有規則，也不支援 `var()`、`:not()`、偽元素。**所以第三階段的核心是一套新的 CSS 引擎。**

## 2. 驗證方式

新增無視窗的截圖模式，對同一個網址產生 PNG，與 Chrome 的畫面比對：

```bash
rubrowser --screenshot out.png --width 1280 --height 900 https://en.wikipedia.org/wiki/Rust_(programming_language)
```

每個里程碑都用這些頁面截圖檢查：Wikipedia 首頁與條目頁、GitHub repo 頁、X 個人頁。

## 3. 里程碑

| 階段 | 內容 |
|---|---|
| **W0** 工具與網路 | `--screenshot` 無視窗截圖；User-Agent、轉址後的最終網址、`//host/path` 網址；`<link media>` |
| **W1** 新 CSS 引擎 | 自寫解析器（巢狀區塊、字串、跳脫、註解）；`@media`（寬高、`prefers-color-scheme`、`print`/`screen`、`and/not/or`）、`@supports`、`@layer`（展開）、`@import`；略過 `@font-face`、`@keyframes`、`@page`。完整的選擇器引擎：`~` 組合器、`[attr^= $= *= \|= ~= i]`、`:not()` `:is()` `:where()` `:has()`（簡單形式）、`:nth-child()` 系列、`:first/last/only-child`、`*-of-type`、`:root` `:empty` `:checked` `:disabled` `:enabled` `:focus-within` `:focus-visible`，以及偽元素標記。**解析一次、規則依最右側選擇器分桶**。自訂屬性與 `var()`（含 fallback、繼承）、`calc()` / `min()` / `max()` / `clamp()`、`vw` / `vh` |
| **W2** 偽元素 | `::before` / `::after` 的 `content`（字串、`attr()`、`counter()` 基本形）、`::marker`、`list-style-type` 更多種類 |
| **W3** 排版 | CSS Grid（`grid-template-*`、`grid-area`、命名區域，用 taffy grid）；`position: sticky`（視為 relative）；float 旁的文字改為「避開 float 的區塊」；`display: contents`、`inline-flex`；`overflow-wrap`、長字斷行；`text-overflow` |
| **W4** SVG | `<img src=*.svg>`、內嵌 `<svg>`（以 `resvg` 繪製）；`mask-image`（Wikipedia 的圖示）；`srcset`、`<picture>` |
| **W5** 字型 | 加入 DejaVu Serif；`font-weight` 500–900；系統字型 fallback（中文字，見 W9） |
| **W6** 效能 | 樣式表快取、選擇器分桶、只有在有 `:hover` 規則時才重算樣式、文字量測與行內排版快取；JS 預算（過大的外部腳本略過、可關閉 JS） |
| **W7** 網站調整 | 逐站截圖比對修正：Wikipedia、GitHub、X |
| **W8** Wikipedia 瀏覽 | 站內連結、搜尋表單、目錄、圖片、上一頁；條目頁與首頁畫面接近 Chrome |
| **W9** 中文 | 系統中文字型 fallback（macOS PingFang、Noto CJK 等），讓中文維基能顯示；IME 列為之後 |

## 4. 已知限制

- **JS 驅動的網站**（Threads、Facebook、X 的動態內容）需要 `fetch`/XHR、大量 DOM API 與極大的 JS 程式；在 wasm 直譯器中執行 MB 級的 JS 會非常慢。第三階段的策略是：保證這些網站不會讓瀏覽器卡住，靜態 HTML 能呈現多少就呈現多少。
- 不做 CSS 動畫、transition、transform、box-shadow（可能以後補）。

## 5. 進度（2026-11-01）

| 階段 | 狀態 | 備註 |
|---|---|---|
| W0 | 完成 | `--screenshot`、轉址、User-Agent；`--no-js`；除錯用 `RUBROWSER_DUMP`、`RUBROWSER_PROFILE` |
| W1 | 完成 | 另外支援 `@layer` 的優先順序（未分層 > 後宣告的層 > 先宣告的層，`!important` 反過來）、邏輯屬性（`margin-inline`、`padding-block`…）、`@media` 中的 `calc()` |
| W2 | 完成 | `content` 的字串、跳脫、`attr()`、`counter()` / `counters()`、引號；`::before` / `::after` 以 DOM 之後的合成節點排版；`::marker` 的 `content` 尚未支援 |
| W3 | 完成 | grid、float 旁的文字、`display: contents`、`position: sticky`（繪製與 hit-test 時位移）；inline-block 依容器寬度 shrink-to-fit（第二次排版）；行內元素的左右 margin / padding；空區塊的 margin 可以穿透合併 |
| W4 | 完成 | `resvg`：SVG 圖片依繪製大小點陣化、內嵌 `<svg>`（還原 SVG 標籤大小寫、`currentColor`）、`mask-image`；`srcset` / `<picture>` 尚未支援 |
| W5 | 完成 | 原生版優先使用系統的 Arial / Georgia（或 Liberation），斷行位置與 Chrome 幾乎一致；DejaVu Serif；缺字退回 DejaVu |
| W6 | 部分 | Wikipedia 樣式計算約 40–100 ms、排版約 40–60 ms；GitHub（4.8 MB CSS）樣式約 100 ms。滑鼠移動造成的重算尚未做增量 |
| W7 | Wikipedia、GitHub 完成 | 兩站的版面位置與 Chrome 對齊到數個像素以內；GitHub 由 React 產生的內容（最後提交、分支數）不會出現 |
| W8 | 完成 | `#片段` 連結在頁內捲動、上一頁不重新載入、載入中保持錨點位置；`html { height: 100% }` 的網站可以捲動 |
| W9 | 部分 | 中文、日文以系統字型顯示；韓文需要另外的字型；IME 尚未支援 |

### 已知差異

- Wikipedia 頁首的 Log in 會換行（搜尋框的 flex basis 比 Chrome 大）；主選單圖示只顯示一條線。
- 需要 JS 的部分（Wikipedia 的外觀設定面板、目錄折疊，GitHub 的檔案列表最後提交欄）不會出現。
- X、Threads 只保證不當掉。

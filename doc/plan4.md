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
| 15 | `<canvas>` 2D：tiny-skia 繪圖（矩形、路徑、弧、橢圓、圓角矩形、Bézier、`Path2D`、填色與描邊、虛線、線條端點與接合、線性 / 放射 / 錐形漸層、圖樣、變形與 save / restore、裁切、`globalAlpha`、合成模式、文字與 `measureText`、`drawImage`、`getImageData` / `putImageData`、`toDataURL`、`isPointInPath`）；預設 300×150，依 CSS 大小縮放繪製。陰影與 `filter` 尚未支援 |
| 16 | `<video>` / `<audio>` 元素：依 `src` 或第一個可播放的 `<source>` 選擇資源，`poster`（依 `object-fit: contain`）、`controls` 控制列（播放 / 暫停、時間、可點擊的進度條、靜音），預設 300×150（audio 有 controls 時 300×54，沒有則不顯示）；`HTMLMediaElement` API（`play()` 的 Promise、`pause`、`currentTime`、`duration`、`volume`、`muted`、`canPlayType`、事件、`Audio()`）；Chrome 的自動播放規則（有聲音需使用者互動）。實際播放由後端提供（v17：FFmpeg），沒有後端時照標準回報錯誤 |
| 17 | FFmpeg 播放後端（feature `media`，需安裝 FFmpeg 8）：每個媒體元素一條解碼執行緒（FFmpeg 自己開啟檔案與 http(s) 網址），影格轉成 RGB 送入有上限的佇列，聲音重新取樣後由 `cpal` 輸出（音量、靜音）；主執行緒依時鐘顯示到期的影格，觸發 `loadedmetadata`、`playing`、`timeupdate`、`seeked`、`ended` 等事件；跳轉從關鍵影格解碼並丟棄目標之前的影格與聲音；`loop`；播放或載入中時視窗以約 60 fps 更新，截圖會等待 |
| 18 | 動態圖片：GIF、APNG、動態 WebP 依各影格延遲播放並循環（延遲 ≤ 10 ms 視為 100 ms，與瀏覽器相同；所有影格合計上限 64M 像素），第一次畫出時開始，視窗在下一影格到期時醒來；`<img>`、CSS 背景圖與 `drawImage` 都用當下影格。AVIF（feature `media`，以 dav1d 解碼）。`Intl`（ECMA-402，只有 en-US 資料）：`DateTimeFormat`（各欄位、`dateStyle`/`timeStyle`、`hour12`、UTC 時區、`formatToParts`）、`NumberFormat`（百分比、貨幣、單位、`compact`、小數位數、`signDisplay`）、`RelativeTimeFormat`（`numeric: "auto"`）、`PluralRules`（含序數）、`Collator`（`numeric`、`sensitivity`）、`ListFormat`、`Segmenter`、`DisplayNames`、`Locale`；`Date`/`Number` 的 `toLocale*String` 與 `localeCompare` 都經由它 |
| 19 | `filter`（`blur`、`brightness`、`contrast`、`grayscale`、`hue-rotate`、`invert`、`opacity`、`saturate`、`sepia`、`drop-shadow`，依序套用；顏色類用規格的色彩矩陣，模糊以三次方框模糊近似高斯）與 `clip-path` 基本形狀（`inset()`（含 `round`）、`circle()`、`ellipse()`、`polygon()`，以 border box 為參考框，每像素 4×4 取樣做反鋸齒）：元素整棵子樹先畫進圖層再處理後合成，順序在 `transform` 之前；`text-shadow`（多重、模糊、半透明色彩，可繼承）；`getComputedStyle` 對這三者回報 `none` |
| 20 | 腳本時鐘與中斷：`performance.now()`、`Date.now()` 在一次呼叫中依已用的 fuel 前進（約 250 萬指令 = 1 ms，可重現且不倒退），等待期限的迴圈因此會結束；呼叫用完 fuel 時改為可續跑的呼叫，給一小段寬限並標記「中斷」，JS runtime 在下一次呼叫瀏覽器原生函式或在工作佇列兩個 job 之間（安全點）以無法攔截的引擎錯誤結束腳本並丟棄剩下的 promise 工作，避免在垃圾回收途中被切斷而損壞；被切斷前的 console 輸出與計時器保留；runtime panic 的訊息寫到 console。`MessageChannel` / `MessagePort`（之後的 task 送達、`start()` 或設定 `onmessage` 才開始）、`BroadcastChannel`（同頁同名頻道）、`DOMException`（名稱與舊式 code 常數）、`document.domain`、`AbortSignal` 的 `abort` 監聽器、`AbortSignal.timeout()` / `any()` |
| 21 | 斷行：`word-break: break-all`（任兩字之間可斷）、`keep-all`（中日韓文字不在字間斷）、舊式 `word-break: break-word`；`overflow-wrap` / `word-wrap`：`break-word`（放不下一行的長字在填滿處斷開，不影響 min-content 寬度）、`anywhere`（min-content 也算這些斷點）。行數限制：`-webkit-line-clamp` / `line-clamp` 只留前 N 行，最後一行接 `…`（放不下時先截短）；`display: -webkit-box` 依 `-webkit-box-orient` 為 vertical 時當作區塊流、否則當作 flex。Canvas：`shadowColor` / `shadowBlur` / `shadowOffsetX/Y` 與 `filter`：有效果時先畫到透明圖層，套用濾鏡後先合成陰影再合成圖層（依目前的合成模式與裁切）|
| 22 | 第四次網站調查（NPR、NASA、Stack Overflow、Hacker News、Mozilla、rust-lang.org、docs.rs、Apple、BBC、Guardian）後的修正：替換元素兩邊都是 `auto` 時，`min-width` / `max-width` / `min-height` / `max-height` 依 CSS 2.1 §10.4 保持比例；只有 `viewBox`（沒有 `width` 與 `height`）的 SVG 圖片沒有自然尺寸，只有比例，寬度取可用空間；內嵌 SVG 引用頁面其他地方的元素（`<use href="#icon">` 指向圖示 sprite 的 `<symbol>`、共用的漸層）時把它們複製進來；混合 `calc()`（百分比加長度）在包含區塊大小還不知道時當作 `auto`，知道後換算成該大小的百分比交給 taffy，所以內容決定大小的容器不會被循環百分比撐大（CSS Sizing 3）；載入時每個主機最多同時 6 個連線，GET 在連線被中斷時重試一次。iframe 內的腳本延後：需要把腳本執行從分頁拆成每個文件各自一份 |
| 23 | 第五次網站調查（CNN、Ars Technica、W3C、MDN、Medium、Wikipedia 入口、YouTube、Reddit、NYT）後的修正，並以內建的 Chromium 量測同一頁當參考：堆疊上下文——正 `z-index` 的定位元素交給所屬的堆疊上下文（根元素、有 `z-index` 的定位元素、fixed / sticky、opacity、transform、filter、clip-path），在其他內容之後依 z-index 繪製（含行內的 inline-block）；`order`（flex / grid 項目依 order 排列與繪製）；`text-indent`；文字顏色的透明度（透明文字不再畫成黑色）；`display: inline` 的 `<button>` 當作 inline-block；收縮至內容寬度時不再把放不下的長字壓到可用寬度；行內百分比寬度在容器寬度改變時重新計算；JS：`NodeFilter`、`TreeWalker`、`NodeIterator`（DOM 規格的走訪演算法），`UIEvent`、`MouseEvent`、`PointerEvent`、`WheelEvent`、`KeyboardEvent`、`FocusEvent`、`InputEvent` 等事件介面與其初始化字典，瀏覽器送出的事件使用對應的介面 |
| 24 | 第六次網站調查（Wikipedia 條目、GitHub、Stripe、rust-lang.org）後的修正：負 `z-index` 的定位元素交給所屬的堆疊上下文，在它的背景之後、其他內容之前繪製（先用不畫圖的一趟找出它們與當時的裁切），所以會被同一個上下文裡較早元素的背景蓋住（CSS 2.1 附錄 E）；自訂屬性在宣告的元素上計算（`var()` 代入後再繼承），自我參照或循環的自訂屬性無效，`var()` 改用預設值（CSS Variables）；SVG 文字：resvg 使用內附的 DejaVu 字型與系統已安裝的字型，內嵌 `<svg>` 的文字繼承 CSS 字型；內嵌 `<svg>` 的視窗是 CSS 給它的方框（不再以 300×150 縮放）；實線邊框與不同顏色的相鄰邊在角落沿對角線接合（CSS 三角形） |
| 25 | 以「最常被看的網站的主要內容要看得清楚」為目標的第七次調查（新聞、百科、字典、文件、台灣常用網站）後的修正：選擇器裡接在另一個偽元素後的偽元素（`::part(x)::after`）不再被當成元素自己的 `::after`；浮動元素緊接在只有一行的行內內容之後、且放得下時，放到那一行的頂端（CSS 2.1 §9.5.1），文字在旁邊排（多輪排版：上一輪量到行數與寬度）；清除浮動（`clear`）的區塊裡只有浮動元素時，不再被自己裡面的浮動強制產生 clearance 而往上移（修改 taffy，見 `vendor/taffy/PATCHED.md`）；使用者的語言偏好（`LANG` 等環境變數，macOS 上為系統偏好語言）用於 `Accept-Language`、`navigator.language` 與 `navigator.languages`（新的主機函式 `languages`） |

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

### 第六批調查（2026-11-03）

| 網站 | 狀況 |
|---|---|
| Wikipedia 條目 | 頁首與工具列圖示原本只有 10px（自我參照的 `--font-size-medium`），現在與 Chrome 相同 |
| GitHub 專案頁 | 授權條目旁的下拉三角形原本畫成長條 |
| Stripe | 「Sign in」按鈕用 SVG 遮罩文字鏤空，原本整顆空白 |
| rust-lang.org | 與 Chrome 相近 |

### 第七批調查（2026-11-03）：常用網站的主要內容

| 網站 | 狀況 |
|---|---|
| Guardian、BBC、Yahoo News、Yahoo 奇摩新聞、Bing 搜尋結果、中文維基百科、百度百科、Hacker News、MDN、NPR | 主要內容清楚 |
| Microsoft Learn | 原本每個元素後面都有紅色 `*`（`::part()::after` 規則），已修正；頁首選單由腳本產生，超過時間預算 |
| 中央社 | 頁尾區塊原本疊在頁首上（taffy 的 clearance 錯誤），已修正；原本以 `en-US` 自稱而跳出「不支援繁體中文」提示，已改用系統語言 |
| PTT | 日期與「⋯」原本掉到下一行、把下一篇的推文數擠歪，已修正 |
| 聯合新聞網 | 主要內容正常；頁首下方空白是廣告位置 |
| python.org | 「Donate」按鈕仍在搜尋列上方（收縮至內容寬度的浮動容器沒有把同一行的浮動算進寬度） |
| IMDb、ESPN（AWS WAF）、Britannica、Merriam-Webster、Fandom（Cloudflare）、Allrecipes、Investopedia | 機器人檢查頁，不處理 |
| PTT 部分看板 | 需先同意滿十八歲的條款，不代替使用者同意 |

## 5. 不做的事

- 不做任何可能違法的事。
- 不繞過機器人檢查或 CAPTCHA（Cloudflare、Akamai 等）。
- 不在專案裡附帶有版權的字型或素材；SVG 文字只用內附的自由字型與使用者系統已安裝的字型。
- 跨來源的 `fetch` 要等 CORS 檢查做好才開放。
- DRM、WebGL、影片播放留到之後。

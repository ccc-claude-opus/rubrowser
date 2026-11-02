# rubrowser 腳本 ABI（v1）

rubrowser 不執行 JavaScript，網頁的行為由 **WebAssembly 模組**負責。本文件是瀏覽器和 wasm 模組之間的介面規格。

一般情況下請直接使用 `rubrowser::sdk`（見 [scripting.md](scripting.md)），它把這裡的低階函式包成安全的 Rust API。只有想用其他語言寫腳本，或想了解底層時才需要讀本文件。

## 1. 載入

```html
<script type="application/wasm" src="app.wasm"></script>
```

- `type` 為 `application/wasm`（或 `wasm`），或 `src` 以 `.wasm` 結尾，才會被當成腳本。
- 其他 `<script>`（JavaScript）一律忽略。
- 腳本在 HTML 解析完、DOM 建好之後載入，依文件順序取得。每個腳本是獨立的 wasm instance。
- 相對路徑以網頁 URL 解析；內建頁面 `about:X` 旁的檔案也是內建的（`counter.wasm` → `about:counter.wasm`）。

## 2. 模組匯出（export）

| 名稱 | 型別 | 說明 |
|---|---|---|
| `memory` | memory | **必要**。瀏覽器透過它讀寫字串 |
| `rb_init` | `() -> ()` | 載入後呼叫一次 |
| `rb_event` | `(listener: i32) -> ()` | 事件發生時呼叫；事件內容用 `event_*` 查詢 |
| `rb_timer` | `(timer: i32) -> ()` | `set_timeout` 到期時呼叫 |
| `rb_alloc` | `(len: i32) -> i32` | 語言 runtime 才需要：配置放原始碼的記憶體 |
| `rb_eval` | `(src, src_len, name, name_len) -> ()` | 語言 runtime 才需要：執行原始碼 |

`listener` 與 `timer` 是腳本自己在註冊時給的 id，瀏覽器原封不動傳回。

## 3. 瀏覽器提供的函式（import，模組名 `rb`）

### 約定

- **節點 handle**：`i32`，-1 表示「沒有」。handle 在整個頁面生命週期內有效；節點被移除後仍可使用（只是不在文件中）。
- **字串輸入**：`(ptr, len)`，UTF-8。
- **字串輸出**：腳本提供緩衝區 `(ptr, cap)`，瀏覽器寫入最多 `cap` 位元組，回傳**完整長度**。若回傳值大於 `cap`，請配置更大的緩衝區再呼叫一次。回傳 -1 表示不存在。
- 指標超出記憶體範圍會使呼叫 trap（腳本被停止），不會影響瀏覽器。

### 查詢

| 函式 | 說明 |
|---|---|
| `query(sel_ptr, sel_len) -> i32` | 第一個符合 CSS 選擇器的元素；沒有回傳 -1，選擇器無效（含不認得的偽類）回傳 -2 |
| `query_all(sel_ptr, sel_len, out_ptr, max) -> i32` | 把最多 `max` 個 handle（`i32` 陣列）寫到 `out_ptr`，回傳總數；選擇器無效回傳 -1 |
| `query_within(root, sel_ptr, sel_len, out_ptr, max) -> i32` | 同上，但只找 `root` 底下的節點（`root` 不在文件中也可以） |
| `rect(h, which) -> f64` | 上次排版的幾何（邏輯像素、相對於視窗）：0 x、1 y、2 寬、3 高（border box），4 寬、5 高（padding box）；`h = 0` 是視窗 |
| `http(url_ptr, url_len, method_ptr, method_len, body_ptr, body_len, type_ptr, type_len) -> i32` | 發出 HTTP 請求（只限同源），回傳請求編號；完成時瀏覽器呼叫匯出的 `rb_http(id, status)`（`status` 0 表示失敗） |
| `http_body(id, ptr, cap) -> i32` | 在 `rb_http` 中讀取回應內容，回傳總長度 |
| `parent(h) -> i32` | 父元素 |
| `relative(h, which) -> i32` | `which`：0 父節點、1 第一個子節點、2 最後一個子節點、3 下一個兄弟、4 上一個兄弟（含文字節點） |
| `child_count(h) -> i32` / `child_at(h, i) -> i32` | 子節點數與第 `i` 個子節點 |
| `special(which) -> i32` | 0 document、1 `<html>`、2 `<head>`、3 `<body>` |
| `node_type(h) -> i32` | 1 元素、3 文字、8 註解、9 文件、-1 無效 |
| `tag_name(h, ptr, cap) -> i32` | 小寫標籤名 |
| `matches(h, sel_ptr, sel_len) -> i32` | 是否符合選擇器（1 / 0）；選擇器無效回傳 -1 |

### 內容與屬性

| 函式 | 說明 |
|---|---|
| `get_text(h, ptr, cap) -> i32` | 所有子孫文字 |
| `set_text(h, ptr, len)` | 以一個文字節點取代所有子節點 |
| `get_attr(h, name_ptr, name_len, ptr, cap) -> i32` | 屬性值，不存在回傳 -1 |
| `canvas(h, cmd_ptr, cmd_len, ptr, cap) -> i32` | `<canvas>` 的 2D 繪圖指令：名稱與參數以 U+0001 分隔（例如 `fillRect␁0␁0␁10␁10`）；查詢指令（`measureText`、`getImageData`、`toDataURL`、`isPointInPath`、建立漸層 / 圖樣）把結果字串寫到 `ptr`，否則回傳 -1 |
| `media_matches(ptr, len) -> i32` | 媒體查詢（如 `(max-width: 600px)`）是否符合目前的 viewport（`matchMedia`） |
| `computed_style(h, name_ptr, name_len, ptr, cap) -> i32` | CSS 屬性的計算值（`getComputedStyle`；不認得的屬性為空字串），`h` 不是元素時回傳 -1 |
| `set_attr(h, name_ptr, name_len, val_ptr, val_len)` | 設定屬性 |
| `remove_attr(h, name_ptr, name_len)` | 移除屬性 |
| `class_op(h, op, ptr, len) -> i32` | `op`：0 查詢、1 加入、2 移除、3 切換；回傳之後是否有此 class |
| `set_style(h, prop_ptr, prop_len, val_ptr, val_len)` | 設定 `style` 屬性中的一個屬性；空值表示移除 |
| `get_value(h, ptr, cap) -> i32` | `<input>` 目前的值 |
| `set_value(h, ptr, len)` | 設定 `<input>` 的值 |
| `get_html(h, outer, ptr, cap) -> i32` | `outer` 為 0 時是 innerHTML，1 是 outerHTML |
| `set_html(h, ptr, len)` | 設定 innerHTML（以 html5ever 片段解析） |
| `attr_count(h) -> i32` / `attr_name_at(h, i, ptr, cap) -> i32` | 列出屬性名稱 |

### 建立與移動節點

| 函式 | 說明 |
|---|---|
| `create_element(tag_ptr, tag_len) -> i32` | 建立（未插入的）元素 |
| `create_text(ptr, len) -> i32` | 建立文字節點 |
| `insert(parent, child, before) -> i32` | 把 `child` 插到 `parent` 中 `before` 之前（`before` = -1 表示最後）；成功回傳 1 |
| `remove(h)` | 從文件移除 |

### 事件

| 函式 | 說明 |
|---|---|
| `listen(h, ev_ptr, ev_len, listener)` | 在節點上登記事件（`h` 為 0 表示整份文件） |
| `unlisten(h, ev_ptr, ev_len, listener)` | 取消登記 |
| `prevent_default()` | 在事件處理中呼叫，取消瀏覽器的預設動作（例如連結跳轉） |
| `stop_propagation()` | 不再往祖先冒泡（同一節點上的其他 listener 仍會執行） |
| `event_string(which, ptr, cap) -> i32` | 0 事件種類、1 key 名稱（`"a"`、`"Enter"`、`"ArrowLeft"`…） |
| `event_int(which) -> i32` | 0 target、1 currentTarget、2 鍵碼、3 修飾鍵、4 滑鼠按鍵 |
| `event_pos(which) -> f64` | 0 x、1 y（頁面座標） |

事件會**冒泡**：先呼叫目標節點上的 listener，再往父節點一路到根。

| 事件 | 觸發時機 | `key` / `mods` |
|---|---|---|
| `click` | 滑鼠點擊、或焦點在按鈕/連結時按 Enter / Space | 座標 |
| `dblclick` | 400 ms 內在同一元素點兩下 | 座標 |
| `mousedown` / `mouseup` | 按下／放開滑鼠左鍵 | 座標 |
| `mouseover` / `mouseout` | 滑鼠移入／移出元素 | 座標 |
| `input` | `<input>` 的內容改變 | — |
| `change` | 在 `<input>` 中按 Enter | `key::ENTER` |
| `keydown` | 頁面有鍵盤焦點時按鍵；目標是焦點元素或 `<body>` | 鍵碼、key 名稱、修飾鍵 |
| `focus` / `blur` | 元素取得／失去焦點 | — |

鍵碼與修飾鍵位元定義在 `rubrowser::abi`（`key::ENTER = 13`、`modifier::SHIFT = 1` 等）。字母與數字的鍵碼是大寫 ASCII；其他可列印字元（`+`、`.` 等）的鍵碼是 `key::CHAR_BASE`（1000）加上字元碼。

### 其他

| 函式 | 說明 |
|---|---|
| `set_timeout(ms: f64, timer: i32)` | `ms` 毫秒後呼叫 `rb_timer(timer)` |
| `clear_timeout(timer: i32)` | 取消尚未觸發的計時器 |
| `location(ptr, cap) -> i32` | 目前網頁的網址 |
| `history_go(delta: i32)` | 在歷史中移動（-1 是上一頁） |
| `navigate(ptr, len)` | 以目前頁面為基準開啟網址 |
| `log(ptr, len)` | 輸出到主控台（原生版為 stderr，網頁版為瀏覽器 console） |
| `now() -> f64` | 瀏覽器啟動後的毫秒數 |
| `wall_time() -> f64` | 真實時間：Unix epoch 起的毫秒數（`Date.now()`） |

## 4. 沙箱

| 限制 | 值 | 超過時 |
|---|---|---|
| 每次呼叫的指令數（fuel） | 30,000,000 | 呼叫中止，腳本被停止並記錄錯誤 |
| 線性記憶體 | 64 MiB | `memory.grow` 失敗 |
| 單一字串 | 1 MiB | 截斷 |
| listener 數 | 10,000 | 之後的登記被忽略 |

腳本只能透過上面的函式和外界互動，無法存取檔案、網路或其他分頁。

## 5. 範例（WebAssembly 文字格式）

```wat
(module
  (import "rb" "query" (func $query (param i32 i32) (result i32)))
  (import "rb" "set_text" (func $set_text (param i32 i32 i32)))
  (import "rb" "listen" (func $listen (param i32 i32 i32 i32)))
  (import "rb" "event_int" (func $event_int (param i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#btn")
  (data (i32.const 8) "click")
  (data (i32.const 16) "Clicked!")
  (func (export "rb_init")
    (call $listen (call $query (i32.const 0) (i32.const 4))
                  (i32.const 8) (i32.const 5) (i32.const 1)))
  (func (export "rb_event") (param $id i32)
    ;; event_int(0) is the event's target.
    (call $set_text (call $event_int (i32.const 0)) (i32.const 16) (i32.const 8))))
```

## 6. 語言 runtime 協定

JavaScript 等直譯式語言以「runtime 模組」執行：一個額外匯出下列函式的 wasm 模組（見 [javascript.md](javascript.md)）。

| 匯出 | 說明 |
|---|---|
| `rb_alloc(len: i32) -> i32` | 配置 `len` 位元組，讓瀏覽器寫入原始碼與檔名 |
| `rb_eval(src, src_len, name, name_len)` | 執行一段原始碼；例外自行回報到 `log` |
| `rb_event` / `rb_timer` | 和一般模組相同，由 runtime 轉給語言內的處理函式 |

瀏覽器每頁每種語言只建立一個 runtime instance，依文件順序對每個 `<script>` 呼叫一次 `rb_eval`。runtime 的 fuel 上限是每次呼叫 20 億條指令、記憶體 512 MiB。

網頁可以用 `<link rel="rubrowser-runtime" type="text/python" href="python.wasm">` 提供自己的 runtime；詳見 [languages.md](languages.md)。

## 7. 表單控制項

| 函式 | 說明 |
|---|---|
| `get_checked(h) -> i32` / `set_checked(h, v)` | checkbox / radio 是否勾選（radio 會取消同組其他項） |
| `get_selected(h) -> i32` / `set_selected(h, i)` | `<select>` 選中的 option 索引（-1 表示沒有） |
| `form_action(h, op)` | 對 `<form>`：0 送出（不觸發 submit 事件）、1 重設 |

`get_value` / `set_value` 也適用於 `<select>`（option 的 value）與 `<textarea>`。

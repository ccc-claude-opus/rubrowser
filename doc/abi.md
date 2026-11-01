# rubrowser 腳本 ABI（v0）

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
| `rb_event` | `(listener: i32, target: i32, key: i32, mods: i32) -> ()` | 事件發生時呼叫 |
| `rb_timer` | `(timer: i32) -> ()` | `set_timeout` 到期時呼叫 |

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
| `query(sel_ptr, sel_len) -> i32` | 第一個符合 CSS 選擇器的元素 |
| `query_all(sel_ptr, sel_len, out_ptr, max) -> i32` | 把最多 `max` 個 handle（`i32` 陣列）寫到 `out_ptr`，回傳總數 |
| `parent(h) -> i32` | 父元素 |

### 內容與屬性

| 函式 | 說明 |
|---|---|
| `get_text(h, ptr, cap) -> i32` | 所有子孫文字 |
| `set_text(h, ptr, len)` | 以一個文字節點取代所有子節點 |
| `get_attr(h, name_ptr, name_len, ptr, cap) -> i32` | 屬性值，不存在回傳 -1 |
| `set_attr(h, name_ptr, name_len, val_ptr, val_len)` | 設定屬性 |
| `remove_attr(h, name_ptr, name_len)` | 移除屬性 |
| `class_op(h, op, ptr, len) -> i32` | `op`：0 查詢、1 加入、2 移除、3 切換；回傳之後是否有此 class |
| `set_style(h, prop_ptr, prop_len, val_ptr, val_len)` | 設定 `style` 屬性中的一個屬性；空值表示移除 |
| `get_value(h, ptr, cap) -> i32` | `<input>` 目前的值 |
| `set_value(h, ptr, len)` | 設定 `<input>` 的值 |

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
| `listen(h, ev_ptr, ev_len, listener)` | 在節點上登記事件 |
| `prevent_default()` | 在事件處理中呼叫，取消瀏覽器的預設動作（例如連結跳轉） |

事件會**冒泡**：先呼叫目標節點上的 listener，再往父節點一路到根。

| 事件 | 觸發時機 | `key` / `mods` |
|---|---|---|
| `click` | 滑鼠點擊、或焦點在按鈕/連結時按 Enter / Space | 0 |
| `input` | `<input>` 的內容改變 | 0 |
| `change` | 在 `<input>` 中按 Enter | `key::ENTER` |
| `keydown` | 頁面有鍵盤焦點時按鍵；目標是焦點元素或 `<body>` | 鍵碼、修飾鍵位元 |

鍵碼與修飾鍵位元定義在 `rubrowser::abi`（`key::ENTER = 13`、`modifier::SHIFT = 1` 等）。

### 其他

| 函式 | 說明 |
|---|---|
| `set_timeout(ms: f64, timer: i32)` | `ms` 毫秒後呼叫 `rb_timer(timer)` |
| `navigate(ptr, len)` | 以目前頁面為基準開啟網址 |
| `log(ptr, len)` | 輸出到主控台（原生版為 stderr，網頁版為瀏覽器 console） |
| `now() -> f64` | 瀏覽器啟動後的毫秒數 |

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
  (memory (export "memory") 1)
  (data (i32.const 0) "#btn")
  (data (i32.const 8) "click")
  (data (i32.const 16) "Clicked!")
  (func (export "rb_init")
    (call $listen (call $query (i32.const 0) (i32.const 4))
                  (i32.const 8) (i32.const 5) (i32.const 1)))
  (func (export "rb_event") (param $id i32) (param $target i32) (param i32 i32)
    (call $set_text (local.get $target) (i32.const 16) (i32.const 8))))
```

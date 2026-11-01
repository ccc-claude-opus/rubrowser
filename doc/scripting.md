# 用 Rust 寫網頁腳本

rubrowser 不支援 JavaScript，網頁的互動改用 **Rust 編成的 WebAssembly**。本文說明如何用 `rubrowser::sdk` 寫一個腳本。底層介面請見 [abi.md](abi.md)。

## 1. 建立專案

```bash
cargo new --lib hello-page
```

`Cargo.toml`：

```toml
[package]
name = "hello-page"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
rubrowser = { version = "0.1", default-features = false, features = ["sdk"] }

[profile.release]
opt-level = "s"
panic = "abort"
strip = true
```

只開 `sdk` feature 時，rubrowser **沒有任何依賴**，編出來的 wasm 通常只有十幾 KB。

## 2. 寫腳本

`src/lib.rs`：

```rust
use rubrowser::sdk::*;

fn init() {
    let out = query("#out").unwrap();
    query("#hello").unwrap().on("click", move |_| {
        out.set_text("Hello from Rust!");
        out.add_class("shown");
    });
}

rubrowser::script!(init);
```

`script!` 巨集會產生瀏覽器需要的匯出函式：`init` 在頁面載入後執行一次，事件與計時器則轉給你登記的閉包。

## 3. 編譯並放進網頁

```bash
cargo build --release --target wasm32-unknown-unknown
```

```html
<!doctype html>
<title>Hello</title>
<style>
  .shown { color: #1a73e8; font-weight: bold; }
</style>
<button id="hello">Say hello</button>
<p id="out"></p>
<script type="application/wasm" src="hello_page.wasm"></script>
```

把 `target/wasm32-unknown-unknown/release/hello_page.wasm` 和 HTML 放在一起，用 rubrowser 開啟：

```bash
rubrowser index.html
```

## 4. API 一覽

### 找元素

| 函式 | 說明 |
|---|---|
| `query(sel) -> Option<Element>` | 第一個符合選擇器的元素 |
| `query_all(sel) -> Vec<Element>` | 所有符合的元素 |
| `create(tag) -> Element` | 建立新元素（尚未插入） |

### `Element`

| 方法 | 說明 |
|---|---|
| `text()` / `set_text(s)` | 讀取／取代文字內容 |
| `attr(name)` / `set_attr(name, v)` / `remove_attr(name)` | 屬性 |
| `has_class` / `add_class` / `remove_class` / `toggle_class` | class |
| `set_style(prop, value)` | 行內樣式（空字串表示移除） |
| `value()` / `set_value(s)` | `<input>` 的值 |
| `append(child)` / `insert_before(child, before)` / `append_text(s)` | 插入子節點 |
| `remove()` | 從文件移除 |
| `parent()` | 父元素 |
| `on(event, handler)` | 登記事件處理 |

### 事件

```rust
input.on("keydown", |e| {
    if e.key == key::ESCAPE {
        e.target.set_value("");
        e.prevent_default();
    }
});
```

| 事件 | 時機 |
|---|---|
| `click` | 點擊（或在焦點按鈕上按 Enter / Space） |
| `input` | `<input>` 內容改變 |
| `change` | 在 `<input>` 中按 Enter |
| `keydown` | 頁面上按鍵（`e.key`、`e.mods`） |

事件會冒泡到祖先元素。`e.prevent_default()` 會取消預設動作，例如連結跳轉。

### 計時與其他

| 函式 | 說明 |
|---|---|
| `set_timeout(ms, f)` | `ms` 毫秒後執行一次 |
| `set_interval(ms, f)` | 每 `ms` 毫秒執行一次 |
| `now()` | 毫秒時間 |
| `navigate(url)` | 開啟網址 |
| `log(msg)` | 寫到主控台 |

## 5. 共用狀態

事件處理閉包是 `'static` 的，要在多個處理器之間共用狀態，可以用 `Rc<Cell<_>>` 或 `Rc<RefCell<_>>`：

```rust
use std::cell::Cell;
use std::rc::Rc;

let n = Rc::new(Cell::new(0));
for (sel, step) in [("#inc", 1), ("#dec", -1)] {
    let n = n.clone();
    query(sel).unwrap().on("click", move |_| n.set(n.get() + step));
}
```

## 6. 範例

專案的 `examples/` 有三個完整範例，在 rubrowser 中直接開啟即可：

| 範例 | 網址 | 示範 |
|---|---|---|
| [counter.rs](../examples/counter.rs) | `about:counter` | 事件、共用狀態、行內樣式 |
| [todo.rs](../examples/todo.rs) | `about:todo` | 建立／刪除元素、讀取輸入框、`change` 事件 |
| [stopwatch.rs](../examples/stopwatch.rs) | `about:stopwatch` | `set_interval`、`now()` |

## 7. 限制

- 每次呼叫最多執行約三千萬個 wasm 指令；超過時腳本會被停止，錯誤寫到主控台。
- 記憶體上限 64 MiB。
- 腳本無法存取網路或檔案；只能透過 `navigate` 換頁。

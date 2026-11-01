//! A todo list: add items, mark them done, delete them.

use rubrowser::sdk::*;

fn add_item(list: Element, text: &str) {
    let item = create("li");
    item.add_class("item");
    let label = create("span");
    label.add_class("label");
    label.set_text(text);
    let delete = create("button");
    delete.add_class("delete");
    delete.set_text("\u{2715}");
    item.append(label);
    item.append(delete);
    list.append(item);
    // Clicking the text toggles "done"; the button removes the item.
    label.on("click", move |_| {
        item.toggle_class("done");
        update_count();
    });
    delete.on("click", move |_| {
        item.remove();
        update_count();
    });
    update_count();
}

fn update_count() {
    let items = query_all(".item");
    let left = items.iter().filter(|i| !i.has_class("done")).count();
    if let Some(c) = query("#left") {
        c.set_text(&format!("{left} of {} left", items.len()));
    }
}

fn init() {
    let list = query("#list").expect("#list");
    let input = query("#new").expect("#new");
    let add = move || {
        let text = input.value();
        if !text.trim().is_empty() {
            add_item(list, text.trim());
            input.set_value("");
        }
    };
    query("#add").expect("#add").on("click", move |_| add());
    input.on("change", move |_| add());
    for text in ["Parse HTML", "Compute styles", "Lay out boxes"] {
        add_item(list, text);
    }
    log("todo script ready");
}

rubrowser::script!(init);

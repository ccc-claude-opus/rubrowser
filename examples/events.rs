//! DOM playground: events, tree navigation and innerHTML.

use std::cell::Cell;
use std::rc::Rc;

use rubrowser::sdk::*;

fn log_line(text: &str) {
    let out = query("#log").expect("#log");
    let line = create("div");
    line.set_text(text);
    match out.first_child() {
        Some(first) => out.insert_before(line, first),
        None => out.append(line),
    }
    // Keep the log short.
    let lines = out.children();
    for old in lines.iter().skip(8) {
        old.remove();
    }
}

fn init() {
    // Hovering cards highlights them; the event bubbles from inner text.
    for card in query_all(".card") {
        card.on("mouseover", move |_| card.add_class("hot"));
        card.on("mouseout", move |_| card.remove_class("hot"));
    }

    // Clicks inside #inner do not reach #outer: stopPropagation.
    query("#outer").expect("#outer").on("click", |e| {
        log_line(&format!("outer got click from <{}>", e.target.tag()));
    });
    query("#inner").expect("#inner").on("click", |e| {
        log_line("inner click (stopped)");
        e.stop_propagation();
    });

    query("#dbl").expect("#dbl").on("dblclick", |e| {
        e.target.toggle_class("on");
        log_line("double click");
    });

    // innerHTML and tree navigation.
    let list = query("#list").expect("#list");
    query("#fill").expect("#fill").on("click", move |_| {
        list.set_inner_html("<li>one</li><li><b>two</b></li><li>three</li>");
        let n = list.children().len();
        let first = list.first_child().map(|c| c.text()).unwrap_or_default();
        log_line(&format!("{n} items, first = {first}"));
    });

    // Keyboard: the document hears keys that are not in an input.
    document().on("keydown", |e| {
        if e.target.tag() != "input" {
            log_line(&format!("key {:?}", e.key));
        }
    });
    query("#name").expect("#name").on("input", |e| {
        query("#hello")
            .expect("#hello")
            .set_text(&format!("Hello, {}!", e.target.value()));
    });

    // An interval that stops itself after five ticks.
    let ticks = Rc::new(Cell::new(0));
    let timer = Rc::new(Cell::new(None));
    let t2 = timer.clone();
    timer.set(Some(set_interval(200.0, move || {
        ticks.set(ticks.get() + 1);
        query("#ticks")
            .expect("#ticks")
            .set_text(&ticks.get().to_string());
        if ticks.get() == 5 {
            if let Some(t) = t2.get() {
                clear_timeout(t);
            }
        }
    })));
    log_line(&format!("ready at {}", location()));
}

rubrowser::script!(init);

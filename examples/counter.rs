//! A counter: two buttons change a number on the page.

use std::cell::Cell;
use std::rc::Rc;

use rubrowser::sdk::*;

fn init() {
    let count = query("#count").expect("#count");
    let n = Rc::new(Cell::new(0));
    for (selector, step) in [("#inc", 1), ("#dec", -1)] {
        let n = n.clone();
        query(selector).expect(selector).on("click", move |_| {
            n.set(n.get() + step);
            count.set_text(&n.get().to_string());
            count.set_style("color", if n.get() < 0 { "#c5221f" } else { "" });
        });
    }
    query("#reset").expect("#reset").on("click", move |_| {
        n.set(0);
        count.set_text("0");
        count.set_style("color", "");
    });
}

rubrowser::script!(init);

//! A stopwatch driven by timers.

use std::cell::Cell;
use std::rc::Rc;

use rubrowser::sdk::*;

#[derive(Default)]
struct Watch {
    /// Accumulated milliseconds while stopped.
    total: Cell<f64>,
    /// When the current run started, if running.
    started: Cell<Option<f64>>,
}

impl Watch {
    fn elapsed(&self) -> f64 {
        self.total.get() + self.started.get().map_or(0.0, |s| now() - s)
    }
}

fn show(ms: f64) {
    let tenths = (ms / 100.0) as u64;
    let text = format!(
        "{:02}:{:02}.{}",
        tenths / 600,
        tenths / 10 % 60,
        tenths % 10
    );
    query("#time").expect("#time").set_text(&text);
}

fn init() {
    let watch = Rc::new(Watch::default());
    let toggle = query("#toggle").expect("#toggle");
    {
        let watch = watch.clone();
        toggle.on("click", move |_| match watch.started.get() {
            Some(s) => {
                watch.total.set(watch.total.get() + now() - s);
                watch.started.set(None);
                toggle.set_text("Start");
                toggle.remove_class("running");
            }
            None => {
                watch.started.set(Some(now()));
                toggle.set_text("Stop");
                toggle.add_class("running");
            }
        });
    }
    {
        let watch = watch.clone();
        query("#reset").expect("#reset").on("click", move |_| {
            watch.total.set(0.0);
            if watch.started.get().is_some() {
                watch.started.set(Some(now()));
            }
            show(0.0);
        });
    }
    set_interval(100.0, move || {
        if watch.started.get().is_some() {
            show(watch.elapsed());
        }
    });
}

rubrowser::script!(init);

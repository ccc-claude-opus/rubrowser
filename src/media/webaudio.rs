//! The Web Audio API's rendering side (a subset): an audio graph of
//! oscillators and gain nodes ending at the destination, with `AudioParam`
//! automation (setValueAtTime, linear and exponential ramps, setTargetAtTime,
//! cancelScheduledValues) computed as the Web Audio spec (§1.6.3) describes.
//! Scripts build the graph through commands ([`Engine::command`]); the
//! samples go to the system's audio output (cpal, with the `media`
//! feature) and the context's `currentTime` is the audio clock. Without an
//! output the clock follows the wall clock and nothing is heard.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wave {
    Sine,
    Square,
    Sawtooth,
    Triangle,
}

/// One automation event of an `AudioParam`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    Set { v: f64, t: f64 },
    Linear { v: f64, t: f64 },
    Exponential { v: f64, t: f64 },
    Target { v: f64, t: f64, tau: f64 },
}

impl Ev {
    fn time(&self) -> f64 {
        match *self {
            Ev::Set { t, .. } | Ev::Linear { t, .. } | Ev::Exponential { t, .. } | Ev::Target { t, .. } => t,
        }
    }
}

#[derive(Clone, Debug)]
struct Param {
    /// The value when no event applies (`.value = …`).
    value: f64,
    events: Vec<Ev>,
}

impl Param {
    fn new(value: f64) -> Param {
        Param { value, events: Vec::new() }
    }

    /// The value at time `t` (Web Audio §1.6.3).
    fn at(&self, t: f64) -> f64 {
        let mut v = self.value; // value before any event
        let mut prev: Option<(f64, f64)> = None; // (time, value) of the last event reached
        for (i, e) in self.events.iter().enumerate() {
            let et = e.time();
            match *e {
                Ev::Linear { v: v1, t: t1 } | Ev::Exponential { v: v1, t: t1 } => {
                    let (t0, v0) = prev.unwrap_or((0.0, v));
                    if t < t1 {
                        if t < t0 {
                            return v;
                        }
                        let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                        return match e {
                            Ev::Exponential { .. } if v0 * v1 > 0.0 => v0 * (v1 / v0).powf(f),
                            Ev::Exponential { .. } => v0,
                            _ => v0 + (v1 - v0) * f,
                        };
                    }
                    v = v1;
                    prev = Some((t1, v1));
                }
                Ev::Set { v: v1, .. } => {
                    if t < et {
                        return v;
                    }
                    v = v1;
                    prev = Some((et, v1));
                }
                Ev::Target { v: target, tau, .. } => {
                    if t < et {
                        return v;
                    }
                    let start = v;
                    let next = self.events.get(i + 1).map(Ev::time);
                    let until = next.map_or(t, |n| t.min(n));
                    let value = target + (start - target) * (-(until - et) / tau.max(1e-6)).exp();
                    if next.is_none_or(|n| t < n) {
                        return value;
                    }
                    v = value;
                    prev = Some((until, value));
                }
            }
        }
        v
    }

    fn command(&mut self, op: &str, value: f64, time: f64, extra: f64) {
        let insert = |events: &mut Vec<Ev>, e: Ev| {
            let at = events.partition_point(|x| x.time() <= e.time());
            events.insert(at, e);
        };
        match op {
            "value" => {
                self.value = value;
                self.events.clear();
            }
            "set" => insert(&mut self.events, Ev::Set { v: value, t: time }),
            "linear" => insert(&mut self.events, Ev::Linear { v: value, t: time }),
            "exponential" => insert(&mut self.events, Ev::Exponential { v: value, t: time }),
            "target" => insert(&mut self.events, Ev::Target { v: value, t: time, tau: extra }),
            // cancelScheduledValues(t): events at or after t go
            "cancel" => self.events.retain(|e| e.time() < time),
            // cancelAndHoldAtTime(t): keep the value reached at t
            "hold" => {
                let held = self.at(time);
                self.events.retain(|e| e.time() < time);
                insert(&mut self.events, Ev::Set { v: held, t: time });
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
enum Kind {
    Destination,
    Gain { gain: Param },
    Oscillator { wave: Wave, frequency: Param, detune: Param, start: f64, stop: f64, phase: f64 },
}

#[derive(Debug)]
struct Node {
    kind: Kind,
    outputs: Vec<u32>,
}

/// The graph of one `AudioContext`.
#[derive(Debug, Default)]
struct Graph {
    nodes: HashMap<u32, Node>,
    next: u32,
    /// Frames rendered (the audio clock).
    frames: u64,
    rate: f64,
    running: bool,
    /// Oscillators that stopped since the script last asked (for `ended`).
    ended: Vec<u32>,
}

impl Graph {
    fn add(&mut self, kind: Kind) -> u32 {
        self.next += 1;
        self.nodes.insert(self.next, Node { kind, outputs: Vec::new() });
        self.next
    }

    /// Renders `out.len()` frames (mono) and advances the clock.
    fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        if !self.running {
            return;
        }
        let rate = self.rate;
        let t0 = self.frames as f64 / rate;
        let n = out.len();
        // Inputs of every node, then each node's samples, sources first.
        let mut inputs: HashMap<u32, Vec<u32>> = HashMap::new();
        for (&id, node) in &self.nodes {
            for &o in &node.outputs {
                inputs.entry(o).or_default().push(id);
            }
        }
        let mut done: HashMap<u32, Vec<f32>> = HashMap::new();
        let ids: Vec<u32> = self.nodes.keys().copied().collect();
        let mut ended = Vec::new();
        for &id in &ids {
            if let Some(Kind::Oscillator { wave, frequency, detune, start, stop, phase }) = self.nodes.get_mut(&id).map(|n| &mut n.kind) {
                let mut buf = vec![0.0f32; n];
                for (i, s) in buf.iter_mut().enumerate() {
                    let t = t0 + i as f64 / rate;
                    if t < *start || t >= *stop {
                        continue;
                    }
                    let f = frequency.at(t) * 2f64.powf(detune.at(t) / 1200.0);
                    let p = *phase;
                    *s = match wave {
                        Wave::Sine => (p * std::f64::consts::TAU).sin(),
                        Wave::Square => if p < 0.5 { 1.0 } else { -1.0 },
                        Wave::Sawtooth => 2.0 * p - 1.0,
                        Wave::Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
                    } as f32;
                    *phase = (p + f / rate).rem_euclid(1.0);
                }
                if t0 + n as f64 / rate >= *stop {
                    ended.push(id);
                }
                done.insert(id, buf);
            }
        }
        // Gain nodes and the destination, after their inputs (the graph is
        // acyclic: a cycle is cut where it closes).
        /// The block being rendered: its length, start time and rate.
        struct Block {
            n: usize,
            t0: f64,
            rate: f64,
        }
        fn eval(id: u32, g: &Graph, inputs: &HashMap<u32, Vec<u32>>, done: &mut HashMap<u32, Vec<f32>>, b: &Block, depth: usize) -> Vec<f32> {
            let Block { n, t0, rate } = *b;
            if let Some(b) = done.get(&id) {
                return b.clone();
            }
            let mut sum = vec![0.0f32; n];
            if depth < 64 {
                for &i in inputs.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
                    let b = eval(i, g, inputs, done, b, depth + 1);
                    for (s, x) in sum.iter_mut().zip(b) {
                        *s += x;
                    }
                }
            }
            if let Some(Node { kind: Kind::Gain { gain }, .. }) = g.nodes.get(&id) {
                for (i, s) in sum.iter_mut().enumerate() {
                    *s *= gain.at(t0 + i as f64 / rate) as f32;
                }
            }
            done.insert(id, sum.clone());
            sum
        }
        for &id in &ids {
            if matches!(self.nodes[&id].kind, Kind::Destination) {
                let b = eval(id, self, &inputs, &mut done, &Block { n, t0, rate }, 0);
                for (o, x) in out.iter_mut().zip(b) {
                    *o += x;
                }
            }
        }
        // Stopped oscillators leave the graph.
        for id in ended {
            self.nodes.remove(&id);
            self.ended.push(id);
        }
        self.frames += n as u64;
    }
}

/// The audio contexts of a document (they stop when it goes away).
#[derive(Default)]
pub struct Engine {
    contexts: HashMap<u32, Context>,
    next: u32,
}

struct Context {
    graph: Arc<Mutex<Graph>>,
    /// The output stream (kept alive), if there is an audio device.
    _output: Option<Box<dyn std::any::Any>>,
    /// Without an output: the clock follows the wall clock (ms when it last
    /// started running, frames then).
    wall: Option<(f64, u64)>,
}

impl Engine {
    /// A script's Web Audio command (fields separated by U+0001); returns a
    /// number (a new id, the current time) or NaN. `now` is the browser
    /// clock in ms; `activated`: the user interacted with the page.
    pub fn command(&mut self, cmd: &str, now: f64, activated: bool) -> f64 {
        if std::env::var_os("RUBROWSER_AUDIO").is_some() && !cmd.starts_with("time") && !cmd.starts_with("ended") {
            eprintln!("[audio] {}", cmd.replace('\u{1}', " "));
        }
        let a: Vec<&str> = cmd.split('\u{1}').collect();
        let num = |i: usize| a.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let id = |i: usize| num(i) as u32;
        if a[0] == "context" {
            let graph = Arc::new(Mutex::new(Graph { rate: 48000.0, ..Default::default() }));
            let output = output(graph.clone());
            if let Some((_, rate)) = &output {
                graph.lock().unwrap().rate = *rate;
            }
            let mut g = graph.lock().unwrap();
            g.add(Kind::Destination); // node 1
            g.running = activated;
            drop(g);
            self.next += 1;
            let wall = output.is_none().then_some((now, 0));
            self.contexts.insert(self.next, Context { graph, _output: output.map(|(o, _)| o), wall });
            return self.next as f64;
        }
        let Some(ctx) = self.contexts.get_mut(&id(1)) else { return f64::NAN };
        // Without a device, render silence to move the clock along.
        if let Some((since, frames)) = ctx.wall {
            let mut g = ctx.graph.lock().unwrap();
            if g.running {
                let due = frames + ((now - since).max(0.0) / 1000.0 * g.rate) as u64;
                while g.frames < due {
                    let n = ((due - g.frames) as usize).min(4096);
                    let mut buf = vec![0.0; n];
                    g.render(&mut buf);
                }
            }
        }
        let mut g = ctx.graph.lock().unwrap();
        match a[0] {
            "time" => g.frames as f64 / g.rate,
            "rate" => g.rate,
            "running" => g.running as u8 as f64,
            "resume" | "suspend" => {
                g.running = a[0] == "resume";
                if let Some(w) = ctx.wall.as_mut() {
                    *w = (now, g.frames);
                }
                g.running as u8 as f64
            }
            "close" => {
                drop(g);
                self.contexts.remove(&id(1));
                0.0
            }
            "oscillator" => g.add(Kind::Oscillator {
                wave: Wave::Sine,
                frequency: Param::new(440.0),
                detune: Param::new(0.0),
                start: f64::INFINITY,
                stop: f64::INFINITY,
                phase: 0.0,
            }) as f64,
            "gain" => g.add(Kind::Gain { gain: Param::new(1.0) }) as f64,
            "connect" => {
                if let (true, Some(n)) = (g.nodes.contains_key(&id(3)), g.nodes.get_mut(&id(2)))
                    && !n.outputs.contains(&id(3))
                {
                    n.outputs.push(id(3));
                }
                0.0
            }
            "disconnect" => {
                if let Some(n) = g.nodes.get_mut(&id(2)) {
                    match a.get(3).filter(|s| !s.is_empty()) {
                        Some(_) => n.outputs.retain(|&o| o != id(3)),
                        None => n.outputs.clear(),
                    }
                }
                0.0
            }
            "type" => {
                if let Some(Node { kind: Kind::Oscillator { wave, .. }, .. }) = g.nodes.get_mut(&id(2)) {
                    *wave = match a.get(3).copied().unwrap_or("") {
                        "square" => Wave::Square,
                        "sawtooth" => Wave::Sawtooth,
                        "triangle" => Wave::Triangle,
                        _ => Wave::Sine,
                    };
                }
                0.0
            }
            "start" | "stop" => {
                let now_t = g.frames as f64 / g.rate;
                if let Some(Node { kind: Kind::Oscillator { start, stop, .. }, .. }) = g.nodes.get_mut(&id(2)) {
                    let when = num(3).max(now_t);
                    if a[0] == "start" {
                        *start = when;
                    } else {
                        *stop = when;
                    }
                }
                0.0
            }
            // param␁ctx␁node␁name␁op␁value␁time␁timeConstant
            "param" => {
                let name = a.get(3).copied().unwrap_or("");
                let param = match g.nodes.get_mut(&id(2)).map(|n| &mut n.kind) {
                    Some(Kind::Gain { gain }) if name == "gain" => Some(gain),
                    Some(Kind::Oscillator { frequency, .. }) if name == "frequency" => Some(frequency),
                    Some(Kind::Oscillator { detune, .. }) if name == "detune" => Some(detune),
                    _ => None,
                };
                if let Some(p) = param {
                    p.command(a.get(4).copied().unwrap_or(""), num(5), num(6), num(7));
                }
                0.0
            }
            // param-value␁ctx␁node␁name: the parameter's value now
            "param-value" => {
                let t = g.frames as f64 / g.rate;
                match (g.nodes.get(&id(2)).map(|n| &n.kind), a.get(3).copied().unwrap_or("")) {
                    (Some(Kind::Gain { gain }), "gain") => gain.at(t),
                    (Some(Kind::Oscillator { frequency, .. }), "frequency") => frequency.at(t),
                    (Some(Kind::Oscillator { detune, .. }), "detune") => detune.at(t),
                    _ => f64::NAN,
                }
            }
            // the next stopped oscillator (0: none)
            "ended" => {
                if g.ended.is_empty() { 0.0 } else { g.ended.remove(0) as f64 }
            }
            _ => f64::NAN,
        }
    }

    /// Whether any context is producing sound or waiting to (the page then
    /// needs updates for `currentTime` and `ended`).
    pub fn active(&self) -> bool {
        self.contexts.values().any(|c| {
            let g = c.graph.lock().unwrap();
            g.running && g.nodes.values().any(|n| matches!(n.kind, Kind::Oscillator { .. }))
        })
    }
}

/// The default audio output playing `graph` (mono, copied to every
/// channel), and its sample rate.
#[cfg(feature = "media")]
fn output(graph: Arc<Mutex<Graph>>) -> Option<(Box<dyn std::any::Any>, f64)> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;
    let rate = config.sample_rate().0 as f64;
    let channels = config.channels() as usize;
    if config.sample_format() != cpal::SampleFormat::F32 {
        return None;
    }
    let config: cpal::StreamConfig = config.into();
    let mut mono = Vec::new();
    let stream = device
        .build_output_stream(
            &config,
            move |d: &mut [f32], _| {
                let frames = d.len() / channels.max(1);
                mono.resize(frames, 0.0);
                if let Ok(mut g) = graph.lock() {
                    g.render(&mut mono);
                }
                for (frame, s) in d.chunks_mut(channels.max(1)).zip(&mono) {
                    frame.fill(s.clamp(-1.0, 1.0));
                }
            },
            |_| {},
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some((Box::new(stream), rate))
}

#[cfg(not(feature = "media"))]
fn output(_: Arc<Mutex<Graph>>) -> Option<(Box<dyn std::any::Any>, f64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_follows_the_spec() {
        let mut p = Param::new(1.0);
        p.command("set", 0.0, 1.0, 0.0);
        p.command("linear", 1.0, 2.0, 0.0);
        p.command("exponential", 0.01, 4.0, 0.0);
        assert_eq!(p.at(0.5), 1.0, "before the first event: the value");
        assert_eq!(p.at(1.0), 0.0);
        assert!((p.at(1.5) - 0.5).abs() < 1e-9, "halfway up the linear ramp");
        // exponential ramp from 1 to 0.01 over 2 s: 0.1 at the middle
        assert!((p.at(3.0) - 0.1).abs() < 1e-9, "{}", p.at(3.0));
        assert_eq!(p.at(9.0), 0.01, "after the last event: its value");
        p.command("cancel", 0.0, 2.0, 0.0);
        assert_eq!(p.at(9.0), 0.0, "cancel removes the events at and after 2 s");
        let mut q = Param::new(0.0);
        q.command("set", 1.0, 0.0, 0.0);
        q.command("target", 0.0, 1.0, 0.5);
        assert!((q.at(1.5) - (-1.0f64).exp()).abs() < 1e-9, "setTargetAtTime: one time constant later");
    }

    #[test]
    fn oscillator_through_a_gain_reaches_the_destination() {
        let mut e = Engine::default();
        let c = e.command("context", 0.0, true) as u32;
        let cmd = |e: &mut Engine, s: String| e.command(&s, 0.0, true);
        let osc = cmd(&mut e, format!("oscillator\u{1}{c}")) as u32;
        let gain = cmd(&mut e, format!("gain\u{1}{c}")) as u32;
        cmd(&mut e, format!("type\u{1}{c}\u{1}{osc}\u{1}square"));
        cmd(&mut e, format!("param\u{1}{c}\u{1}{gain}\u{1}gain\u{1}value\u{1}0.25"));
        cmd(&mut e, format!("param\u{1}{c}\u{1}{osc}\u{1}frequency\u{1}value\u{1}1"));
        cmd(&mut e, format!("connect\u{1}{c}\u{1}{osc}\u{1}{gain}"));
        cmd(&mut e, format!("connect\u{1}{c}\u{1}{gain}\u{1}1"));
        cmd(&mut e, format!("start\u{1}{c}\u{1}{osc}\u{1}0"));
        cmd(&mut e, format!("stop\u{1}{c}\u{1}{osc}\u{1}0.01"));
        let g = e.contexts[&c].graph.clone();
        let mut g = g.lock().unwrap();
        g.rate = 1000.0;
        let mut buf = vec![0.0; 20];
        g.render(&mut buf);
        assert_eq!(&buf[..3], &[0.25, 0.25, 0.25], "a square wave at a quarter");
        assert_eq!(buf[15], 0.0, "stopped at 10 ms");
        assert!(!g.nodes.contains_key(&osc) && g.ended == vec![osc], "the stopped oscillator leaves the graph");
    }
}

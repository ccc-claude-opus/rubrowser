//! The FFmpeg playback backend (feature `media`): a decoder thread per
//! player reads the resource (FFmpeg opens files and http(s) URLs itself),
//! sends video frames through a small bounded queue and puts resampled
//! audio into a buffer that a `cpal` output stream plays.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

use super::{Frame, Info, Player};

/// Video frames wider than this are scaled down when decoded.
const MAX_WIDTH: u32 = 1280;
/// Decoded audio kept ahead of playback (seconds).
const AUDIO_AHEAD: f32 = 1.0;

/// What the decoder thread reports.
enum Msg {
    Meta {
        duration: f64,
        width: u32,
        height: u32,
    },
    Frame {
        generation: u64,
        pts: f64,
        frame: Frame,
    },
    /// The end of the resource (of `generation`).
    End {
        generation: u64,
    },
    Error(u8),
}

/// Requests to the decoder thread.
enum Cmd {
    Seek { seconds: f64, generation: u64 },
    Quit,
}

/// Audio shared between the decoder thread and the output stream.
struct Shared {
    samples: Mutex<VecDeque<f32>>,
    paused: AtomicBool,
    /// `f32` bits: volume × (not muted).
    gain: AtomicU32,
}

struct Output {
    _stream: cpal::Stream,
    rate: u32,
    channels: u16,
}

/// Opens the system's default audio output, playing from `shared`.
fn output(shared: Arc<Shared>) -> Option<Output> {
    let device = cpal::default_host().default_output_device()?;
    let config = device.default_output_config().ok()?;
    let (rate, channels) = (config.sample_rate().0, config.channels());
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let fill = move |out: &mut [f32]| {
        let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
        if shared.paused.load(Ordering::Relaxed) {
            out.fill(0.0);
            return;
        }
        let mut q = shared.samples.lock().unwrap();
        for s in out.iter_mut() {
            *s = q.pop_front().unwrap_or(0.0) * gain;
        }
    };
    let err = |_| {};
    let stream = match format {
        cpal::SampleFormat::F32 => device
            .build_output_stream(&config, move |d: &mut [f32], _| fill(d), err, None)
            .ok()?,
        cpal::SampleFormat::I16 => {
            let mut buf = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |d: &mut [i16], _| {
                        buf.resize(d.len(), 0.0);
                        fill(&mut buf);
                        for (o, s) in d.iter_mut().zip(&buf) {
                            *o = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        }
                    },
                    err,
                    None,
                )
                .ok()?
        }
        _ => return None,
    };
    stream.play().ok()?;
    Some(Output {
        _stream: stream,
        rate,
        channels,
    })
}

pub fn open(url: &str) -> Option<Box<dyn Player>> {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Quiet);
    });
    let path = url
        .strip_prefix("file://")
        .map(|p| p.split(['?', '#']).next().unwrap_or(p).to_string())
        .unwrap_or_else(|| url.to_string());
    let shared = Arc::new(Shared {
        samples: Mutex::new(VecDeque::new()),
        paused: AtomicBool::new(true),
        gain: AtomicU32::new(1.0f32.to_bits()),
    });
    let audio = output(shared.clone());
    let format = audio.as_ref().map(|o| (o.rate, o.channels));
    let (tx, rx) = mpsc::sync_channel(8);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let thread_shared = shared.clone();
    std::thread::Builder::new()
        .name("media".into())
        .spawn(move || decode(&path, format, thread_shared, tx, cmd_rx))
        .ok()?;
    Some(Box::new(FfPlayer {
        rx,
        cmd: cmd_tx,
        shared,
        _audio: audio,
        generation: 0,
        info: Info {
            duration: f64::NAN,
            paused: true,
            ..Info::default()
        },
        base: 0.0,
        since: None,
        queue: VecDeque::new(),
        current: None,
        ended_stream: false,
        looping: false,
        last_update: None,
        events: Vec::new(),
    }))
}

/// The decoder thread: decodes `path` until told to quit.
fn decode(
    path: &str,
    audio_format: Option<(u32, u16)>,
    shared: Arc<Shared>,
    tx: SyncSender<Msg>,
    cmds: Receiver<Cmd>,
) {
    let Ok(mut input) = ff::format::input(&path) else {
        let _ = tx.send(Msg::Error(2));
        return;
    };
    let video = input.streams().best(ff::media::Type::Video).map(|s| {
        let tb = s.time_base();
        (s.index(), f64::from(tb), s.parameters())
    });
    let audio = input.streams().best(ff::media::Type::Audio).map(|s| {
        let tb = s.time_base();
        (s.index(), f64::from(tb), s.parameters())
    });
    let mut vdec = video.as_ref().and_then(|(_, _, p)| {
        ff::codec::context::Context::from_parameters(p.clone())
            .ok()?
            .decoder()
            .video()
            .ok()
    });
    let mut adec = audio.as_ref().and_then(|(_, _, p)| {
        ff::codec::context::Context::from_parameters(p.clone())
            .ok()?
            .decoder()
            .audio()
            .ok()
    });
    if vdec.is_none() && adec.is_none() {
        let _ = tx.send(Msg::Error(4));
        return;
    }
    let (vw, vh) = vdec.as_ref().map_or((0, 0), |d| (d.width(), d.height()));
    let (ow, oh) = if vw > MAX_WIDTH {
        (
            MAX_WIDTH,
            (vh as u64 * MAX_WIDTH as u64 / vw as u64).max(1) as u32 & !1,
        )
    } else {
        (vw, vh)
    };
    let duration = if input.duration() > 0 {
        input.duration() as f64 / ff::ffi::AV_TIME_BASE as f64
    } else {
        f64::INFINITY
    };
    if tx
        .send(Msg::Meta {
            duration,
            width: ow,
            height: oh,
        })
        .is_err()
    {
        return;
    }
    let mut scaler: Option<ff::software::scaling::Context> = None;
    let mut resampler: Option<ff::software::resampling::Context> = None;
    let mut generation = 0u64;
    // After a seek: frames before the target are decoded but not used.
    let mut skip_until = 0.0f64;
    let mut vframe = ff::frame::Video::empty();
    let mut aframe = ff::frame::Audio::empty();
    loop {
        // Requests first.
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Seek {
                    seconds,
                    generation: g,
                }) => {
                    generation = g;
                    skip_until = seconds;
                    let ts = (seconds * ff::ffi::AV_TIME_BASE as f64) as i64;
                    let _ = input.seek(ts, ..ts);
                    if let Some(d) = vdec.as_mut() {
                        d.flush();
                    }
                    if let Some(d) = adec.as_mut() {
                        d.flush();
                    }
                    shared.samples.lock().unwrap().clear();
                }
                Ok(Cmd::Quit) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        // Enough audio decoded ahead: wait for playback to use some.
        if let Some((rate, ch)) = audio_format
            && adec.is_some()
        {
            let ahead = shared.samples.lock().unwrap().len() as f32 / (rate as f32 * ch as f32);
            if ahead > AUDIO_AHEAD {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        }
        let mut packet = ff::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) => {}
            Err(ff::Error::Eof) => {
                if tx.send(Msg::End { generation }).is_err() {
                    return;
                }
                // Wait for a seek (looping) or the end.
                match cmds.recv() {
                    Ok(Cmd::Seek {
                        seconds,
                        generation: g,
                    }) => {
                        generation = g;
                        skip_until = seconds;
                        let ts = (seconds * ff::ffi::AV_TIME_BASE as f64) as i64;
                        let _ = input.seek(ts, ..ts);
                        if let Some(d) = vdec.as_mut() {
                            d.flush();
                        }
                        if let Some(d) = adec.as_mut() {
                            d.flush();
                        }
                        shared.samples.lock().unwrap().clear();
                        continue;
                    }
                    _ => return,
                }
            }
            Err(_) => continue,
        }
        let index = packet.stream();
        if let (Some((vi, tb, _)), Some(dec)) = (video.as_ref(), vdec.as_mut())
            && index == *vi
        {
            if dec.send_packet(&packet).is_err() {
                continue;
            }
            while dec.receive_frame(&mut vframe).is_ok() {
                let pts = vframe.timestamp().unwrap_or(0) as f64 * tb;
                if pts < skip_until - 0.001 {
                    continue;
                }
                let sc = scaler.get_or_insert_with(|| {
                    ff::software::scaling::Context::get(
                        vframe.format(),
                        vframe.width(),
                        vframe.height(),
                        ff::format::Pixel::BGRA,
                        ow,
                        oh,
                        ff::software::scaling::Flags::BILINEAR,
                    )
                    .expect("scaler")
                });
                let mut out = ff::frame::Video::empty();
                if sc.run(&vframe, &mut out).is_err() {
                    continue;
                }
                let stride = out.stride(0);
                let data = out.data(0);
                let mut rgb = Vec::with_capacity((ow * oh) as usize);
                for row in 0..oh as usize {
                    let line = &data[row * stride..row * stride + ow as usize * 4];
                    rgb.extend(
                        line.chunks_exact(4)
                            .map(|p| (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32),
                    );
                }
                let frame = Frame {
                    width: ow,
                    height: oh,
                    rgb,
                };
                if tx
                    .send(Msg::Frame {
                        generation,
                        pts,
                        frame,
                    })
                    .is_err()
                {
                    return;
                }
            }
        } else if let (Some((ai, atb, _)), Some(dec), Some((rate, channels))) =
            (audio.as_ref(), adec.as_mut(), audio_format)
            && index == *ai
        {
            if dec.send_packet(&packet).is_err() {
                continue;
            }
            while dec.receive_frame(&mut aframe).is_ok() {
                let start = aframe.timestamp().unwrap_or(0) as f64 * atb;
                let length = aframe.samples() as f64 / aframe.rate().max(1) as f64;
                if start + length <= skip_until {
                    continue;
                }
                let layout = if aframe.channel_layout().channels() > 0 {
                    aframe.channel_layout()
                } else {
                    ff::ChannelLayout::default(aframe.channels() as i32)
                };
                aframe.set_channel_layout(layout);
                let rs = resampler.get_or_insert_with(|| {
                    ff::software::resampling::Context::get(
                        aframe.format(),
                        layout,
                        aframe.rate(),
                        ff::format::Sample::F32(ff::format::sample::Type::Packed),
                        ff::ChannelLayout::default(channels as i32),
                        rate,
                    )
                    .expect("resampler")
                });
                let mut out = ff::frame::Audio::empty();
                if rs.run(&aframe, &mut out).is_err() {
                    continue;
                }
                let n = out.samples() * channels as usize;
                let bytes = &out.data(0)[..n * 4];
                let mut q = shared.samples.lock().unwrap();
                q.extend(
                    bytes
                        .chunks_exact(4)
                        .map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])),
                );
            }
        }
    }
}

struct FfPlayer {
    rx: Receiver<Msg>,
    cmd: Sender<Cmd>,
    shared: Arc<Shared>,
    _audio: Option<Output>,
    /// Bumped by each seek; frames of older ones are dropped.
    generation: u64,
    info: Info,
    /// The media time when the clock last started (or stopped).
    base: f64,
    /// When it started, if playing.
    since: Option<Instant>,
    /// Decoded frames not shown yet, by presentation time.
    queue: VecDeque<(f64, Rc<Frame>)>,
    current: Option<Rc<Frame>>,
    /// The decoder reached the end (of the current generation).
    ended_stream: bool,
    looping: bool,
    /// When `timeupdate` last fired.
    last_update: Option<Instant>,
    events: Vec<&'static str>,
}

impl FfPlayer {
    fn clock(&self) -> f64 {
        self.base + self.since.map_or(0.0, |s| s.elapsed().as_secs_f64())
    }

    fn stop_clock(&mut self) {
        self.base = self.clock();
        self.since = None;
    }
}

impl Drop for FfPlayer {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        // Unblock a decoder waiting to send.
        while self.rx.try_recv().is_ok() {}
    }
}

impl Player for FfPlayer {
    fn play(&mut self) {
        if self.info.ended {
            self.seek(0.0);
            self.info.ended = false;
        }
        if self.info.paused {
            self.info.paused = false;
            self.since = Some(Instant::now());
            self.shared.paused.store(false, Ordering::Relaxed);
            if self.info.ready >= 2 {
                self.events.push("playing");
            }
        }
    }

    fn pause(&mut self) {
        if !self.info.paused {
            self.stop_clock();
            self.info.paused = true;
            self.shared.paused.store(true, Ordering::Relaxed);
        }
    }

    fn seek(&mut self, seconds: f64) {
        let t = if self.info.duration.is_finite() {
            seconds.min(self.info.duration)
        } else {
            seconds
        };
        self.generation += 1;
        let _ = self.cmd.send(Cmd::Seek {
            seconds: t,
            generation: self.generation,
        });
        self.queue.clear();
        self.ended_stream = false;
        self.info.ended = false;
        self.base = t;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }

    fn set_volume(&mut self, volume: f32, muted: bool) {
        let gain = if muted { 0.0 } else { volume.clamp(0.0, 1.0) };
        self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn set_loop(&mut self, looping: bool) {
        self.looping = looping;
    }

    fn update(&mut self) -> Vec<&'static str> {
        // Take what the decoder has (keeping a few frames buffered here so
        // it can work ahead).
        while self.queue.len() < 4 {
            match self.rx.try_recv() {
                Ok(Msg::Meta {
                    duration,
                    width,
                    height,
                }) => {
                    self.info.duration = duration;
                    self.info.width = width;
                    self.info.height = height;
                    self.info.ready = self.info.ready.max(1);
                    self.events.extend(["durationchange", "loadedmetadata"]);
                    if width == 0 {
                        // Audio only: playable from here.
                        self.info.ready = 4;
                        self.events
                            .extend(["loadeddata", "canplay", "canplaythrough"]);
                    }
                }
                Ok(Msg::Frame {
                    generation,
                    pts,
                    frame,
                }) => {
                    if generation == self.generation {
                        self.queue.push_back((pts, Rc::new(frame)));
                    }
                }
                Ok(Msg::End { generation }) => {
                    if generation == self.generation {
                        self.ended_stream = true;
                    }
                }
                Ok(Msg::Error(code)) => {
                    self.info.error = Some(code);
                    self.events.push("error");
                }
                Err(_) => break,
            }
        }
        let now = self.clock();
        // The frame due now (the first one even before playing).
        while let Some((pts, _)) = self.queue.front() {
            if *pts > now && self.current.is_some() {
                break;
            }
            let (_, f) = self.queue.pop_front().unwrap();
            self.current = Some(f);
            if self.info.ready < 2 {
                self.info.ready = 4;
                self.events
                    .extend(["loadeddata", "canplay", "canplaythrough"]);
                if !self.info.paused {
                    self.events.push("playing");
                }
            }
        }
        if !self.info.paused {
            self.info.current_time = if self.info.duration.is_finite() {
                now.min(self.info.duration)
            } else {
                now
            };
            let due = self
                .last_update
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
            if due {
                self.last_update = Some(Instant::now());
                self.events.push("timeupdate");
            }
            // The end: no more frames and the time has run out.
            let audio_left = !self.shared.samples.lock().unwrap().is_empty();
            let past_end = !self.info.duration.is_finite() || now >= self.info.duration - 0.05;
            if self.ended_stream && self.queue.is_empty() && (!audio_left || past_end) {
                if self.looping {
                    self.seek(0.0);
                } else {
                    self.stop_clock();
                    if self.info.duration.is_finite() {
                        self.base = self.info.duration;
                        self.info.current_time = self.info.duration;
                    }
                    self.info.paused = true;
                    self.info.ended = true;
                    self.shared.paused.store(true, Ordering::Relaxed);
                    self.events.extend(["timeupdate", "pause", "ended"]);
                }
            }
        } else {
            self.info.current_time = self.base;
        }
        std::mem::take(&mut self.events)
    }

    fn info(&self) -> Info {
        self.info.clone()
    }

    fn frame(&self) -> Option<Rc<Frame>> {
        self.current.clone()
    }
}

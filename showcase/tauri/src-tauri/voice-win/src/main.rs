// Voice helper for Roamies on Windows and Linux: the twin of voice/main.swift, with the same JSON lines in and out.
// Watches the talk chord (GetAsyncKeyState on Windows, the X server on Linux X11 sessions; neither needs a
// permission), records the default microphone and
// transcribes on this PC with NVIDIA Parakeet TDT 0.6B v3 (int8 ONNX via parakeet-rs). Parakeet isn't a
// streaming model, so live text comes from re-reading the take about once a second.
// Hold the chord = push-to-talk (release sends). Quick tap = hands-free (ends on a pause or another tap).
// While the chord is held, moving the cursor round something circles it (ink.rs); the circled area goes out with
// what was said, as on macOS (Windows only). On Wayland no app can watch the keyboard, so there {"cmd":"toggle"}
// (sent for `roamies --talk`, which people bind to a key) starts a take, and sends it when it comes again or on a
// pause. Exits when stdin closes.
// With the wake word on ({"cmd":"wake","on":true,"words":[…]}), it also listens quietly while idle: each burst of
// speech is read by Parakeet on this PC, and one of the wake phrases at its start turns it into a hands-free take
// (wake.rs). Held off while Roamies answers ({"cmd":"wake_hold"}), so it can't wake itself.
// With "send when I pause" on ({"cmd":"pause_sends"}), a held take also sends once the talking stops.
// End words ({"cmd":"end_words","words":["over","go"]}): one said last sends the take at once, without it.
// Cancel words ({"cmd":"cancel_words","words":["cancel","never mind"]}): one said last drops the whole take, nothing sent.
// {"cmd":"wake_pause","on":true} pauses wake listening until the talk chord is pressed again ({"type":"wake_paused"}
// tells the app either way), or until {"cmd":"wake_pause","on":false}.
// {"cmd":"listen","quiet":3} listens hands-free and lets go after 3 s with nothing said (listening after a reply).
//
// roamies-voice --model <dir> [--chord control,option]   normal use (Roamies starts it)
// roamies-voice --perms                                   prints {"type":"perms","mic":…}
// roamies-voice --devices                                 prints {"type":"devices",…}: every mic, the default, formats
// roamies-voice --model <dir> --transcribe <wav>          dev: transcribe a file and exit
// roamies-voice --model <dir> --test-audio <wav>          dev: a file stands in for the mic ({"cmd":"listen"} starts)

mod audio;
mod ink;
mod keys;
mod wake;

use audio::{MicError, Sink, Source};
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const RATE: u32 = 16_000;

pub fn emit(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn log(s: &str) {
    eprintln!("[voice] {s}");
}

/// The error the app shows when the microphone can't be used; `reason` picks what the Roamie says.
fn mic_error(e: &MicError) -> Value {
    json!({ "type": "error", "code": "no_input", "reason": e.reason, "message": e.detail })
}

pub enum Ev {
    Cmd(Value),
    StdinClosed,
    Level(f64),
    AudioFailed(MicError),
    Text { take: u64, text: String, last: bool },
    EngineFailed(String),
}

// ---------- transcription (its own thread: loading and reading never stall the hotkey loop) ----------

struct Job {
    take: u64,
    audio: Vec<f32>,
    last: bool,
}

fn model_present(dir: &Path) -> bool {
    dir.join("vocab.txt").is_file() && dir.join("encoder-model.int8.onnx").is_file() && dir.join("decoder_joint-model.int8.onnx").is_file()
}

fn load_model(dir: &Path) -> Result<ParakeetTDT, String> {
    // Half the cores: quick enough to read a minute of speech in a second or two, without starving the PC.
    let threads = thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(2).clamp(1, 8);
    let t0 = Instant::now();
    let env = parakeet_rs::ort::init().with_name("roamies-voice").build().map_err(|e| e.to_string())?;
    let config = ExecutionConfig::new().with_environment(env).with_intra_threads(threads);
    let mut asr = ParakeetTDT::from_pretrained(dir, Some(config)).map_err(|e| e.to_string())?;
    // One short warm-up run, so the first real request doesn't pay for ONNX Runtime's setup.
    let _ = asr.transcribe_samples(vec![0.0; RATE as usize / 2], RATE, 1, None);
    log(&format!("model ready in {:.1}s ({threads} threads)", t0.elapsed().as_secs_f64()));
    Ok(asr)
}

fn transcribe(asr: &mut ParakeetTDT, audio: Vec<f32>) -> String {
    if audio.len() < RATE as usize / 5 {
        return String::new();
    }
    match asr.transcribe_samples(audio, RATE, 1, None) {
        Ok(r) => r.text.split_whitespace().collect::<Vec<_>>().join(" "),
        Err(e) => {
            log(&format!("transcribe: {e}"));
            String::new()
        }
    }
}

fn spawn_engine(dir: PathBuf, tx: Sender<Ev>) -> (Sender<Job>, thread::JoinHandle<()>) {
    let (jtx, jrx) = mpsc::channel::<Job>();
    let handle = thread::spawn(move || {
        let mut asr = match load_model(&dir) {
            Ok(a) => a,
            Err(e) => {
                log(&format!("model failed to load: {e}"));
                let _ = tx.send(Ev::EngineFailed(e));
                return;
            }
        };
        while let Ok(first) = jrx.recv() {
            // Only the newest live read matters; a closing read always runs.
            let mut jobs = vec![first];
            jobs.extend(jrx.try_iter());
            let job = match jobs.iter().rposition(|j| j.last) {
                Some(i) => jobs.swap_remove(i),
                None => jobs.pop().expect("at least one job"),
            };
            let text = transcribe(&mut asr, job.audio);
            if tx.send(Ev::Text { take: job.take, text, last: job.last }).is_err() {
                return;
            }
        }
    });
    (jtx, handle)
}

// ---------- the talk session (port of the Voice class in voice/main.swift) ----------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    // Listening quietly for a wake phrase (no pill, no level); hearing one turns into a hands-free take.
    Wake,
    Armed,
    Holding,
    HandsFree,
    Finishing,
}

const TAP_LIMIT: f64 = 0.25;
// A release must last this long to count, so one glitchy frame of key state can't end a session.
const RELEASE_DEBOUNCE: f64 = 0.09;
const PAUSE_ENDS: f64 = 1.5;
const MAX_SESSION: f64 = 60.0;
// Keep the mic open a moment after release so a word said while letting go isn't clipped.
const TAIL: f64 = 0.3;
const FINAL_TIMEOUT: f64 = 30.0;
const READ_EVERY: f64 = 0.8;
// Parakeet makes up words ("Thank you.") from silence, so a take is only read once the mic heard a voice:
// about -49 dBFS, above room noise and below quiet speech.
const SPEECH_LEVEL: f64 = 0.15;
// Words stopped changing this long, and a read since then heard no more: the talking is over even if the room is
// loud enough to keep the level up (a fan, music, a busy office).
const STILL_ENDS: f64 = 2.2;
// Wake listening: the sound kept from before the speech starts (so the phrase's first sound isn't cut), how long a
// pause ends a burst that had no phrase in it, the longest burst read, and the rest after each take.
const WAKE_PREROLL: f64 = 0.6;
const WAKE_PAUSE: f64 = 1.2;
const WAKE_MAX: f64 = 6.0;
const WAKE_COOLDOWN: f64 = 2.0;
// Hands-free with nothing said: let go after this long (unless asked for less), and never later than this past it
// even if the room stays loud.
const NOTHING_HEARD: f64 = 8.0;
const LOUD_GRACE: f64 = 5.0;

struct Voice {
    phase: Phase,
    chord: u8,
    chord_at: f64,
    listen_at: f64,
    heard_at: f64,
    voice_at: f64,
    level_at: f64,
    released_at: Option<f64>,
    blocked: bool,
    prev_chord: bool,
    report_empty: bool,
    spoke: bool,
    best: String,
    // Each session gets a new take number; results for older takes are ignored.
    take: u64,
    buf: Arc<Mutex<Vec<f32>>>,
    source: Option<Source>,
    engine: Option<Sender<Job>>,
    engine_error: Option<String>,
    reading: bool,
    read_at: f64,
    read_len: usize,
    tail_until: Option<f64>,
    final_until: Option<f64>,
    test_audio: Option<PathBuf>,
    ink: ink::Ink,
    tx: Sender<Ev>,
    t0: Instant,
    // When the last live read came back (pause detection in a loud room).
    read_done_at: f64,
    // A held take sends on a pause too.
    pause_sends: bool,
    // Wake word: on in settings, its phrases, held off while Roamies answers, and resting after a take or a failure.
    wake_on: bool,
    wake_phrases: Vec<wake::Phrase>,
    wake_hold: bool,
    wake_cooldown_until: f64,
    wake_retry_at: f64,
    // The take began with a wake phrase: it's taken off what was said.
    woke: bool,
    wake_text: String,
    // Words that, said last, send the take straight away (lowercase, letters and digits only, per word).
    end_words: Vec<Vec<String>>,
    ended_by_word: bool,
    // Words that, said last, drop the take: nothing is sent.
    cancel_words: Vec<Vec<String>>,
    // A hands-free take with nothing said ends after this long (shorter when listening after a reply).
    quiet_limit: f64,
    // Wake listening paused by the user ("pause voice detection", or the tray menu); the talk chord resumes it.
    wake_paused: bool,
}

impl Voice {
    fn new(engine: Option<Sender<Job>>, test_audio: Option<PathBuf>, tx: Sender<Ev>) -> Voice {
        Voice {
            phase: Phase::Idle,
            chord: keys::mask(&["control".into(), "option".into()]).expect("default chord"),
            chord_at: 0.0,
            listen_at: 0.0,
            heard_at: 0.0,
            voice_at: 0.0,
            level_at: 0.0,
            released_at: None,
            blocked: false,
            prev_chord: false,
            report_empty: false,
            spoke: false,
            best: String::new(),
            take: 0,
            buf: Arc::new(Mutex::new(Vec::new())),
            source: None,
            engine,
            engine_error: None,
            reading: false,
            read_at: 0.0,
            read_len: 0,
            tail_until: None,
            final_until: None,
            test_audio,
            ink: ink::Ink::default(),
            tx,
            t0: Instant::now(),
            read_done_at: 0.0,
            pause_sends: false,
            wake_on: false,
            wake_phrases: wake::phrases(&["Hey Roamies".to_owned()]),
            wake_hold: false,
            wake_cooldown_until: 0.0,
            wake_retry_at: 0.0,
            woke: false,
            wake_text: String::new(),
            end_words: Vec::new(),
            ended_by_word: false,
            cancel_words: Vec::new(),
            wake_paused: false,
            quiet_limit: NOTHING_HEARD,
        }
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn set_chord(&mut self, names: &[String]) {
        let Some(m) = keys::mask(names) else { return log(&format!("ignored chord {names:?}")) };
        if self.phase == Phase::Wake {
            self.stop_wake();
        } else if self.phase != Phase::Idle {
            self.cancel();
        }
        self.chord = m;
        self.blocked = true;
        log(&format!("talk chord: {}", names.join("+")));
    }

    fn tick(&mut self) {
        let now = self.now();
        self.tick_keys(now);
        self.ink.set_armed(matches!(self.phase, Phase::Armed | Phase::Holding), now);
        self.ink.follow(now);
        match self.phase {
            Phase::Idle if self.wake_allowed(now) => self.start_wake(now),
            Phase::Wake => self.wake_tick(now),
            Phase::Holding | Phase::HandsFree => self.maybe_read(now),
            Phase::Finishing => {
                if self.tail_until.is_some_and(|t| now >= t) {
                    self.tail_until = None;
                    self.close_take(now);
                } else if self.final_until.is_some_and(|t| now >= t) {
                    log("final read timed out");
                    self.finalize();
                }
            }
            _ => {}
        }
    }

    fn tick_keys(&mut self, now: f64) {
        let f = keys::held();
        let held = f.mods & self.chord;
        let down = held == self.chord; // once talking, a stray extra modifier doesn't count as letting go
        let extra = f.mods & !self.chord != 0 || f.altgr;
        let chord = down && !extra;
        let prev = self.prev_chord;
        self.prev_chord = chord;
        if held == 0 {
            self.blocked = false;
        }
        let released = if down {
            self.released_at = None;
            false
        } else {
            let at = *self.released_at.get_or_insert(now);
            now - at >= RELEASE_DEBOUNCE
        };

        match self.phase {
            Phase::Idle | Phase::Wake => {
                if chord && !prev && !self.blocked {
                    self.stop_wake();
                    // Paused voice detection comes back with the talk keys.
                    self.set_wake_paused(false);
                    self.arm(now);
                }
            }
            Phase::Armed => {
                // Only the first moment decides shortcut vs. talk: the chord plus a key or another modifier is a shortcut.
                if down && (extra || keys::other_key_down()) {
                    log("not a talk: the chord was part of a shortcut");
                    self.cancel();
                    self.blocked = true;
                } else if released {
                    self.phase = Phase::HandsFree;
                    self.show_listening("handsfree", now);
                } else if down && now - self.chord_at >= TAP_LIMIT {
                    self.phase = Phase::Holding;
                    self.show_listening("hold", now);
                }
            }
            Phase::Holding => {
                if released {
                    self.end("released", now);
                    self.finish();
                } else if self.pause_sends && self.paused(now) {
                    // Still holding the keys, but done talking: send it now. Letting go later starts nothing new.
                    self.end("pause (keys held)", now);
                    self.finish();
                } else if now - self.listen_at > MAX_SESSION {
                    self.end("max length", now);
                    self.finish();
                }
            }
            Phase::HandsFree => {
                // Pressing again switches to push-to-talk; letting go then sends.
                if chord && !prev && !self.blocked {
                    self.phase = Phase::Holding;
                    self.released_at = None;
                    return;
                }
                if self.paused(now) {
                    self.end("pause", now);
                    self.finish();
                } else if self.best.is_empty()
                    && !self.reading
                    && now - self.listen_at > self.quiet_limit
                    && (now - self.voice_at > PAUSE_ENDS.min(self.quiet_limit / 2.0) || now - self.listen_at > self.quiet_limit + LOUD_GRACE)
                {
                    self.end("nothing heard", now);
                    self.cancel();
                } else if now - self.listen_at > MAX_SESSION {
                    self.end("max length", now);
                    self.finish();
                }
            }
            Phase::Finishing => {}
        }
    }

    fn end(&self, why: &str, now: f64) {
        log(&format!("session end: {why} after {:.1}s", now - self.listen_at));
    }

    /// Done talking: some words came, then either the mic went quiet, or (in a loud room, where the level never
    /// drops) the words stopped changing and a read since then heard nothing new.
    fn paused(&self, now: f64) -> bool {
        if self.best.is_empty() {
            return false;
        }
        let quiet = now - self.heard_at.max(self.voice_at) > PAUSE_ENDS;
        let still = now - self.heard_at > STILL_ENDS && self.read_done_at - self.heard_at > STILL_ENDS - READ_EVERY;
        quiet || still
    }

    fn arm(&mut self, now: f64) {
        self.phase = Phase::Armed;
        self.chord_at = now;
        self.best.clear();
        self.spoke = false;
        self.ended_by_word = false;
        self.quiet_limit = NOTHING_HEARD;
        self.take += 1;
        self.reading = false;
        self.read_len = 0;
        // Start the mic now so the first word isn't lost (a shortcut cancels it a moment later).
        if self.engine.is_some() && keys::mic_allowed() {
            self.start_audio();
        }
    }

    fn show_listening(&mut self, mode: &str, now: f64) {
        self.listen_at = now;
        self.heard_at = now;
        self.voice_at = now;
        self.read_at = now;
        if self.engine.is_none() {
            let code = if self.engine_error.is_some() { "recognition" } else { "model_missing" };
            emit(json!({ "type": "error", "code": code, "message": self.engine_error.clone().unwrap_or_default() }));
            return self.cancel();
        }
        if !keys::mic_allowed() {
            emit(json!({ "type": "error", "code": "permission", "what": "microphone" }));
            return self.cancel();
        }
        if self.source.is_none() {
            self.start_audio();
            if self.phase == Phase::Idle {
                return;
            }
        }
        emit(json!({ "type": "state", "state": "listening", "mode": mode }));
        if !self.best.is_empty() {
            emit(json!({ "type": "partial", "text": self.best }));
        }
    }

    fn start_audio(&mut self) {
        self.buf.lock().unwrap().clear();
        let sink = Sink::new(self.buf.clone(), self.tx.clone());
        let started = match &self.test_audio {
            Some(p) => audio::play_file(p, sink).map_err(|e| MicError::new("failed", e)),
            None => audio::open_mic(sink),
        };
        match started {
            Ok(s) => self.source = Some(s),
            Err(e) => {
                log(&format!("audio: {} ({})", e.detail, e.reason));
                emit(mic_error(&e));
                self.cancel();
            }
        }
    }

    fn stop_audio(&mut self) {
        self.source = None;
    }

    // While listening: re-read the whole take now and then, for live text and pause detection.
    fn maybe_read(&mut self, now: f64) {
        let Some(engine) = &self.engine else { return };
        if !self.spoke || self.reading || now - self.read_at < READ_EVERY {
            return;
        }
        let audio = self.buf.lock().unwrap().clone();
        if audio.len() < RATE as usize / 2 || audio.len() < self.read_len + RATE as usize * 3 / 10 {
            return;
        }
        self.read_at = now;
        self.read_len = audio.len();
        self.reading = engine.send(Job { take: self.take, audio, last: false }).is_ok();
    }

    fn finish(&mut self) {
        if !matches!(self.phase, Phase::Armed | Phase::Holding | Phase::HandsFree) {
            return;
        }
        let shown = self.phase != Phase::Armed;
        self.report_empty = shown;
        self.phase = Phase::Finishing;
        self.ink.capture(self.now());
        if shown {
            emit(json!({ "type": "state", "state": "processing" }));
        }
        if self.source.is_none() {
            return self.finalize();
        }
        self.tail_until = Some(self.now() + TAIL);
    }

    // Mic off, then one last read of the whole take; its result ends the session.
    fn close_take(&mut self, now: f64) {
        self.stop_audio();
        let audio = std::mem::take(&mut *self.buf.lock().unwrap());
        match &self.engine {
            Some(engine) if self.spoke && engine.send(Job { take: self.take, audio, last: true }).is_ok() => self.final_until = Some(now + FINAL_TIMEOUT),
            _ => self.finalize(),
        }
    }

    fn on_text(&mut self, take: u64, text: String, last: bool) {
        if take != self.take {
            return;
        }
        if !last {
            self.reading = false;
            self.read_done_at = self.now();
        }
        if self.phase == Phase::Wake {
            return self.heard_while_waiting(text);
        }
        // A take started by a wake phrase: the phrase itself isn't part of what was said.
        let text = if self.woke { wake::strip(&self.wake_phrases, &text) } else { text };
        // A cancel word said last ("…, never mind"): drop the whole take, live or at its closing read.
        if matches!(self.phase, Phase::Holding | Phase::HandsFree | Phase::Finishing) && wake::ends_with_word(&self.cancel_words, &text) {
            let now = self.now();
            self.end("cancel word", now);
            return self.cancel();
        }
        // An end word said last: what came before it is the whole request, sent now.
        let cut = wake::cut_end_word(&self.end_words, &text);
        let had_cut = cut.is_some();
        let text = match cut {
            Some(c) => c,
            // Ended by an end word, but the closing read heard a little more after it: keep what the live read had.
            None if last && self.ended_by_word => self.best.clone(),
            None => text,
        };
        if had_cut && !self.ended_by_word && matches!(self.phase, Phase::Holding | Phase::HandsFree) && !text.is_empty() {
            self.ended_by_word = true;
            self.best = text.clone();
            self.heard_at = self.now();
            emit(json!({ "type": "partial", "text": self.best }));
            let now = self.now();
            self.end("end word", now);
            self.finish();
        }
        // A closing read that came back empty keeps whatever the live reads heard.
        if !text.is_empty() && text != self.best {
            self.best = text;
            self.heard_at = self.now();
            if !matches!(self.phase, Phase::Idle | Phase::Armed) {
                emit(json!({ "type": "partial", "text": self.best }));
            }
        }
        if last && self.phase == Phase::Finishing {
            self.finalize();
        }
    }

    fn on_level(&mut self, v: f64) {
        let now = self.now();
        if v > 0.2 {
            self.voice_at = now;
        }
        if v > SPEECH_LEVEL && self.phase != Phase::Idle {
            self.spoke = true;
        }
        if !matches!(self.phase, Phase::Holding | Phase::HandsFree) || now - self.level_at < 0.05 {
            return;
        }
        self.level_at = now;
        emit(json!({ "type": "level", "v": (v * 100.0).round() / 100.0 }));
    }

    fn finalize(&mut self) {
        if self.phase != Phase::Finishing {
            return;
        }
        self.stop_audio();
        self.tail_until = None;
        self.final_until = None;
        self.take += 1;
        self.phase = Phase::Idle;
        self.rest_wake();
        let text = self.best.trim().to_string();
        let area = self.ink.take();
        if !text.is_empty() {
            // What was circled on screen while talking rides along with the words.
            let mut msg = json!({ "type": "final", "text": text });
            if let Some(a) = area {
                msg["area"] = a;
                log("sent what was said, with the circled area");
            }
            emit(msg);
        } else if let Some(f) = area.as_ref().and_then(|a| a["file"].as_str()) {
            // Circled something but said nothing: drop it quietly (no "I didn't hear you").
            let _ = std::fs::remove_file(f);
            log("nothing said, dropped the circled area");
            self.report_empty = false;
        }
        emit(json!({ "type": "state", "state": "idle" }));
        // Listened but got no words: say so instead of closing silently.
        if text.is_empty() && self.report_empty {
            emit(json!({ "type": "error", "code": "nothing_heard", "spoke": self.voice_at > self.listen_at + 0.05 }));
        }
        self.report_empty = false;
    }

    fn cancel(&mut self) {
        self.stop_audio();
        self.buf.lock().unwrap().clear();
        self.tail_until = None;
        self.final_until = None;
        self.take += 1;
        self.reading = false;
        self.phase = Phase::Idle;
        self.report_empty = false;
        self.rest_wake();
        let now = self.now();
        self.ink.clear(now);
        emit(json!({ "type": "state", "state": "idle", "cancelled": true }));
    }

    // ---------- wake word ----------

    fn set_wake(&mut self, on: bool, words: Option<Vec<String>>) {
        if let Some(words) = words {
            let phrases = wake::phrases(&words);
            if phrases != self.wake_phrases {
                log(&format!("wake phrases: {}", phrases.len()));
                self.wake_phrases = phrases;
            }
        }
        if on != self.wake_on {
            log(&format!("wake word {}", if on { "on" } else { "off" }));
            self.wake_on = on;
        }
        if !self.wake_on || self.wake_phrases.is_empty() {
            self.stop_wake();
        }
    }

    fn set_wake_paused(&mut self, on: bool) {
        if on == self.wake_paused {
            return;
        }
        self.wake_paused = on;
        log(if on { "wake: paused by the user" } else { "wake: resumed by the user" });
        if on {
            self.stop_wake();
        }
        emit(json!({ "type": "wake_paused", "on": on }));
    }

    fn set_wake_hold(&mut self, on: bool) {
        if on != self.wake_hold && self.wake_on {
            log(if on { "wake: paused while Roamies answers" } else { "wake: resumed" });
        }
        self.wake_hold = on;
        if on {
            self.stop_wake();
        }
    }

    /// After a take (sent or dropped): rest a moment before listening for the phrase again.
    fn rest_wake(&mut self) {
        self.woke = false;
        self.wake_cooldown_until = self.now() + WAKE_COOLDOWN;
    }

    // Never asks for anything: wake listening starts only with the model loaded and the microphone allowed.
    fn wake_allowed(&self, now: f64) -> bool {
        self.wake_on
            && !self.wake_hold
            && !self.wake_paused
            && !self.wake_phrases.is_empty()
            && self.engine.is_some()
            && now >= self.wake_cooldown_until
            && now >= self.wake_retry_at
    }

    fn start_wake(&mut self, now: f64) {
        // If the mic won't open, try again in a while rather than every frame.
        self.wake_retry_at = now + 30.0;
        if !keys::mic_allowed() {
            return;
        }
        self.take += 1;
        self.reset_burst(now);
        self.buf.lock().unwrap().clear();
        let sink = Sink::new(self.buf.clone(), self.tx.clone());
        let started = match &self.test_audio {
            Some(p) => audio::play_file(p, sink).map_err(|e| MicError::new("failed", e)),
            None => audio::open_mic(sink),
        };
        match started {
            // Wake listening fails quietly: no error for the user, who didn't ask for anything just now.
            Err(e) => log(&format!("wake: couldn't open the mic: {} ({})", e.detail, e.reason)),
            Ok(s) => {
                self.source = Some(s);
                self.phase = Phase::Wake;
                self.wake_retry_at = 0.0;
                log("wake: listening");
            }
        }
    }

    fn stop_wake(&mut self) {
        if self.phase != Phase::Wake {
            return;
        }
        self.stop_audio();
        self.buf.lock().unwrap().clear();
        self.take += 1;
        self.reading = false;
        self.phase = Phase::Idle;
    }

    // Start fresh for the next thing said: no speech yet, nothing read.
    fn reset_burst(&mut self, now: f64) {
        self.spoke = false;
        self.reading = false;
        self.read_len = 0;
        self.read_at = now;
        self.voice_at = now;
        self.heard_at = now;
        self.wake_text.clear();
        self.best.clear();
    }

    // While waiting: keep a moment of sound from before any speech, read each burst of speech as it grows, and
    // let it go after a pause (or once it's long) when it didn't start with a phrase.
    fn wake_tick(&mut self, now: f64) {
        if !self.spoke {
            let keep = (WAKE_PREROLL * RATE as f64) as usize;
            let mut buf = self.buf.lock().unwrap();
            if buf.len() > keep * 2 {
                let cut = buf.len() - keep;
                buf.drain(..cut);
            }
            return;
        }
        let len = self.buf.lock().unwrap().len();
        let ended = now - self.voice_at > WAKE_PAUSE && !self.reading;
        if ended || len as f64 > WAKE_MAX * RATE as f64 {
            if !self.wake_text.is_empty() {
                log(&format!("wake: heard {} words, not a phrase", self.wake_text.split_whitespace().count()));
            }
            self.take += 1; // a read still under way is for the burst that's gone
            self.reset_burst(now);
            self.buf.lock().unwrap().clear();
            return;
        }
        self.maybe_read(now);
    }

    fn heard_while_waiting(&mut self, text: String) {
        if text == self.wake_text {
            return;
        }
        self.wake_text = text;
        let Some(hit) = wake::find(&self.wake_phrases, &self.wake_text) else { return };
        let now = self.now();
        log(&format!("wake: heard phrase {} (distance {})", hit.phrase + 1, hit.distance));
        // Same mic, same take: what follows the phrase is already the start of what's said.
        self.woke = true;
        self.phase = Phase::HandsFree;
        self.chord_at = now;
        self.best = hit.rest;
        self.blocked = true; // keys held right now aren't a tap on the talk chord
        self.show_listening("wake", now);
    }

    /// Start listening hands-free without a key press (after an area snap). Ends on a pause or a tap.
    fn listen_now(&mut self) {
        self.stop_wake();
        if self.phase != Phase::Idle {
            return;
        }
        let now = self.now();
        self.arm(now);
        self.phase = Phase::HandsFree;
        self.blocked = true; // the snap shortcut's keys may still be held: don't read them as a tap
        self.show_listening("handsfree", now);
    }

    /// The talk shortcut where the keys can't be watched (`roamies --talk`): the first one starts listening
    /// hands-free, the next one sends what was said (so does a pause).
    fn toggle(&mut self) {
        match self.phase {
            Phase::Idle | Phase::Wake => self.listen_now(),
            Phase::Armed | Phase::Holding | Phase::HandsFree => {
                let now = self.now();
                self.end("toggled", now);
                self.finish();
            }
            Phase::Finishing => {}
        }
    }

    /// Roamies saw its show/hide shortcut: that chord press was a shortcut, not a request to talk.
    fn external_cancel(&mut self) {
        if matches!(self.phase, Phase::Armed | Phase::Holding | Phase::HandsFree) {
            self.cancel();
        }
        self.blocked = true;
    }

    /// Returns true when Roamies has gone (stdin closed) and the helper should exit.
    fn handle(&mut self, ev: Ev) -> bool {
        match ev {
            Ev::Cmd(msg) => match msg.get("cmd").and_then(Value::as_str) {
                Some("cancel") => self.external_cancel(),
                Some("listen") => {
                    self.listen_now();
                    if let Some(q) = msg.get("quiet").and_then(Value::as_f64).filter(|q| *q >= 1.0) {
                        self.quiet_limit = q;
                    }
                }
                Some("cancel_words") => {
                    let words: Vec<String> = msg.get("words").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    self.cancel_words = wake::end_words(&words);
                    log(&format!("cancel words: {}", self.cancel_words.len()));
                }
                Some("end_words") => {
                    let words: Vec<String> = msg.get("words").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    self.end_words = wake::end_words(&words);
                    log(&format!("end words: {}", self.end_words.len()));
                }
                Some("toggle") => self.toggle(),
                Some("chord") => {
                    let mods: Vec<String> = msg.get("mods").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    self.set_chord(&mods);
                }
                Some("wake") => {
                    let words = msg.get("words").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect());
                    self.set_wake(msg.get("on").and_then(Value::as_bool) == Some(true), words);
                }
                Some("wake_hold") => self.set_wake_hold(msg.get("on").and_then(Value::as_bool) == Some(true)),
                Some("wake_pause") => self.set_wake_paused(msg.get("on").and_then(Value::as_bool) == Some(true)),
                Some("pause_sends") => self.pause_sends = msg.get("on").and_then(Value::as_bool) == Some(true),
                // Muting the speakers while listening is macOS-only for now.
                _ => {}
            },
            Ev::StdinClosed => return true,
            Ev::Level(v) => self.on_level(v),
            Ev::AudioFailed(e) => {
                log(&format!("audio stream: {} ({})", e.detail, e.reason));
                if matches!(self.phase, Phase::Armed | Phase::Holding | Phase::HandsFree) {
                    emit(mic_error(&e));
                    self.cancel();
                } else if self.phase == Phase::Wake {
                    // Quietly: try again in a while (a mic plugged back in, or another one made the default).
                    self.stop_wake();
                    self.wake_retry_at = self.now() + 30.0;
                }
            }
            Ev::Text { take, text, last } => self.on_text(take, text, last),
            Ev::EngineFailed(e) => {
                self.engine = None;
                self.engine_error = Some(e);
                self.stop_wake();
            }
        }
        false
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--perms") {
        emit(json!({ "type": "perms", "mic": if keys::mic_allowed() { "granted" } else { "denied" }, "input": audio::has_input() }));
        return;
    }
    if args.iter().any(|a| a == "--devices") {
        let mut v = audio::devices();
        v["mic_allowed"] = json!(keys::mic_allowed());
        emit(v);
        return;
    }
    let model = arg(&args, "--model").map(PathBuf::from);
    // Cursor positions and captures in real pixels on every monitor (ink.rs turns them into screen points).
    #[cfg(windows)]
    // SAFETY: a plain call at startup, before any window or thread exists.
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    if let Some(wav) = arg(&args, "--transcribe") {
        let dir = model.expect("--transcribe needs --model <dir>");
        let mut asr = load_model(&dir).unwrap_or_else(|e| panic!("model: {e}"));
        let (samples, rate) = audio::read_wav(Path::new(&wav)).unwrap_or_else(|e| panic!("{wav}: {e}"));
        let t0 = Instant::now();
        let text = transcribe(&mut asr, samples.clone());
        emit(json!({ "text": text, "seconds": (t0.elapsed().as_secs_f64() * 100.0).round() / 100.0, "audio": samples.len() as f64 / rate as f64 }));
        return;
    }

    let (tx, rx): (Sender<Ev>, Receiver<Ev>) = mpsc::channel();
    let (engine, engine_thread) = match &model {
        Some(dir) if model_present(dir) => {
            let (jobs, handle) = spawn_engine(dir.clone(), tx.clone());
            (Some(jobs), Some(handle))
        }
        _ => {
            log("speech model not downloaded yet");
            (None, None)
        }
    };
    let mut voice = Voice::new(engine, arg(&args, "--test-audio").map(PathBuf::from), tx.clone());
    if let Some(c) = arg(&args, "--chord") {
        voice.set_chord(&c.split(',').map(str::to_owned).collect::<Vec<_>>());
    }

    let stdin_tx = tx.clone();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = stdin_tx.send(Ev::Cmd(v));
            }
        }
        let _ = stdin_tx.send(Ev::StdinClosed);
    });

    emit(json!({ "type": "ready", "engine": "parakeet", "model": voice.engine.is_some() }));
    loop {
        let quit = match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(ev) => {
                let mut quit = voice.handle(ev);
                while let Ok(ev) = rx.try_recv() {
                    quit |= voice.handle(ev);
                }
                quit
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => true,
        };
        if quit {
            break;
        }
        voice.tick();
    }
    // Let the engine thread finish (a model still loading included) before exiting: tearing ONNX Runtime
    // down under it crashes.
    drop(voice);
    if let Some(h) = engine_thread {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A Voice with a stand-in engine (the test reads its jobs) and a short WAV in place of the microphone.
    fn voice() -> (Voice, Receiver<Job>, PathBuf) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let wav = std::env::temp_dir().join(format!("roamies-voice-test-{}-{}.wav", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).expect("test wav");
        for i in 0..RATE / 2 {
            w.write_sample(((i as f32 * 0.2).sin() * 8000.0) as i16).expect("sample");
        }
        w.finalize().expect("wav written");
        let (jobs, jrx) = mpsc::channel();
        let (tx, _events) = mpsc::channel();
        (Voice::new(Some(jobs), Some(wav.clone()), tx), jrx, wav)
    }

    #[test]
    fn toggle_starts_a_take_and_the_next_one_sends_it() {
        let (mut v, jobs, wav) = voice();
        v.toggle();
        assert_eq!(v.phase, Phase::HandsFree, "the first toggle listens hands-free");
        assert!(v.source.is_some(), "the microphone is on");
        v.on_level(0.5);
        v.on_text(v.take, "hello there".into(), false);
        v.toggle();
        assert_eq!(v.phase, Phase::Finishing, "the second toggle ends the take");
        v.toggle();
        assert_eq!(v.phase, Phase::Finishing, "a third one while it finishes changes nothing");
        // The mic's tail is over: the whole take goes for one last read.
        v.tail_until = Some(0.0);
        v.tick();
        let job = jobs.try_recv().expect("a closing read");
        assert!(job.last && job.take == v.take);
        assert!(v.source.is_none(), "the microphone is off");
        v.on_text(job.take, "hello there friend".into(), true);
        assert_eq!(v.phase, Phase::Idle);
        assert_eq!(v.best, "hello there friend");
        // And it's ready to go again.
        v.toggle();
        assert_eq!(v.phase, Phase::HandsFree);
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn toggle_without_a_speech_model_stays_idle() {
        let (tx, _events) = mpsc::channel();
        let mut v = Voice::new(None, None, tx);
        v.toggle();
        assert_eq!(v.phase, Phase::Idle);
        assert!(v.source.is_none());
    }

    #[test]
    fn a_toggled_take_with_nothing_said_ends_without_a_read() {
        let (mut v, jobs, wav) = voice();
        v.toggle();
        v.toggle();
        v.tail_until = Some(0.0);
        v.tick();
        assert!(jobs.try_recv().is_err(), "silence isn't read (Parakeet makes words up from it)");
        assert_eq!(v.phase, Phase::Idle);
        let _ = std::fs::remove_file(wav);
    }

    // Wake listening is on with the given phrases, and the test file stands in for the room.
    fn waiting(words: &[&str]) -> (Voice, Receiver<Job>, PathBuf) {
        let (mut v, jobs, wav) = voice();
        v.set_wake(true, Some(words.iter().map(|s| s.to_string()).collect()));
        v.tick();
        assert_eq!(v.phase, Phase::Wake, "idle with the wake word on: listening for the phrase");
        assert!(v.source.is_some(), "the microphone is on");
        (v, jobs, wav)
    }

    #[test]
    fn a_wake_phrase_starts_a_hands_free_take_without_the_phrase() {
        let (mut v, jobs, wav) = waiting(&["Hey Jarvis"]);
        v.on_text(v.take, "so anyway".into(), false);
        assert_eq!(v.phase, Phase::Wake, "other words don't wake it");
        v.on_text(v.take, "Hey Jarvis, open".into(), false);
        assert_eq!(v.phase, Phase::HandsFree, "the phrase starts a take");
        assert!(v.source.is_some(), "on the same microphone");
        assert_eq!(v.best, "open", "what came after the phrase is the start of it");
        // Later reads of the whole take still begin with the phrase; it's taken off each time.
        v.on_text(v.take, "Hey Jarvis, open Chrome".into(), false);
        assert_eq!(v.best, "open Chrome");
        v.end("test", 0.0);
        v.finish();
        v.tail_until = Some(0.0);
        v.spoke = true;
        v.tick();
        let job = jobs.try_iter().find(|j| j.last).expect("a closing read");
        v.on_text(job.take, "Hey Jarvis, open Chrome please.".into(), true);
        assert_eq!(v.phase, Phase::Idle);
        assert_eq!(v.best, "open Chrome please.");
        assert!(!v.woke);
        v.tick();
        assert_eq!(v.phase, Phase::Idle, "it rests a moment before listening again");
        v.wake_cooldown_until = 0.0;
        v.tick();
        assert_eq!(v.phase, Phase::Wake, "then listens for the phrase again");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn wake_listening_waits_while_roamies_answers_and_stops_when_turned_off() {
        let (mut v, _jobs, wav) = waiting(&["Hey Roamies"]);
        v.set_wake_hold(true);
        assert_eq!(v.phase, Phase::Idle, "held: the mic is off");
        assert!(v.source.is_none());
        v.tick();
        assert_eq!(v.phase, Phase::Idle, "and stays off while held");
        v.set_wake_hold(false);
        v.tick();
        assert_eq!(v.phase, Phase::Wake);
        v.set_wake(false, None);
        assert_eq!(v.phase, Phase::Idle);
        assert!(v.source.is_none());
        v.set_wake(true, Some(vec!["   ".into()]));
        v.tick();
        assert_eq!(v.phase, Phase::Idle, "no phrases, nothing to listen for");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn paused_voice_detection_stays_off_until_resumed() {
        let (mut v, _jobs, wav) = waiting(&["Hey Roamies"]);
        v.handle(Ev::Cmd(json!({ "cmd": "wake_pause", "on": true })));
        assert_eq!(v.phase, Phase::Idle, "the mic is off");
        assert!(v.source.is_none());
        v.wake_cooldown_until = 0.0;
        v.tick();
        assert_eq!(v.phase, Phase::Idle, "and stays off while paused");
        v.handle(Ev::Cmd(json!({ "cmd": "wake_pause", "on": false })));
        v.tick();
        assert_eq!(v.phase, Phase::Wake, "resumed: listening for the phrase again");
        // The talk keys resume it too (set_wake_paused is what the chord press calls).
        v.handle(Ev::Cmd(json!({ "cmd": "wake_pause", "on": true })));
        v.set_wake_paused(false);
        assert!(!v.wake_paused);
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn a_burst_without_the_phrase_is_let_go_after_a_pause() {
        let (mut v, jobs, wav) = waiting(&["Hey Roamies"]);
        v.on_level(0.5);
        v.buf.lock().unwrap().extend(vec![0.1; RATE as usize]);
        v.read_at = -10.0;
        v.tick();
        let job = jobs.try_recv().expect("speech is read for the phrase");
        assert!(!job.last);
        v.on_text(job.take, "turn the music down".into(), false);
        assert_eq!(v.phase, Phase::Wake);
        v.voice_at = v.now() - WAKE_PAUSE - 0.1;
        v.tick();
        assert!(!v.spoke && v.wake_text.is_empty(), "a fresh start for the next thing said");
        let old = job.take;
        v.on_text(old, "Hey Roamies".into(), false);
        assert_eq!(v.phase, Phase::Wake, "a late read of the old burst can't wake it");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn a_held_take_sends_on_a_pause_when_asked() {
        let (mut v, _jobs, wav) = voice();
        v.arm(v.now());
        v.phase = Phase::Holding;
        let now = v.now();
        v.show_listening("hold", now);
        v.on_level(0.5);
        v.on_text(v.take, "what's the time".into(), false);
        v.heard_at -= 3.0;
        v.voice_at -= 3.0;
        // tick_keys reads the real keyboard; nothing is held in a test, and the first frame of a release doesn't
        // count yet (the debounce), so this frame is decided by the pause alone.
        v.tick_keys(v.now());
        assert_eq!(v.phase, Phase::Holding, "off by default: holding the keys keeps listening");
        v.released_at = None;
        v.pause_sends = true;
        v.tick_keys(v.now());
        assert_eq!(v.phase, Phase::Finishing, "on: done talking sends it");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn an_end_word_sends_at_once_without_it() {
        let (mut v, jobs, wav) = voice();
        v.handle(Ev::Cmd(json!({ "cmd": "end_words", "words": ["over", "go"] })));
        v.toggle();
        v.on_level(0.5);
        v.on_text(v.take, "open my".into(), false);
        assert_eq!(v.phase, Phase::HandsFree);
        v.on_text(v.take, "open my email, go.".into(), false);
        assert_eq!(v.phase, Phase::Finishing, "the end word ends it straight away");
        assert_eq!(v.best, "open my email");
        v.tail_until = Some(0.0);
        v.tick();
        let job = jobs.try_iter().find(|j| j.last).expect("a closing read");
        // The closing read heard a breath after it: what the live read had stands.
        v.on_text(job.take, "Open my email, go. Uh".into(), true);
        assert_eq!(v.phase, Phase::Idle);
        assert_eq!(v.best, "open my email");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn a_cancel_word_drops_the_take_and_sends_nothing() {
        let (mut v, jobs, wav) = voice();
        v.handle(Ev::Cmd(json!({ "cmd": "cancel_words", "words": ["cancel", "never mind"] })));
        v.handle(Ev::Cmd(json!({ "cmd": "end_words", "words": ["over"] })));
        v.toggle();
        v.on_level(0.5);
        v.on_text(v.take, "open my email".into(), false);
        assert_eq!(v.phase, Phase::HandsFree);
        v.on_text(v.take, "open my email, never mind.".into(), false);
        assert_eq!(v.phase, Phase::Idle, "dropped at once");
        assert!(v.source.is_none(), "the microphone is off");
        assert!(jobs.try_iter().all(|j| !j.last), "no closing read: nothing is sent");
        // Said just before letting go, it's caught at the closing read instead.
        v.toggle();
        v.on_level(0.5);
        v.on_text(v.take, "open my".into(), false);
        v.end("test", 0.0);
        v.finish();
        v.tail_until = Some(0.0);
        v.tick();
        let job = jobs.try_iter().find(|j| j.last).expect("a closing read");
        v.on_text(job.take, "Open my email. Cancel.".into(), true);
        assert_eq!(v.phase, Phase::Idle);
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn listening_after_a_reply_lets_go_after_a_short_quiet() {
        let (mut v, _jobs, wav) = voice();
        v.handle(Ev::Cmd(json!({ "cmd": "listen", "quiet": 3 })));
        assert_eq!(v.phase, Phase::HandsFree);
        assert_eq!(v.quiet_limit, 3.0);
        v.listen_at -= 2.0;
        v.voice_at -= 2.0;
        v.released_at = None;
        v.tick_keys(v.now());
        assert_eq!(v.phase, Phase::HandsFree, "2 s in: still listening");
        v.listen_at -= 1.5;
        v.voice_at -= 1.5;
        v.tick_keys(v.now());
        assert_eq!(v.phase, Phase::Idle, "3 s of nothing: the mic closes");
        // A key tap afterwards gets the usual 8 s.
        v.arm(v.now());
        assert_eq!(v.quiet_limit, NOTHING_HEARD);
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn a_loud_room_still_ends_when_the_words_stop() {
        let (mut v, _jobs, wav) = voice();
        v.best = "open my email".into();
        let now = v.now();
        v.voice_at = now; // the fan keeps the level up
        v.heard_at = now - 3.0;
        v.read_done_at = now - 3.0 + 0.5;
        assert!(!v.paused(now), "the last read was right after the words: maybe still talking");
        v.read_done_at = now - 0.1;
        assert!(v.paused(now), "a read since then heard no more words");
        v.best.clear();
        assert!(!v.paused(now), "nothing said yet: keep listening");
        let _ = std::fs::remove_file(wav);
    }

    #[test]
    fn a_mic_error_tells_the_app_why() {
        let v = mic_error(&MicError::new("busy", "device in use"));
        assert_eq!(v["code"], "no_input", "the code older app builds already handle");
        assert_eq!(v["reason"], "busy");
        assert_eq!(v["message"], "device in use");
    }

    #[test]
    fn losing_the_mic_mid_take_ends_it_and_idle_ignores_it() {
        let (mut v, _jobs, wav) = voice();
        v.handle(Ev::AudioFailed(MicError::new("lost", "unplugged")));
        assert_eq!(v.phase, Phase::Idle, "nothing to end while idle");
        v.toggle();
        assert_eq!(v.phase, Phase::HandsFree);
        v.handle(Ev::AudioFailed(MicError::new("lost", "unplugged")));
        assert_eq!(v.phase, Phase::Idle, "the take ends");
        assert!(v.source.is_none(), "the microphone is off");
        let _ = std::fs::remove_file(wav);
    }
}

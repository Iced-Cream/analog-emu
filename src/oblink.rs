//! A stand-in for the Overbridge host -> Rytm audio stream: 12 channels a frame,
//! in Overwitch's host->Rytm order (MAIN L/R, MAIN FX L/R, then BD, SD, RS/CP,
//! BT, LT, MT/HT, CH/OH, CY/CB). The firmware's `voice_output_mix` adds each
//! voice's channel into the voice's digital layer (rytm-teensy
//! docs/overbridge-voice-inputs.md); `Machine` writes these frames into the
//! buffer it reads, above the USB controller, as it feeds USB MIDI.
//!
//! Over TCP (127.0.0.1:7301 by default) the other end plays the Overbridge host
//! (a simulated Teensy). The Rytm is the clock, as with real Overbridge: the
//! emulator asks for frames and the host answers.
//!
//!   emulator -> host: b'N' + u32 LE   frames wanted
//!                     b'T' + voice, note, velocity (u8 each)   a trig on a voice
//!                       that plays the EXT machine
//!                     b'P' + voice + 8 x u8   the EXT machine's P1..P8 on that
//!                       voice, when they change
//!   (T and P stand in for the Rytm-side report a mod will send over USB: the
//!   Rytm's sequencer sends no MIDI notes for its trigs.)
//!                     b'R' + u32 LE n + n x 12 x i32 LE   what the Rytm sends the
//!                       host, with a simulated USB host streaming (usbhost.rs):
//!                       MAIN L/R, the 8 voices, AUDIO IN L/R (full scale 2^31)
//!   host -> emulator: b'A' + u32 LE n + n x 12 x i32 LE   samples (full scale
//!                     2^31, as Overbridge's int32)

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

pub const CHANNELS: usize = 12;
pub type Frame = [i32; CHANNELS];

/// Frames to keep buffered (about 10 ms).
const TARGET: usize = 512;

pub struct ObLink {
    frames: mpsc::Receiver<Frame>,
    requests: mpsc::Sender<Vec<u8>>,
    outstanding: usize,
    pub connected: bool,
}

impl ObLink {
    /// Listen on `addr`; the first host to connect is served (one at a time).
    pub fn listen(addr: &str) -> std::io::Result<ObLink> {
        let l = TcpListener::bind(addr)?;
        let (ftx, frx) = mpsc::channel::<Frame>();
        let (rtx, rrx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let rrx = std::sync::Arc::new(std::sync::Mutex::new(rrx));
            for s in l.incoming().flatten() {
                eprintln!("overbridge link: host connected from {}", s.peer_addr().map(|a| a.to_string()).unwrap_or_default());
                let _ = s.set_nodelay(true);
                // Requests out on one thread, audio in on this one.
                let mut w = match s.try_clone() {
                    Ok(w) => w,
                    Err(_) => continue,
                };
                let rq = rrx.clone();
                let writer = std::thread::spawn(move || {
                    let _ = w.write_all(&[b'N'].iter().copied().chain((TARGET as u32).to_le_bytes()).collect::<Vec<u8>>());
                    loop {
                        let m = match rq.lock().unwrap().recv() {
                            Ok(m) => m,
                            Err(_) => return,
                        };
                        if w.write_all(&m).is_err() {
                            return;
                        }
                    }
                });
                if read_audio(s, &ftx).is_err() {
                    eprintln!("overbridge link: host disconnected");
                }
                drop(writer);
            }
        });
        Ok(ObLink { frames: frx, requests: rtx, outstanding: TARGET, connected: false })
    }

    /// Frames received since the last call (appended to `q`); asks the host for
    /// more so that `q` plus what is in flight stays near TARGET.
    pub fn poll(&mut self, q: &mut VecDeque<Frame>) {
        while let Ok(f) = self.frames.try_recv() {
            q.push_back(f);
            self.outstanding = self.outstanding.saturating_sub(1);
            self.connected = true;
        }
        let have = q.len() + self.outstanding;
        if have < TARGET {
            let want = TARGET - have;
            let mut m = vec![b'N'];
            m.extend((want as u32).to_le_bytes());
            if self.requests.send(m).is_ok() {
                self.outstanding += want;
            }
        }
    }
}

impl ObLink {
    /// Rytm -> host frames (the Overbridge IN stream).
    pub fn send_rytm(&self, frames: &[Frame]) {
        if frames.is_empty() {
            return;
        }
        let mut m = Vec::with_capacity(5 + frames.len() * CHANNELS * 4);
        m.push(b'R');
        m.extend((frames.len() as u32).to_le_bytes());
        for f in frames {
            for x in f {
                m.extend(x.to_le_bytes());
            }
        }
        let _ = self.requests.send(m);
    }

    /// A trig on `voice` (note 0..127, velocity 0..127).
    pub fn send_trig(&self, voice: u8, note: u8, velocity: u8) {
        let _ = self.requests.send(vec![b'T', voice, note, velocity]);
    }

    /// The EXT machine's eight parameters on `voice`.
    pub fn send_params(&self, voice: u8, p: [u8; 8]) {
        let mut m = vec![b'P', voice];
        m.extend(p);
        let _ = self.requests.send(m);
    }
}

fn read_audio(mut s: TcpStream, out: &mpsc::Sender<Frame>) -> std::io::Result<()> {
    let mut hdr = [0u8; 5];
    loop {
        s.read_exact(&mut hdr)?;
        if hdr[0] != b'A' {
            return Err(std::io::Error::other("bad message"));
        }
        let n = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        let mut buf = vec![0u8; n * CHANNELS * 4];
        s.read_exact(&mut buf)?;
        for f in buf.chunks_exact(CHANNELS * 4) {
            let mut fr = [0i32; CHANNELS];
            for (c, b) in f.chunks_exact(4).enumerate() {
                fr[c] = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
            if out.send(fr).is_err() {
                return Ok(());
            }
        }
    }
}

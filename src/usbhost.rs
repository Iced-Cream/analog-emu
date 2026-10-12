//! A simulated USB host on the Rytm's USB port, driving the device controller
//! (`usbdev.rs`) the way a real host would: VBUS, bus reset, a high-speed port,
//! enumeration, then Overbridge's audio stream (Overwitch, src/engine.c:
//! configuration 1, interfaces 1 and 2 at alternate setting 3, interrupt
//! endpoints 0x83 IN and 0x03 OUT; blocks of a 32-byte header, `0x07ff` magic and
//! a frame counter in front, then 7 frames x 12 channels of big-endian int32).
//!
//! The controller side, ChipIdea-style (MCF5441x RM, USB OTG): endpoint queue
//! heads (dQH, 64 bytes, index 2 x endpoint + direction, at EPLISTADDR) hold the
//! setup packet at +0x28 and the overlay of the transfer descriptor list at +8;
//! transfer descriptors (dTD) are big-endian: +0 next (bit 0 terminate), +4 token
//! (total bytes 30:16, IOC bit 15, active bit 7), +8.. five buffer pointers.
//! Completing a dTD clears active, leaves the untransferred count in the token,
//! advances the overlay and, with IOC, sets the endpoint's EPCOMPLETE bit and
//! USBSTS.UI. The controller interrupts on INTC2 source 17 (vector 209,
//! handler 0x400040ae in OS 1.73) [measured].

use std::collections::VecDeque;

use crate::bus::Bus;

const R: u32 = 0xFC0B_0000;
const USBCMD: u32 = R + 0x140;
const USBSTS: u32 = R + 0x144;
const USBINTR: u32 = R + 0x148;
const FRINDEX: u32 = R + 0x14C;
const EPLISTADDR: u32 = R + 0x158;
const PORTSC1: u32 = R + 0x184;
const OTGSC: u32 = R + 0x1A4;
const EPSETUPSR: u32 = R + 0x1AC;
const EPSR: u32 = R + 0x1B8;
const EPCOMPLETE: u32 = R + 0x1BC;
const EPNAK: u32 = R + 0x178;

const STS_UI: u32 = 1 << 0;
const STS_PCI: u32 = 1 << 2;
const STS_URI: u32 = 1 << 6;
const STS_SRI: u32 = 1 << 7;
const OTG_BSV: u32 = 1 << 11;
const OTG_AVV: u32 = 1 << 9;
const OTG_ASV: u32 = 1 << 10;
const OTG_BSE: u32 = 1 << 12;
const OTG_BSVIS: u32 = 1 << 19;

pub const OB_FRAMES_PER_BLOCK: usize = 7;
pub const OB_CHANNELS: usize = 12;
pub const OB_BLOCK: usize = 32 + OB_FRAMES_PER_BLOCK * OB_CHANNELS * 4; // 368

/// The interrupt line: an enabled USBSTS bit, or an enabled OTGSC status bit.
pub fn line(bus: &Bus) -> bool {
    let sts = bus.peek32(USBSTS) & bus.peek32(USBINTR);
    let otg = bus.peek32(OTGSC);
    sts != 0 || ((otg >> 16) & (otg >> 24) & 0x7F) != 0
}

fn raise(bus: &mut Bus, sts: u32) {
    let v = bus.peek32(USBSTS) | sts;
    bus.poke32(USBSTS, v);
    bus.io.usb_irq = line(bus);
    bus.io.update_irq();
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting for the firmware to start the controller (USBCMD.RS) in device mode.
    Idle,
    /// VBUS raised; waiting for RS.
    Vbus,
    /// Bus reset signalled at `t`.
    Reset,
    /// Port reported; control requests from here.
    Enumerate,
    /// Configured: the audio stream runs.
    Stream,
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    /// Setup packet posted, waiting for the data stage's dTD (or the status).
    Data,
    /// Data done, waiting for the status stage's dTD.
    Status,
}

struct Ctl {
    setup: [u8; 8],
    out: Vec<u8>,
    stage: Stage,
    got: Vec<u8>,
    t0: u64,
}

pub struct UsbHost {
    phase: Phase,
    /// Microframes since the host was attached, and the phase's start.
    uf: u64,
    t: u64,
    queue: VecDeque<[u8; 8]>,
    ctl: Option<Ctl>,
    log: bool,
    /// Descriptors read during enumeration (device, configuration).
    pub device_desc: Vec<u8>,
    pub config_desc: Vec<u8>,
    /// Rytm -> host frames received (12 channels), for the caller to take.
    pub from_rytm: VecDeque<[i32; OB_CHANNELS]>,
    /// Frames the host may still send (Overwitch sends as much as it receives).
    credit: usize,
    out_counter: u16,
    /// IN bytes the endpoint may still move: a high-speed interrupt endpoint with
    /// a 368-byte packet and bInterval 1 moves one packet per 125 us microframe.
    in_budget: usize,
    /// Read the MIDI streaming interface's bulk IN endpoint (0x81), as a host
    /// with the port open does. Only used when the firmware's own USB MIDI
    /// sender runs (`Machine::usb_midi_real`); otherwise the emulator takes
    /// the sender's calls and nothing is queued there.
    pub midi_in: bool,
    /// Clocks [from, to) during which the host stops reading MIDI IN but
    /// keeps the bus running (an application with the port closed, a stalled
    /// host): SOFs and the audio stream go on, the sender's requests stay
    /// queued.
    pub midi_sleep: Option<(u64, u64)>,
    /// Clocks [from, to) during which the host suspends the bus (a sleeping
    /// laptop): no SOF (FRINDEX stops), no transfers; PORTSC1.SUSP and
    /// USBSTS.SLI set on entry, a port change on resume.
    pub suspend: Option<(u64, u64)>,
    suspended: bool,
    /// MIDI bytes read from 0x81, for the caller to take.
    pub midi_out: Vec<u8>,
    /// The controller's general-purpose timers 0 and 1 (GPTIMERnLD at 0x80 /
    /// 0x88, GPTIMERnCTRL at 0x84 / 0x8C: bit 31 run, 30 reset, 24 repeat;
    /// 1 us counts; USBSTS bit 24 + n at zero): microseconds left, None when
    /// not loaded. OS 1.73 runs timer 0 at 10 ms (load 9999, 0x81000000;
    /// 0x40004f84) and its USB interrupt calls the callbacks registered with
    /// 0x40004b6e on each expiry - among them 0x40002566, which turns the USB
    /// MIDI sender on (0x413db254 = 1) once MIDI IN NAKs, and off, flushing
    /// endpoint 1 IN, when its oldest request has waited over 400
    /// microframes [measured]. Ticked only with `midi_in`, so the default
    /// mode runs as before.
    gpt: [Option<i64>; 2],
    pub stats: UsbStats,
}

#[derive(Default, Debug, Clone)]
pub struct UsbStats {
    pub microframes: u64,
    pub in_transfers: u64,
    pub out_transfers: u64,
    pub in_frames: u64,
    pub out_frames: u64,
    pub bad_blocks: u64,
    /// MIDI IN (0x81) dTDs completed, and microframes the host did not read it.
    pub midi_dtds: u64,
    pub midi_asleep: u64,
}

impl Default for UsbHost {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbHost {
    pub fn new() -> UsbHost {
        UsbHost {
            phase: Phase::Idle,
            uf: 0,
            t: 0,
            queue: VecDeque::new(),
            ctl: None,
            log: std::env::var_os("DTEMU_USB_LOG").is_some(),
            device_desc: Vec::new(),
            config_desc: Vec::new(),
            from_rytm: VecDeque::new(),
            credit: 0,
            out_counter: 0,
            in_budget: 0,
            midi_in: false,
            midi_sleep: None,
            suspend: None,
            suspended: false,
            midi_out: Vec::new(),
            gpt: [None, None],
            stats: UsbStats::default(),
        }
    }

    pub fn streaming(&self) -> bool {
        self.phase == Phase::Stream
    }

    fn say(&self, s: &str) {
        if self.log {
            eprintln!("usb host: {s}");
        }
    }

    /// One 125 us microframe. `to_rytm` supplies host -> Rytm frames.
    pub fn microframe(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        self.stats.microframes += 1;
        self.uf += 1;
        let now = self.uf;
        let ms = 8; // microframes
        let cmd = bus.peek32(USBCMD);
        let running = cmd & 1 != 0;
        if self.midi_in {
            self.gptimers(bus);
        }
        let t = bus.io.now;
        let asleep = self.suspend.is_some_and(|(a, b)| (a..b).contains(&t));
        if asleep != self.suspended {
            self.suspended = asleep;
            let p = bus.peek32(PORTSC1);
            if asleep {
                bus.poke32(PORTSC1, p | (1 << 7));
                raise(bus, 1 << 8);
                self.say("suspend");
            } else {
                bus.poke32(PORTSC1, p & !(1 << 7));
                raise(bus, STS_PCI);
                self.say("resume");
            }
        }
        if asleep {
            self.stats.midi_asleep += 1;
            return;
        }
        match self.phase {
            Phase::Idle => {
                // Cable in: VBUS valid (B session) and its interrupt.
                let otg = bus.peek32(OTGSC) | OTG_BSV | OTG_AVV | OTG_ASV | OTG_BSE | OTG_BSVIS;
                bus.poke32(OTGSC, otg);
                bus.io.usb_irq = line(bus);
                bus.io.update_irq();
                self.phase = Phase::Vbus;
                self.t = now;
                self.say("VBUS on");
            }
            Phase::Vbus => {
                if running && now > self.t + 50 * ms {
                    let p = bus.peek32(PORTSC1) | (1 << 8);
                    bus.poke32(PORTSC1, p);
                    raise(bus, STS_URI);
                    self.phase = Phase::Reset;
                    self.t = now;
                    self.say("bus reset");
                }
            }
            Phase::Reset => {
                if now > self.t + 10 * ms && bus.peek32(USBSTS) & STS_URI == 0 {
                    // Reset done: connected, enabled, high speed.
                    let p = (bus.peek32(PORTSC1) & !(1 << 8) & !(3 << 26)) | 1 | (1 << 2) | (2 << 26);
                    bus.poke32(PORTSC1, p);
                    raise(bus, STS_PCI);
                    self.phase = Phase::Enumerate;
                    self.t = now;
                    for s in [
                        [0x80, 6, 0, 1, 0, 0, 64, 0],     // GET_DESCRIPTOR device (64)
                        [0x00, 5, 7, 0, 0, 0, 0, 0],      // SET_ADDRESS 7
                        [0x80, 6, 0, 1, 0, 0, 18, 0],     // GET_DESCRIPTOR device
                        [0x80, 6, 0, 2, 0, 0, 9, 0],      // GET_DESCRIPTOR configuration (9)
                    ] {
                        self.queue.push_back(s);
                    }
                    self.say("port enabled, high speed");
                }
            }
            Phase::Enumerate | Phase::Stream => {
                if !running {
                    return;
                }
                // Start of (micro)frame: FRINDEX counts microframes.
                let fr = (bus.peek32(FRINDEX) + 1) & 0x3FFF;
                bus.poke32(FRINDEX, fr);
                raise(bus, STS_SRI);
                self.control(bus, now);
                if self.phase == Phase::Stream {
                    self.stream(bus, to_rytm);
                    if self.midi_in {
                        self.midi(bus);
                    }
                }
            }
        }
    }

    fn qh(bus: &Bus, ep: u32, dir_in: bool) -> u32 {
        (bus.peek32(EPLISTADDR) & !0x7FF) + 64 * (2 * ep + dir_in as u32)
    }

    /// The next dTD the controller would run: the overlay's next pointer, or,
    /// when that terminates, the current dTD's own next pointer read again from
    /// memory (how a dTD appended while the endpoint stays primed is found).
    fn next_dtd(bus: &Bus, qh: u32) -> u32 {
        let next = bus.peek32(qh + 8);
        if next & 1 == 0 {
            return next;
        }
        let cur = bus.peek32(qh + 4) & !0x1F;
        if cur == 0 {
            return 1;
        }
        bus.peek32(cur)
    }

    /// Run the active dTDs of one endpoint direction. For IN (device -> host)
    /// returns the bytes sent; for OUT, `data` is what the host sends (as much as
    /// fits). -> (bytes, any dTD completed)
    fn run_dtds(&mut self, bus: &mut Bus, ep: u32, dir_in: bool, data: &[u8]) -> (Vec<u8>, bool) {
        let bit = if dir_in { 1u32 << (16 + ep) } else { 1u32 << ep };
        if bus.peek32(EPSR) & bit == 0 {
            return (Vec::new(), false);
        }
        let qh = Self::qh(bus, ep, dir_in);
        let mut out = Vec::new();
        let mut sent = 0usize;
        let mut done = false;
        let mut next = Self::next_dtd(bus, qh);
        let mut guard = 0;
        while next & 1 == 0 && guard < 64 {
            guard += 1;
            let td = next & !0x1F;
            let tok = bus.peek32(td + 4);
            if tok & 0x80 == 0 {
                // An inactive dTD ends the list: the controller un-primes the
                // endpoint, and the firmware's next add primes it again (its
                // dTDs are a reused ring, so following `next` would go around).
                next = 1;
                break;
            }
            let total = ((tok >> 16) & 0x7FFF) as usize;
            let p0 = bus.peek32(td + 8);
            let addr = |k: usize| -> u32 {
                let first = 4096 - (p0 as usize & 0xFFF);
                if k < first {
                    p0 + k as u32
                } else {
                    let page = 1 + (k - first) / 4096;
                    (bus.peek32(td + 8 + 4 * page as u32) & !0xFFF) + ((k - first) % 4096) as u32
                }
            };
            let n;
            if dir_in {
                n = total;
                let addrs: Vec<u32> = (0..n).map(addr).collect();
                for a in addrs {
                    out.push(bus.peek8(a));
                }
            } else {
                n = total.min(data.len() - sent);
                let addrs: Vec<u32> = (0..n).map(addr).collect();
                for (k, a) in addrs.into_iter().enumerate() {
                    bus.poke8(a, data[sent + k]);
                }
                sent += n;
            }
            let left = (total - n) as u32;
            bus.poke32(td + 4, (tok & !0x7FFF_0000 & !0xFF) | (left << 16));
            // The (1 ms) frame it completed in, in buffer pointer 1's low 11 bits.
            // The firmware reads it (0x400052b2); its stream sync waits for a
            // completion in the frame after the first one and works back to the
            // microframe by counting packets (0x4000590a, OUT; 0x4000580c, IN)
            // to find that microframe's SOF timestamp (0x42f67a4c[uframe & 31]).
            let p1 = bus.peek32(td + 12);
            bus.poke32(td + 12, (p1 & !0x7FF) | ((bus.peek32(FRINDEX) >> 3) & 0x7FF));
            bus.poke32(qh + 4, td);
            let nx = bus.peek32(td);
            bus.poke32(qh + 8, nx);
            bus.poke32(qh + 12, bus.peek32(td + 4));
            if tok & 0x8000 != 0 {
                let c = bus.peek32(EPCOMPLETE) | bit;
                bus.poke32(EPCOMPLETE, c);
                done = true;
            }
            next = nx;
            // One dTD per call: the stream moves one packet per microframe (the
            // caller paces multi-packet dTDs), and the firmware's stream sync
            // depends on completions spreading over microframes as on a real bus.
            if ep != 0 {
                break;
            }
        }
        // The controller fetches the next dTD at once: none, or one not yet
        // active, un-primes the endpoint (the firmware then primes again when it
        // adds one: 0x40003dfc).
        if next & 1 != 0 || bus.peek32((next & !0x1F) + 4) & 0x80 == 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
        }
        if done {
            raise(bus, STS_UI);
        }
        (out, done)
    }

    fn control(&mut self, bus: &mut Bus, now: u64) {
        if self.ctl.is_none() {
            let Some(setup) = self.queue.pop_front() else {
                if self.phase == Phase::Enumerate {
                    self.phase = Phase::Stream;
                    self.say("configured: streaming");
                }
                return;
            };
            // Post the setup packet.
            // The setup buffer holds the packet as two 32-bit words in USB (little-
            // endian) order: the firmware reads each and byte-reverses it
            // (0x400051ec) [measured].
            let qh = Self::qh(bus, 0, false);
            bus.poke32(qh + 0x28, u32::from_le_bytes([setup[0], setup[1], setup[2], setup[3]]));
            bus.poke32(qh + 0x2C, u32::from_le_bytes([setup[4], setup[5], setup[6], setup[7]]));
            let s = bus.peek32(EPSETUPSR) | 1;
            bus.poke32(EPSETUPSR, s);
            raise(bus, STS_UI);
            self.say(&format!("setup {:02x?}", setup));
            self.ctl = Some(Ctl { setup, out: Vec::new(), stage: Stage::Data, got: Vec::new(), t0: now });
            return;
        }
        let mut c = self.ctl.take().unwrap();
        // A stalled endpoint 0 (EPCR0 TXS/RXS) refuses the request.
        let epcr0 = bus.peek32(R + 0x1C0);
        if epcr0 & 0x0001_0001 != 0 {
            self.say(&format!("setup {:02x?}: stalled", c.setup));
            bus.poke32(R + 0x1C0, epcr0 & !0x0001_0001);
            return;
        }
        let dev_to_host = c.setup[0] & 0x80 != 0;
        let wlen = u16::from_le_bytes([c.setup[6], c.setup[7]]) as usize;
        match c.stage {
            Stage::Data => {
                if wlen == 0 {
                    c.stage = Stage::Status;
                } else if dev_to_host {
                    let (b, done) = self.run_dtds(bus, 0, true, &[]);
                    c.got.extend(b);
                    if done || c.got.len() >= wlen {
                        c.got.truncate(wlen);
                        c.stage = Stage::Status;
                    }
                } else {
                    let data = c.out.clone();
                    let (_, done) = self.run_dtds(bus, 0, false, &data);
                    if done {
                        c.stage = Stage::Status;
                    }
                }
                if !matches!(c.stage, Stage::Status) {
                    if now > c.t0 + 16_000 {
                        self.say(&format!("setup {:02x?}: no data stage, dropped", c.setup));
                        return;
                    }
                    self.ctl = Some(c);
                    return;
                }
            }
            Stage::Status => {}
        }
        // Status stage: the opposite direction, zero length.
        let (_, done) = if dev_to_host && wlen > 0 {
            self.run_dtds(bus, 0, false, &[])
        } else {
            self.run_dtds(bus, 0, true, &[])
        };
        if !done {
            if now > c.t0 + 16_000 {
                self.say(&format!("setup {:02x?}: no status stage, dropped", c.setup));
                return;
            }
            self.ctl = Some(c);
            return;
        }
        self.finished(&c);
    }

    fn finished(&mut self, c: &Ctl) {
        self.say(&format!("setup {:02x?} done, {} bytes in", c.setup, c.got.len()));
        let (req, dtype) = (c.setup[1], c.setup[3]);
        if c.setup[0] == 0x80 && req == 6 && dtype == 1 {
            self.device_desc = c.got.clone();
        }
        if c.setup[0] == 0x80 && req == 6 && dtype == 2 {
            if c.got.len() >= 4 && c.got.len() == 9 {
                let total = u16::from_le_bytes([c.got[2], c.got[3]]);
                let t = total.to_le_bytes();
                self.queue.push_front([0x80, 6, 0, 2, 0, 0, t[0], t[1]]);
            } else {
                self.config_desc = c.got.clone();
                for s in [
                    [0x00, 9, 1, 0, 0, 0, 0, 0],  // SET_CONFIGURATION 1
                    [0x01, 11, 3, 0, 1, 0, 0, 0], // SET_INTERFACE 1, alt 3 (audio in)
                    [0x01, 11, 3, 0, 2, 0, 0, 0], // SET_INTERFACE 2, alt 3 (audio out)
                ] {
                    self.queue.push_back(s);
                }
            }
        }
    }

    /// One microframe (125 us) of the general-purpose timers (`gpt`).
    fn gptimers(&mut self, bus: &mut Bus) {
        for k in 0..2u32 {
            let (ld, ctrl) = (R + 0x80 + 8 * k, R + 0x84 + 8 * k);
            let c = bus.peek32(ctrl);
            if c & (1 << 30) != 0 {
                self.gpt[k as usize] = None;
                bus.poke32(ctrl, c & !(1 << 30));
            }
            if c & (1 << 31) == 0 {
                continue;
            }
            let left = self.gpt[k as usize].unwrap_or(bus.peek32(ld) as i64 & 0xFF_FFFF) - 125;
            if left < 0 {
                raise(bus, 1 << (24 + k));
                if c & (1 << 24) != 0 {
                    self.gpt[k as usize] = Some(left + 1 + (bus.peek32(ld) as i64 & 0xFF_FFFF));
                } else {
                    self.gpt[k as usize] = None;
                    bus.poke32(ctrl, c & !(1 << 31));
                }
            } else {
                self.gpt[k as usize] = Some(left);
            }
        }
    }

    /// MIDI IN: up to 8 of the firmware's queued dTDs on endpoint 1 IN a
    /// microframe (bulk; a real host's polling is about that eager), their
    /// USB-MIDI event packets decoded into `midi_out`. Each dTD carries IOC,
    /// so its completion interrupt hands the request back to the sender's pool
    /// (0x40003b24 -> 0x40003938, OS 1.73) [measured]. While asleep nothing
    /// is read and the requests stay queued.
    fn midi(&mut self, bus: &mut Bus) {
        let now = bus.io.now;
        if self.midi_sleep.is_some_and(|(a, b)| (a..b).contains(&now)) {
            self.stats.midi_asleep += 1;
            return;
        }
        // Polling an IN endpoint with nothing primed NAKs: EPNAK's bit 17. The
        // firmware takes that as "a host reads MIDI" (0x40002566 sets
        // 0x413db254 = 1, the sender's go) [measured, OS 1.73].
        if bus.peek32(EPSR) & (1 << 17) == 0 {
            let v = bus.peek32(EPNAK) | (1 << 17);
            bus.poke32(EPNAK, v);
        }
        for _ in 0..8 {
            let (b, done) = self.run_dtds(bus, 1, true, &[]);
            if !done && b.is_empty() {
                break;
            }
            self.stats.midi_dtds += 1;
            for p in b.chunks_exact(4) {
                // CIN -> bytes in the packet (USB MIDI 1.0, table 4-1).
                let n = match p[0] & 0x0F {
                    0x5 | 0xF => 1,
                    0x2 | 0x6 | 0xC | 0xD => 2,
                    0x3 | 0x4 | 0x7 | 0x8 | 0x9 | 0xA | 0xB | 0xE => 3,
                    _ => 0,
                };
                self.midi_out.extend_from_slice(&p[1..1 + n]);
            }
        }
    }

    /// The audio stream: take what the Rytm sends on 0x83, answer on 0x03 with as
    /// many frames.
    fn stream(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        // One packet per microframe: a dTD of n packets completes in its n-th.
        let bit = 1u32 << 19;
        if bus.peek32(EPSR) & bit != 0 {
            let qh = Self::qh(bus, 3, true);
            let td = Self::next_dtd(bus, qh);
            if td & 1 == 0 {
                let total = ((bus.peek32((td & !0x1F) + 4) >> 16) & 0x7FFF) as usize;
                self.in_budget += OB_BLOCK;
                if total > self.in_budget {
                    return self.stream_out(bus, to_rytm);
                }
                self.in_budget = 0;
            }
        }
        let (b, done) = self.run_dtds(bus, 3, true, &[]);
        if done || !b.is_empty() {
            self.stats.in_transfers += 1;
            if self.log && self.stats.in_transfers <= 4 {
                eprintln!("usb host: IN {} bytes: {:02x?}", b.len(), &b[..b.len().min(80)]);
            }
            for blk in b.chunks(OB_BLOCK) {
                // Rytm -> host blocks start 0x0700 (host -> Rytm: 0x07ff) [measured].
                if blk.len() < OB_BLOCK || blk[0] != 0x07 {
                    self.stats.bad_blocks += 1;
                    continue;
                }
                for f in 0..OB_FRAMES_PER_BLOCK {
                    let mut fr = [0i32; OB_CHANNELS];
                    for (c, x) in fr.iter_mut().enumerate() {
                        let o = 32 + 4 * (f * OB_CHANNELS + c);
                        *x = i32::from_be_bytes([blk[o], blk[o + 1], blk[o + 2], blk[o + 3]]);
                    }
                    self.from_rytm.push_back(fr);
                    self.stats.in_frames += 1;
                    self.credit += 1;
                }
            }
            while self.from_rytm.len() > 48_000 {
                self.from_rytm.pop_front();
            }
        }
        self.stream_out(bus, to_rytm);
    }

    /// OUT: one transfer of whole blocks, when the Rytm has a buffer primed and
    /// we owe it frames.
    fn stream_out(&mut self, bus: &mut Bus, to_rytm: &mut VecDeque<[i32; OB_CHANNELS]>) {
        let bit = 1u32 << 3;
        if bus.peek32(EPSR) & bit == 0 || self.credit < OB_FRAMES_PER_BLOCK {
            return;
        }
        let qh = Self::qh(bus, 3, false);
        let td = Self::next_dtd(bus, qh);
        if td & 1 != 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
            return;
        }
        let tok = bus.peek32((td & !0x1F) + 4);
        if tok & 0x80 == 0 {
            let s = bus.peek32(EPSR) & !bit;
            bus.poke32(EPSR, s);
            return;
        }
        let total = ((tok >> 16) & 0x7FFF) as usize;
        // One packet (one block) per microframe.
        let blocks = (total / OB_BLOCK).min(self.credit / OB_FRAMES_PER_BLOCK).clamp(1, 1);
        if self.log && self.stats.out_transfers < 4 {
            eprintln!("usb host: OUT dTD {total} bytes, sending {blocks} blocks");
        }
        let mut data = vec![0u8; blocks * OB_BLOCK];
        for blk in data.chunks_mut(OB_BLOCK) {
            blk[0] = 0x07;
            blk[1] = 0xFF;
            blk[2..4].copy_from_slice(&self.out_counter.to_be_bytes());
            self.out_counter = self.out_counter.wrapping_add(OB_FRAMES_PER_BLOCK as u16);
            for f in 0..OB_FRAMES_PER_BLOCK {
                let fr = to_rytm.pop_front().unwrap_or([0; OB_CHANNELS]);
                for (c, x) in fr.iter().enumerate() {
                    let o = 32 + 4 * (f * OB_CHANNELS + c);
                    blk[o..o + 4].copy_from_slice(&x.to_be_bytes());
                }
            }
        }
        self.credit -= blocks * OB_FRAMES_PER_BLOCK;
        // The Rytm's OUT dTDs carry no IOC: its audio code polls them.
        self.run_dtds(bus, 3, false, &data);
        self.stats.out_transfers += 1;
        self.stats.out_frames += (blocks * OB_FRAMES_PER_BLOCK) as u64;
    }
}

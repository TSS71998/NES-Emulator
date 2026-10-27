use crate::region::{FrameSequencerTiming, Region};
use crate::state::{StateReader, StateWriter};

pub(crate) const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

pub(crate) const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 25% negated
];

const TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

/// The length counter shared by the pulse, triangle and noise channels.
/// Writes to the length/halt registers don't take effect at once: they're
/// applied on the next CPU cycle, after the frame counter has had its chance
/// to clock the counter. So a reload that lands on the same cycle as a
/// length clock is ignored (if the counter was non-zero), and a halt change
/// on that cycle comes too late to stop the clock.
#[derive(Default)]
struct LengthCounter {
    enabled: bool,
    counter: u8,
    halt: bool,
    new_halt: bool,
    reload: u8,
    previous: u8,
}

impl LengthCounter {
    fn load(&mut self, index: u8) {
        if self.enabled {
            self.reload = LENGTH_TABLE[(index & 0x1F) as usize];
            self.previous = self.counter;
        }
    }

    fn set_halt(&mut self, halt: bool) {
        self.new_halt = halt;
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.counter = 0;
            self.reload = 0;
        }
    }

    fn clock(&mut self) {
        if self.counter > 0 && !self.halt {
            self.counter -= 1;
        }
    }

    fn apply_writes(&mut self) {
        if self.reload != 0 {
            if self.counter == self.previous {
                self.counter = self.reload;
            }
            self.reload = 0;
        }
        self.halt = self.new_halt;
    }

    fn active(&self) -> bool {
        self.counter > 0
    }
}

#[derive(Default)]
pub struct Envelope {
    start: bool,
    loop_flag: bool,
    constant: bool,
    volume: u8,
    divider: u8,
    decay: u8,
}

impl Envelope {
    fn write(&mut self, value: u8) {
        self.loop_flag = value & 0x20 != 0;
        self.constant = value & 0x10 != 0;
        self.volume = value & 0x0f;
    }

    fn clock(&mut self) {
        if self.start {
            self.start = false;
            self.decay = 15;
            self.divider = self.volume;
        } else if self.divider == 0 {
            self.divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if self.loop_flag {
                self.decay = 15;
            }
        } else {
            self.divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.constant {
            self.volume
        } else {
            self.decay
        }
    }
}

#[derive(Default)]
pub struct Pulse {
    is_pulse1: bool,
    length: LengthCounter,
    duty: u8,
    duty_pos: u8,
    timer_period: u16,
    timer: u16,
    envelope: Envelope,
    sweep_enabled: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_divider: u8,
    sweep_reload: bool,
    muted: bool,
}

impl Pulse {
    fn new(is_pulse1: bool) -> Self {
        let mut pulse = Pulse {
            is_pulse1,
            ..Default::default()
        };
        pulse.update_muted();
        pulse
    }

    fn write_control(&mut self, value: u8) {
        self.duty = value >> 6;
        self.length.set_halt(value & 0x20 != 0);
        self.envelope.write(value);
    }

    fn write_sweep(&mut self, value: u8) {
        self.sweep_enabled = value & 0x80 != 0;
        self.sweep_period = (value >> 4) & 0x07;
        self.sweep_negate = value & 0x08 != 0;
        self.sweep_shift = value & 0x07;
        self.sweep_reload = true;
        self.update_muted();
    }

    fn write_timer_lo(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x0700) | value as u16;
        self.update_muted();
    }

    fn write_timer_hi(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x00ff) | (((value & 0x07) as u16) << 8);
        self.update_muted();
        self.length.load(value >> 3);
        self.duty_pos = 0;
        self.envelope.start = true;
    }

    #[inline]
    fn clock_timer(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.timer_period;
            self.duty_pos = (self.duty_pos + 1) & 7;
            true
        } else {
            self.timer -= 1;
            false
        }
    }

    fn sweep_target(&self) -> i32 {
        let period = self.timer_period as i32;
        let change = period >> self.sweep_shift;
        if self.sweep_negate {
            period - change - if self.is_pulse1 { 1 } else { 0 }
        } else {
            period + change
        }
    }

    fn update_muted(&mut self) {
        self.muted = self.timer_period < 8 || self.sweep_target() > 0x7FF;
    }

    #[inline]
    fn is_muted(&self) -> bool {
        self.muted
    }

    fn clock_sweep(&mut self) {
        if self.sweep_divider == 0 && self.sweep_enabled && self.sweep_shift > 0 && !self.is_muted()
        {
            self.timer_period = self.sweep_target().max(0) as u16;
            self.update_muted();
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if !self.length.active()
            || self.is_muted()
            || DUTY_TABLE[self.duty as usize][self.duty_pos as usize] == 0
        {
            0
        } else {
            self.envelope.output()
        }
    }
}

#[derive(Default)]
struct Triangle {
    length: LengthCounter,
    control: bool,
    linear_reload_value: u8,
    linear_counter: u8,
    linear_reload: bool,
    timer_period: u16,
    timer: u16,
    seq_pos: u8,
}

impl Triangle {
    fn write_linear(&mut self, value: u8) {
        self.control = value & 0x80 != 0;
        self.length.set_halt(self.control);
        self.linear_reload_value = value & 0x7f;
    }

    fn write_timer_lo(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x0700) | value as u16;
    }

    fn write_timer_hi(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x00ff) | (((value & 0x07) as u16) << 8);
        self.length.load(value >> 3);
        self.linear_reload = true;
    }

    #[inline]
    fn clock_timer(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.timer_period;
            if self.length.active() && self.linear_counter > 0 && self.timer_period >= 2 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
            true
        } else {
            self.timer -= 1;
            false
        }
    }

    fn clock_linear(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control {
            self.linear_reload = false;
        }
    }

    fn output(&self) -> u8 {
        TRIANGLE_TABLE[self.seq_pos as usize]
    }
}

struct Noise {
    length: LengthCounter,
    envelope: Envelope,
    mode: bool,
    timer_period: u16,
    timer: u16,
    shift: u16,
}

impl Noise {
    fn new() -> Self {
        Noise {
            length: LengthCounter::default(),
            envelope: Envelope::default(),
            mode: false,
            timer_period: Region::Ntsc.noise_periods()[0],
            timer: 0,
            shift: 1,
        }
    }

    fn write_control(&mut self, value: u8) {
        self.length.set_halt(value & 0x20 != 0);
        self.envelope.write(value);
    }

    fn write_period(&mut self, value: u8, periods: &[u16; 16]) {
        self.mode = value & 0x80 != 0;
        self.timer_period = periods[(value & 0x0F) as usize];
    }

    fn write_length(&mut self, value: u8) {
        self.length.load(value >> 3);
        self.envelope.start = true;
    }

    #[inline]
    fn clock_timer(&mut self) -> bool {
        if self.timer == 0 {
            self.timer = self.timer_period - 1;
            let tap = if self.mode { 6 } else { 1 };
            let feedback = (self.shift & 1) ^ ((self.shift >> tap) & 1);
            self.shift = (self.shift >> 1) | (feedback << 14);
            true
        } else {
            self.timer -= 1;
            false
        }
    }

    fn output(&self) -> u8 {
        if !self.length.active() || self.shift & 1 == 1 {
            0
        } else {
            self.envelope.output()
        }
    }
}

struct Dmc {
    irq_enabled: bool,
    loop_flag: bool,
    rate: u16,
    timer: u16,
    output_level: u8,
    sample_addr: u16,
    sample_len: u16,
    current_addr: u16,
    bytes_remaining: u16,
    sample_buffer: Option<u8>,
    shift_reg: u8,
    bits_remaining: u8,
    silence: bool,
    irq_flag: bool,
    /// CPU cycles until a sample started by $4015 asks for its first byte.
    start_delay: u8,
    disable_delay: u8,
    stop_dma: bool,
    reloaded: bool,
}

impl Dmc {
    fn new() -> Self {
        Dmc {
            irq_enabled: false,
            loop_flag: false,
            rate: Region::Ntsc.dmc_rates()[0],
            timer: 0,
            output_level: 0,
            sample_addr: 0xc000,
            sample_len: 1,
            current_addr: 0xc000,
            bytes_remaining: 0,
            sample_buffer: None,
            shift_reg: 0,
            bits_remaining: 8,
            silence: true,
            irq_flag: false,
            start_delay: 0,
            disable_delay: 0,
            stop_dma: false,
            reloaded: false,
        }
    }

    fn write_control(&mut self, value: u8, rates: &[u16; 16]) {
        self.irq_enabled = value & 0x80 != 0;
        if !self.irq_enabled {
            self.irq_flag = false;
        }
        self.loop_flag = value & 0x40 != 0;
        self.rate = rates[(value & 0x0F) as usize];

        if self.reloaded {
            self.timer = self.rate - 1;
        }
    }

    fn write_direct_load(&mut self, value: u8) {
        self.output_level = value & 0x7f;
    }

    fn write_sample_addr(&mut self, value: u8) {
        self.sample_addr = 0xc000 | ((value as u16) << 6);
    }

    fn write_sample_len(&mut self, value: u8) {
        self.sample_len = ((value as u16) << 4) | 1;
    }

    fn restart(&mut self) {
        self.current_addr = self.sample_addr;
        self.bytes_remaining = self.sample_len;
    }

    /// A $4015 write. Turning the DMC off takes 2 or 3 cycles (1 APU cycle
    /// plus alignment), and a fetch that starts in that time gets aborted.
    fn set_enabled(&mut self, enabled: bool, even_cycle: bool) {
        if !enabled {
            if self.disable_delay == 0 {
                self.disable_delay = if even_cycle { 2 } else { 3 };
            }
        } else if self.bytes_remaining == 0 {
            self.restart();
            self.start_delay = if even_cycle { 2 } else { 3 };
        }
    }

    /// Runs the delayed $4015 writes; called once per CPU cycle.
    fn clock_delays(&mut self) {
        if self.disable_delay > 0 {
            self.disable_delay -= 1;
            if self.disable_delay == 0 {
                self.bytes_remaining = 0;
                self.stop_dma = true;
            }
        }
        if self.start_delay > 0 {
            self.start_delay -= 1;
        }
    }

    fn fetch_addr(&self) -> Option<u16> {
        if self.start_delay == 0 && self.sample_buffer.is_none() && self.bytes_remaining > 0 {
            Some(self.current_addr)
        } else {
            None
        }
    }

    fn provide_byte(&mut self, byte: u8) {
        if self.bytes_remaining == 0 {
            return;
        }

        self.sample_buffer = Some(byte);
        self.current_addr = if self.current_addr == 0xffff {
            0x8000
        } else {
            self.current_addr + 1
        };
        self.bytes_remaining -= 1;
        if self.bytes_remaining == 0 {
            if self.loop_flag {
                self.restart();
            } else if self.irq_enabled {
                self.irq_flag = true;
            }
        }

        // Implicit abort: when the last byte of a 1-byte sample arrives just
        // before the output unit finishes its byte, the output unit takes
        // the new byte at once and asks for another one, which the DMA unit
        // starts and then drops: one halted CPU cycle. (Follows Mesen.)
        if self.sample_len == 1 && !self.loop_flag && self.bits_remaining == 1 && self.timer < 2 {
            self.restart();
            self.disable_delay = 3;
        }
    }

    #[inline]
    fn clock_timer(&mut self) -> bool {
        self.reloaded = self.timer == 0;
        if self.timer > 0 {
            self.timer -= 1;
            return false;
        }
        self.timer = self.rate - 1;

        if !self.silence {
            if self.shift_reg & 1 == 1 {
                if self.output_level <= 125 {
                    self.output_level += 2;
                }
            } else if self.output_level >= 2 {
                self.output_level -= 2;
            }
        }
        self.shift_reg >>= 1;
        self.bits_remaining -= 1;

        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            match self.sample_buffer.take() {
                Some(byte) => {
                    self.shift_reg = byte;
                    self.silence = false;
                }
                None => self.silence = true,
            }
        }
        true
    }

    fn output(&self) -> u8 {
        self.output_level
    }
}

struct HighPass {
    alpha: f32,
    prev_in: f32,
    prev_out: f32,
}

impl HighPass {
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let dt = 1.0 / sample_rate;
        HighPass {
            alpha: rc / (rc + dt),
            prev_in: 0.0,
            prev_out: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.alpha * (self.prev_out + x - self.prev_in);
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

struct LowPass {
    alpha: f32,
    prev_out: f32,
}

impl LowPass {
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let dt = 1.0 / sample_rate;
        LowPass {
            alpha: dt / (rc + dt),
            prev_out: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.prev_out += self.alpha * (x - self.prev_out);
        self.prev_out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameClock {
    None,
    Quarter,
    Half,
}

/// The step types, the same for both modes: the 4-step sequence's steps 3
/// and 5 and the 5-step sequence's steps 3 and 5 only mark timing.
const FRAME_CLOCKS: [FrameClock; 6] = [
    FrameClock::Quarter,
    FrameClock::Half,
    FrameClock::Quarter,
    FrameClock::None,
    FrameClock::Half,
    FrameClock::None,
];

struct FrameCounter {
    steps: [[u32; 6]; 2],
    /// 0 = 4-step, 1 = 5-step
    mode: usize,
    step: usize,
    cycle: u32,
    irq_inhibit: bool,
    irq_flag: bool,
    /// A $4017 write takes effect 3 or 4 CPU cycles later, depending on
    /// whether it lands on an APU cycle (every other CPU cycle).
    pending_write: Option<u8>,
    write_delay: u8,
    clear_delay: u8,
    /// A clock from the sequence and one from a 5-step-mode write landing
    /// within two cycles of each other only clock the units once.
    block_clocks: u8,
    last_write: u8,
}

impl FrameCounter {
    fn new(timing: FrameSequencerTiming) -> Self {
        let mut counter = FrameCounter {
            steps: [[0; 6]; 2],
            mode: 0,
            step: 0,
            // At power-on the APU acts as if $4017 had been written with $00
            // 9-12 cycles before the first instruction. Starting the sequence
            // at power-on does that
            cycle: 0,
            irq_inhibit: false,
            irq_flag: false,
            pending_write: None,
            write_delay: 0,
            clear_delay: 0,
            block_clocks: 0,
            last_write: 0,
        };
        counter.set_timing(timing);
        counter
    }

    fn set_timing(&mut self, t: FrameSequencerTiming) {
        let [q1, h1, q2] = t.steps;
        self.steps = [
            [
                q1,
                h1,
                q2,
                t.four_step_end - 1,
                t.four_step_end,
                t.four_step_end + 1,
            ],
            [
                q1,
                h1,
                q2,
                t.four_step_end,
                t.five_step_end,
                t.five_step_end + 1,
            ],
        ];
    }

    fn reset(&mut self) {
        let value = self.last_write;
        self.mode = (value >> 7) as usize;
        self.step = 0;
        self.cycle = 0;
        self.irq_inhibit = value & 0x40 != 0;
        self.irq_flag = false;
        self.clear_delay = 0;
        self.pending_write = None;
        self.block_clocks = 0
    }

    fn write(&mut self, value: u8, apu_cycle: bool) {
        self.last_write = value;
        self.pending_write = Some(value);
        self.write_delay = if apu_cycle { 3 } else { 4 };
        self.irq_inhibit = value & 0x40 != 0;
        if self.irq_inhibit {
            self.irq_flag = false;
        }
    }

    fn tick(&mut self) -> FrameClock {
        let mut clock = FrameClock::None;
        if self.clear_delay > 0 {
            self.clear_delay -= 1;
            if self.clear_delay == 0 {
                self.irq_flag = false;
            }
        }
        self.cycle += 1;
        if self.cycle == self.steps[self.mode][self.step] {
            // The 4-step sequence sets the flag on its last three cycles.
            // With interrupts inhibited it is still set on the first two,
            // where $4015 can see it, and cleared on the third.
            if self.mode == 0 && self.step >= 3 {
                self.irq_flag = !(self.irq_inhibit && self.step == 5);
            }
            let step_clock = FRAME_CLOCKS[self.step];
            if step_clock != FrameClock::None && self.block_clocks == 0 {
                clock = step_clock;
                self.block_clocks = 2;
            }
            self.step += 1;
            if self.step == 6 {
                self.step = 0;
                self.cycle = 0;
            }
        }

        if let Some(value) = self.pending_write {
            self.write_delay -= 1;
            if self.write_delay == 0 {
                self.pending_write = None;
                self.mode = (value >> 7) as usize;
                self.step = 0;
                self.cycle = 0;
                if self.mode == 1 && self.block_clocks == 0 {
                    clock = FrameClock::Half;
                    self.block_clocks = 2;
                }
            }
        }

        if self.block_clocks > 0 {
            self.block_clocks -= 1;
        }

        clock
    }
}

pub struct APU {
    pulse1: Pulse,
    pulse2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,

    frame_counter: FrameCounter,
    length_writes_pending: bool,
    odd_cycle: bool,

    pulse_table: [f32; 31],
    tnd_table: [f32; 203],

    region: Region,
    cpu_clock: f64,

    sample_rate: f64,
    sample_phase: f64,
    sample_sum: f32,
    sample_count: u32,
    rate_adjust: f64,
    high_pass_90: HighPass,
    high_pass_440: HighPass,
    low_pass_14k: LowPass,
    samples: Vec<f32>,

    mix_cache: f32,
    mix_dirty: bool,

    expansion_audio: f32
}

impl APU {
    pub fn new(sample_rate: u32) -> Self {
        let mut pulse_table = [0.0f32; 31];
        for (n, v) in pulse_table.iter_mut().enumerate().skip(1) {
            *v = 95.52 / (8128.0 / n as f32 + 100.0);
        }

        let mut tnd_table = [0.0f32; 203];
        for (n, v) in tnd_table.iter_mut().enumerate().skip(1) {
            *v = 163.67 / (24329.0 / n as f32 + 100.0);
        }

        APU {
            pulse1: Pulse::new(true),
            pulse2: Pulse::new(false),
            triangle: Triangle::default(),
            noise: Noise::new(),
            dmc: Dmc::new(),
            frame_counter: FrameCounter::new(Region::Ntsc.frame_sequencer()),
            length_writes_pending: false,
            odd_cycle: false,
            pulse_table,
            tnd_table,
            region: Region::Ntsc,
            cpu_clock: Region::Ntsc.cpu_clock_hz(),
            sample_rate: sample_rate as f64,
            sample_phase: 0.0,
            sample_sum: 0.0,
            sample_count: 0,
            rate_adjust: 1.0,
            high_pass_90: HighPass::new(90.0, sample_rate as f32),
            high_pass_440: HighPass::new(440.0, sample_rate as f32),
            low_pass_14k: LowPass::new(14000.0, sample_rate as f32),
            samples: Vec::with_capacity(sample_rate as usize / 30),
            mix_cache: 0.0,
            mix_dirty: true,
            expansion_audio: 0.0
        }
    }

    pub fn reset(&mut self) {
        self.write_register(0x4015, 0x00);
        self.frame_counter.reset();
        self.dmc.bytes_remaining = 0;
        self.dmc.disable_delay = 0;
    }

    pub fn set_expansion_audio(&mut self, level: f32) {
        self.expansion_audio = level;
    }

    pub fn set_rate_adjust(&mut self, ratio: f64) {
        self.rate_adjust = ratio;
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.sample_rate = sample_rate as f64;
        self.high_pass_90 = HighPass::new(90.0, sample_rate as f32);
        self.high_pass_440 = HighPass::new(440.0, sample_rate as f32);
    }

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.cpu_clock = region.cpu_clock_hz();
        self.frame_counter.set_timing(region.frame_sequencer());
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
        self.mix_dirty = true;
        if matches!(
            addr,
            0x4000 | 0x4003 | 0x4004 | 0x4007 | 0x4008 | 0x400B | 0x400C | 0x400F
        ) {
            self.length_writes_pending = true;
        }
        match addr {
            0x4000 => self.pulse1.write_control(value),
            0x4001 => self.pulse1.write_sweep(value),
            0x4002 => self.pulse1.write_timer_lo(value),
            0x4003 => self.pulse1.write_timer_hi(value),
            0x4004 => self.pulse2.write_control(value),
            0x4005 => self.pulse2.write_sweep(value),
            0x4006 => self.pulse2.write_timer_lo(value),
            0x4007 => self.pulse2.write_timer_hi(value),
            0x4008 => self.triangle.write_linear(value),
            0x4009 => {} // unused
            0x400a => self.triangle.write_timer_lo(value),
            0x400b => self.triangle.write_timer_hi(value),
            0x400c => self.noise.write_control(value),
            0x400d => {} // unused
            0x400e => self.noise.write_period(value, self.region.noise_periods()),
            0x400f => self.noise.write_length(value),
            0x4010 => self.dmc.write_control(value, self.region.dmc_rates()),
            0x4011 => self.dmc.write_direct_load(value),
            0x4012 => self.dmc.write_sample_addr(value),
            0x4013 => self.dmc.write_sample_len(value),
            0x4015 => {
                self.pulse1.length.set_enabled(value & 0x01 != 0);
                self.pulse2.length.set_enabled(value & 0x02 != 0);
                self.triangle.length.set_enabled(value & 0x04 != 0);
                self.noise.length.set_enabled(value & 0x08 != 0);
                self.dmc.set_enabled(value & 0x10 != 0, !self.odd_cycle);
                self.dmc.irq_flag = false;
            }
            0x4017 => self.frame_counter.write(value, !self.odd_cycle),
            _ => {}
        }
    }

    /// $4015 read: which channels are still playing, plus the frame (bit 6)
    /// and DMC (bit 7) interrupt flags. Reading clears the frame flag, but
    /// not the DMC one. Bit 5 is open bus (the bus fills it in).
    pub fn read_status(&mut self) -> u8 {
        let mut status = 0;
        if self.pulse1.length.active() {
            status |= 0x01;
        }
        if self.pulse2.length.active() {
            status |= 0x02;
        }
        if self.triangle.length.active() {
            status |= 0x04;
        }
        if self.noise.length.active() {
            status |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            status |= 0x10;
        }
        if self.frame_counter.irq_flag {
            status |= 0x40;
        }
        // the read clears the frame flag at the next put-to-get transition:
        // 1 cycle away on a put cycle, 2 on a get cycle
        if self.frame_counter.clear_delay == 0 {
            self.frame_counter.clear_delay = if self.get_cycle() { 2 } else { 1 };
        }
        if self.dmc.irq_flag {
            status |= 0x80;
        }
        status
    }

    /// Whether the current CPU cycle is a "get" cycle, the first half of an
    /// APU cycle (the DMA unit reads on these; see Bus::run_dma).
    fn get_cycle(&self) -> bool {
        self.odd_cycle
    }

    pub fn irq_pending(&self) -> bool {
        (self.frame_counter.irq_flag && !self.frame_counter.irq_inhibit) || self.dmc.irq_flag
    }

    pub fn take_dmc_stop(&mut self) -> bool {
        std::mem::take(&mut self.dmc.stop_dma)
    }

    pub fn dmc_addr(&mut self) -> u16 {
        self.dmc.current_addr
    }

    pub fn dmc_fetch_addr(&self) -> Option<u16> {
        self.dmc.fetch_addr()
    }

    pub fn dmc_provide_byte(&mut self, byte: u8) {
        self.mix_dirty = true;
        self.dmc.provide_byte(byte);
    }

    fn quarter_frame(&mut self) {
        self.mix_dirty = true;
        self.pulse1.envelope.clock();
        self.pulse2.envelope.clock();
        self.noise.envelope.clock();
        self.triangle.clock_linear();
    }

    fn half_frame(&mut self) {
        self.mix_dirty = true;
        self.pulse1.length.clock();
        self.pulse1.clock_sweep();
        self.pulse2.length.clock();
        self.pulse2.clock_sweep();
        self.triangle.length.clock();
        self.noise.length.clock();
    }

    fn clock_frame_sequencer(&mut self) {
        match self.frame_counter.tick() {
            FrameClock::None => {}
            FrameClock::Quarter => self.quarter_frame(),
            FrameClock::Half => {
                self.quarter_frame();
                self.half_frame();
            }
        }

        if self.length_writes_pending {
            self.length_writes_pending = false;
            self.mix_dirty = true;
            self.pulse1.length.apply_writes();
            self.pulse2.length.apply_writes();
            self.triangle.length.apply_writes();
            self.noise.length.apply_writes();
        }
    }

    fn mix(&self) -> f32 {
        let pulse = self.pulse1.output() + self.pulse2.output();
        let tnd = 3 * self.triangle.output() as usize
            + 2 * self.noise.output() as usize
            + self.dmc.output() as usize;
        self.pulse_table[pulse as usize] + self.tnd_table[tnd]
    }

    pub fn tick(&mut self) {
        self.clock_frame_sequencer();
        self.dmc.clock_delays();

        let mut changed = self.triangle.clock_timer();
        changed |= self.noise.clock_timer();
        changed |= self.dmc.clock_timer();
        if self.odd_cycle {
            changed |= self.pulse1.clock_timer();
            changed |= self.pulse2.clock_timer();
        }
        self.odd_cycle = !self.odd_cycle;

        if changed || self.mix_dirty {
            self.mix_cache = self.mix();
            self.mix_dirty = false;
        }

        self.sample_sum += self.mix_cache + self.expansion_audio;
        self.sample_count += 1;
        self.sample_phase += self.sample_rate * self.rate_adjust;
        if self.sample_phase >= self.cpu_clock {
            self.sample_phase -= self.cpu_clock;
            let raw = self.sample_sum / self.sample_count as f32;
            self.sample_sum = 0.0;
            self.sample_count = 0;
            let filtered = self.high_pass_440.process(self.high_pass_90.process(raw));
            self.samples.push(self.low_pass_14k.process(filtered));
        }
    }

    pub fn take_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }
}

impl LengthCounter {
    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.enabled);
        w.u8(self.counter);
        w.bool(self.halt);
        w.bool(self.new_halt);
        w.u8(self.reload);
        w.u8(self.previous);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.enabled = r.bool();
        self.counter = r.u8();
        self.halt = r.bool();
        self.new_halt = r.bool();
        self.reload = r.u8();
        self.previous = r.u8();
    }
}

impl Envelope {
    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.start);
        w.bool(self.loop_flag);
        w.bool(self.constant);
        w.u8(self.volume);
        w.u8(self.divider);
        w.u8(self.decay);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.start = r.bool();
        self.loop_flag = r.bool();
        self.constant = r.bool();
        self.volume = r.u8();
        self.divider = r.u8();
        self.decay = r.u8();
    }
}

impl Pulse {
    fn save_state(&self, w: &mut StateWriter) {
        self.length.save_state(w);
        w.u8(self.duty);
        w.u8(self.duty_pos);
        w.u16(self.timer_period);
        w.u16(self.timer);
        self.envelope.save_state(w);
        w.bool(self.sweep_enabled);
        w.u8(self.sweep_period);
        w.bool(self.sweep_negate);
        w.u8(self.sweep_shift);
        w.u8(self.sweep_divider);
        w.bool(self.sweep_reload);
        w.bool(self.muted);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.length.load_state(r);
        self.duty = r.u8();
        self.duty_pos = r.u8();
        self.timer_period = r.u16();
        self.timer = r.u16();
        self.envelope.load_state(r);
        self.sweep_enabled = r.bool();
        self.sweep_period = r.u8();
        self.sweep_negate = r.bool();
        self.sweep_shift = r.u8();
        self.sweep_divider = r.u8();
        self.sweep_reload = r.bool();
        self.muted = r.bool();
    }
}

impl Triangle {
    fn save_state(&self, w: &mut StateWriter) {
        self.length.save_state(w);
        w.bool(self.control);
        w.u8(self.linear_reload_value);
        w.u8(self.linear_counter);
        w.bool(self.linear_reload);
        w.u16(self.timer_period);
        w.u16(self.timer);
        w.u8(self.seq_pos);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.length.load_state(r);
        self.control = r.bool();
        self.linear_reload_value = r.u8();
        self.linear_counter = r.u8();
        self.linear_reload = r.bool();
        self.timer_period = r.u16();
        self.timer = r.u16();
        self.seq_pos = r.u8();
    }
}

impl Noise {
    fn save_state(&self, w: &mut StateWriter) {
        self.length.save_state(w);
        self.envelope.save_state(w);
        w.bool(self.mode);
        w.u16(self.timer_period);
        w.u16(self.timer);
        w.u16(self.shift);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.length.load_state(r);
        self.envelope.load_state(r);
        self.mode = r.bool();
        self.timer_period = r.u16();
        self.timer = r.u16();
        self.shift = r.u16();
    }
}

impl Dmc {
    fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.irq_enabled);
        w.bool(self.loop_flag);
        w.u16(self.rate);
        w.u16(self.timer);
        w.u8(self.output_level);
        w.u16(self.sample_addr);
        w.u16(self.sample_len);
        w.u16(self.current_addr);
        w.u16(self.bytes_remaining);
        w.option_u8(self.sample_buffer);
        w.u8(self.shift_reg);
        w.u8(self.bits_remaining);
        w.bool(self.silence);
        w.bool(self.irq_flag);
        w.u8(self.start_delay);
        w.u8(self.disable_delay);
        w.bool(self.stop_dma);
        w.bool(self.reloaded);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.irq_enabled = r.bool();
        self.loop_flag = r.bool();
        self.rate = r.u16();
        self.timer = r.u16();
        self.output_level = r.u8();
        self.sample_addr = r.u16();
        self.sample_len = r.u16();
        self.current_addr = r.u16();
        self.bytes_remaining = r.u16();
        self.sample_buffer = r.option_u8();
        self.shift_reg = r.u8();
        self.bits_remaining = r.u8();
        self.silence = r.bool();
        self.irq_flag = r.bool();
        self.start_delay = r.u8();
        self.disable_delay = r.u8();
        self.stop_dma = r.bool();
        self.reloaded = r.bool();
    }
}

impl FrameCounter {
    fn save_state(&self, w: &mut StateWriter) {
        w.usize(self.mode);
        w.usize(self.step);
        w.u32(self.cycle);
        w.bool(self.irq_inhibit);
        w.bool(self.irq_flag);
        w.option_u8(self.pending_write);
        w.u8(self.write_delay);
        w.u8(self.clear_delay);
        w.u8(self.block_clocks);
        w.u8(self.last_write);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.mode = r.usize() & 1;
        self.step = r.usize().min(5);
        self.cycle = r.u32();
        self.irq_inhibit = r.bool();
        self.irq_flag = r.bool();
        self.pending_write = r.option_u8();
        self.write_delay = r.u8();
        self.clear_delay = r.u8();
        self.block_clocks = r.u8();
        self.last_write = r.u8();
    }
}

impl APU {
    pub fn save_state(&self, w: &mut StateWriter) {
        self.pulse1.save_state(w);
        self.pulse2.save_state(w);
        self.triangle.save_state(w);
        self.noise.save_state(w);
        self.dmc.save_state(w);
        self.frame_counter.save_state(w);
        w.bool(self.length_writes_pending);
        w.bool(self.odd_cycle);
        w.f64(self.sample_phase);
        w.f32(self.sample_sum);
        w.u32(self.sample_count);
        w.f64(self.rate_adjust);
        for filter in [&self.high_pass_90, &self.high_pass_440] {
            w.f32(filter.prev_in);
            w.f32(filter.prev_out);
        }
        w.f32(self.low_pass_14k.prev_out);
        w.u32(self.samples.len() as u32);
        for &sample in &self.samples {
            w.f32(sample);
        }
        w.f32(self.mix_cache);
        w.bool(self.mix_dirty);
    }

    pub fn load_state(&mut self, r: &mut StateReader) {
        self.pulse1.load_state(r);
        self.pulse2.load_state(r);
        self.triangle.load_state(r);
        self.noise.load_state(r);
        self.dmc.load_state(r);
        self.frame_counter.load_state(r);
        self.length_writes_pending = r.bool();
        self.odd_cycle = r.bool();
        self.sample_phase = r.f64();
        self.sample_sum = r.f32();
        self.sample_count = r.u32();
        self.rate_adjust = r.f64();
        for filter in [&mut self.high_pass_90, &mut self.high_pass_440] {
            filter.prev_in = r.f32();
            filter.prev_out = r.f32();
        }
        self.low_pass_14k.prev_out = r.f32();
        let count = (r.u32() as usize).min(48_000);
        self.samples.clear();
        for _ in 0..count {
            self.samples.push(r.f32());
        }
        self.mix_cache = r.f32();
        self.mix_dirty = r.bool();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_status_reflects_length_counters() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4015, 0x01); // enable pulse 1
        apu.write_register(0x4003, 0x08); // length index 1 -> 254
        apu.tick();
        assert_eq!(apu.read_status() & 0x01, 0x01);

        apu.write_register(0x4015, 0x00); // disabling clears the length counter
        assert_eq!(apu.read_status() & 0x01, 0x00);
    }

    #[test]
    fn test_length_not_loaded_while_disabled() {
        let mut apu = APU::new(44100);
        apu.write_register(0x400b, 0xf8);
        assert_eq!(apu.read_status() & 0x04, 0);
    }

    #[test]
    fn test_length_counter_runs_out() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4000, 0x0f);
        apu.write_register(0x4003, 0x18); // length index 3 -> 2 half frames
        // two half frames happen within one 4-step sequence (29830 cycles)
        for _ in 0..29830 {
            apu.tick();
        }
        assert_eq!(apu.read_status() & 0x01, 0);
    }

    #[test]
    fn test_produces_samples_at_requested_rate() {
        let mut apu = APU::new(44100);
        for _ in 0..(Region::Ntsc.cpu_clock_hz() as usize) {
            apu.tick();
        }
        let n = apu.take_samples().len() as i32;
        assert!((n - 44100).abs() <= 1, "got {} samples", n);
    }

    #[test]
    fn test_pulse_makes_sound() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4000, 0b1011_1111); // 50% duty, halt, constant volume 15
        apu.write_register(0x4002, 0xfd); // ~440 Hz
        apu.write_register(0x4003, 0x00);
        for _ in 0..30000 {
            apu.tick();
        }
        let samples = apu.take_samples();
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "peak {}", peak);
    }

    #[test]
    fn test_dmc_fetches_from_sample_address() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4012, 0x01); // $C040
        apu.write_register(0x4013, 0x00); // 1 byte
        apu.write_register(0x4015, 0x10);
        assert_eq!(
            apu.dmc_fetch_addr(),
            None,
            "the first fetch waits 2-3 cycles"
        );
        for _ in 0..3 {
            apu.tick();
        }
        assert_eq!(apu.dmc_fetch_addr(), Some(0xc040));
        apu.dmc_provide_byte(0xff);
        assert_eq!(apu.dmc_fetch_addr(), None);
        assert_eq!(apu.read_status() & 0x10, 0);
    }

    #[test]
    fn test_rate_adjust_changes_sample_count() {
        let mut apu = APU::new(44100);
        apu.set_rate_adjust(1.005);
        for _ in 0..(Region::Ntsc.cpu_clock_hz() as usize) {
            apu.tick();
        }
        let n = apu.take_samples().len() as i32;
        assert!((n - 44320).abs() <= 1, "got {} samples", n);
    }

    #[test]
    fn test_pal_produces_samples_at_requested_rate() {
        let mut apu = APU::new(44100);
        apu.set_region(Region::Pal);
        for _ in 0..(Region::Pal.cpu_clock_hz() as usize) {
            apu.tick();
        }
        let n = apu.take_samples().len() as i32;
        assert!((n - 44100).abs() <= 1, "got {} samples", n);
    }

    #[test]
    fn test_region_rate_tables() {
        let mut apu = APU::new(44100);
        apu.set_region(Region::Pal);
        apu.write_register(0x400e, 0x0f);
        apu.write_register(0x4010, 0x0f);
        assert_eq!(apu.noise.timer_period, 3778);
        assert_eq!(apu.dmc.rate, 50);

        apu.set_region(Region::Dendy); // NTSC-style APU
        apu.write_register(0x400e, 0x0f);
        assert_eq!(apu.noise.timer_period, 4068);
    }

    /// CPU cycles between the first two half-frame clocks (length counter ticks).
    fn half_frame_interval(region: Region) -> u32 {
        let mut apu = APU::new(44100);
        apu.set_region(region);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4000, 0x00); // length counter not halted
        apu.write_register(0x4003, 0x18);
        apu.tick(); // length 2
        let mut ticks = vec![];
        let mut cycle = 0u32;
        while ticks.len() < 2 {
            let before = apu.pulse1.length.counter;
            apu.tick();
            cycle += 1;
            if apu.pulse1.length.counter != before {
                ticks.push(cycle);
            }
        }
        ticks[1] - ticks[0]
    }

    #[test]
    fn test_frame_sequencer_timing_by_region() {
        // from the mid-frame half clock to the end-of-sequence one
        assert_eq!(half_frame_interval(Region::Ntsc), 29829 - 14913);
        assert_eq!(half_frame_interval(Region::Pal), 33253 - 16627);
        assert_eq!(half_frame_interval(Region::Dendy), 29829 - 14913);
    }

    fn run(apu: &mut APU, cycles: u32) {
        for _ in 0..cycles {
            apu.tick();
        }
    }

    /// Writes $4017 and waits until the write has landed (at most 4 cycles).
    fn write_frame_counter(apu: &mut APU, value: u8) {
        apu.write_register(0x4017, value);
        run(apu, 4);
    }

    #[test]
    fn test_frame_irq_set_in_4_step_mode_and_cleared_by_read() {
        let mut apu = APU::new(44100);
        write_frame_counter(&mut apu, 0x00);
        run(&mut apu, 29826);
        assert!(!apu.irq_pending());
        run(&mut apu, 5);
        assert!(apu.irq_pending());
        assert_eq!(apu.read_status() & 0x40, 0x40);
        run(&mut apu, 2);
        assert!(!apu.irq_pending(), "reading $4015 acknowledges it");
        assert_eq!(apu.read_status() & 0x40, 0);
    }

    #[test]
    fn test_frame_irq_inhibit_and_5_step_mode() {
        let mut apu = APU::new(44100);
        write_frame_counter(&mut apu, 0x40); // inhibit
        run(&mut apu, 40000);
        assert!(!apu.irq_pending());

        write_frame_counter(&mut apu, 0x80); // 5-step: never raises it
        run(&mut apu, 40000);
        assert!(!apu.irq_pending());

        write_frame_counter(&mut apu, 0x00);
        run(&mut apu, 30000);
        assert!(apu.irq_pending());
        apu.write_register(0x4017, 0x40); // setting inhibit clears the flag at once
        assert!(!apu.irq_pending());
    }

    #[test]
    fn test_frame_counter_write_lands_after_3_or_4_cycles() {
        // writing $80 clocks the length counters when the write lands
        for parity in 0..2 {
            let mut apu = APU::new(44100);
            run(&mut apu, 10 + parity);
            apu.write_register(0x4015, 0x01);
            apu.write_register(0x4003, 0x18); // length 2
            apu.tick();
            apu.write_register(0x4017, 0x80);
            let mut delay = 0;
            while apu.pulse1.length.counter == 2 {
                apu.tick();
                delay += 1;
            }
            assert!(delay == 3 || delay == 4, "landed after {}", delay);
        }
    }

    #[test]
    fn test_length_reload_on_a_clock_cycle_is_ignored() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4003, 0x18); // length 2
        apu.tick();
        apu.write_register(0x4017, 0x80); // lands in 3 or 4 cycles and clocks
        let mut cycles = 0;
        while apu.pulse1.length.counter == 2 {
            // keep reloading: the reload that coincides with the clock is lost
            apu.write_register(0x4003, 0x18);
            apu.tick();
            cycles += 1;
            assert!(cycles < 10);
        }
        assert_eq!(apu.pulse1.length.counter, 1);
    }

    #[test]
    fn test_dmc_irq_when_sample_ends() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4010, 0x80); // IRQ on, no loop
        apu.write_register(0x4013, 0x00); // 1 byte
        apu.write_register(0x4015, 0x10);
        apu.dmc_provide_byte(0x00);
        assert!(apu.irq_pending());
        assert_eq!(apu.read_status() & 0x80, 0x80);
        assert_eq!(apu.read_status() & 0x80, 0x80, "reading doesn't clear it");
        apu.write_register(0x4015, 0x00); // writing $4015 does
        assert!(!apu.irq_pending());

        apu.write_register(0x4015, 0x10);
        apu.dmc_provide_byte(0x00);
        assert!(apu.irq_pending());
        apu.write_register(0x4010, 0x00); // so does disabling the IRQ
        assert!(!apu.irq_pending());
    }

    #[test]
    fn test_looping_dmc_never_raises_irq() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4010, 0xC0); // IRQ on, loop
        apu.write_register(0x4013, 0x00);
        apu.write_register(0x4015, 0x10);
        for _ in 0..5 {
            apu.dmc_provide_byte(0x00);
        }
        assert!(!apu.irq_pending());
        assert_eq!(apu.read_status() & 0x10, 0x10, "still playing");
    }

    #[test]
    fn test_reset_silences_channels_and_restarts_frame_counter() {
        let mut apu = APU::new(44100);
        write_frame_counter(&mut apu, 0x00);
        apu.write_register(0x4015, 0x0F);
        apu.write_register(0x4003, 0x08); // length 254
        run(&mut apu, 30000);
        assert!(apu.irq_pending());
        assert_eq!(apu.read_status() & 0x01, 0x01);

        apu.reset();
        assert!(!apu.irq_pending(), "the frame IRQ flag is cleared");
        assert_eq!(apu.read_status() & 0x0F, 0, "$4015 is cleared");
        // $4017 = $00 was rewritten, so the IRQ comes back a sequence later
        run(&mut apu, 29831);
        assert!(apu.irq_pending());
    }

    #[test]
    fn test_dmc_turns_off_two_or_three_cycles_after_the_write() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4013, 0x01); // 17 bytes
        apu.write_register(0x4015, 0x10);
        apu.write_register(0x4015, 0x00);
        assert_eq!(apu.read_status() & 0x10, 0x10, "still playing");
        let mut cycles = 0;
        while !apu.take_dmc_stop() {
            apu.tick();
            cycles += 1;
        }
        assert!(cycles == 2 || cycles == 3, "took {}", cycles);
        assert_eq!(apu.read_status() & 0x10, 0);
    }

    #[test]
    fn test_dmc_rate_written_as_the_timer_reloads_is_used_at_once() {
        let rates = Region::Ntsc.dmc_rates();
        for (wait, expected) in [(0, rates[0] - 1), (1, rates[15] - 2)] {
            let mut apu = APU::new(44100);
            apu.write_register(0x4010, 0x0F); // fastest rate
            apu.dmc.timer = 0;
            apu.tick(); // reloads with the fastest rate
            for _ in 0..wait {
                apu.tick();
            }
            apu.write_register(0x4010, 0x00); // slowest rate
            assert_eq!(apu.dmc.timer, expected, "written {} cycles later", wait);
        }
    }

    #[test]
    fn test_frame_irq_clear_waits_for_a_get_cycle() {
        // a read on a get cycle clears the flag 2 cycles later, on a put
        // cycle 1 cycle later
        for extra in 0..2 {
            let mut apu = APU::new(44100);
            write_frame_counter(&mut apu, 0x00);
            run(&mut apu, 29840 + extra);
            let delay = if apu.get_cycle() { 2 } else { 1 };
            apu.read_status();
            for _ in 1..delay {
                apu.tick();
                assert!(apu.irq_pending(), "not cleared yet");
            }
            apu.tick();
            assert!(!apu.irq_pending());
        }
    }

    #[test]
    fn test_inhibited_frame_flag_is_seen_but_raises_no_irq() {
        // with interrupts inhibited the flag is still set for two cycles at
        // the end of the sequence, but the IRQ line stays low
        let mut apu = APU::new(44100);
        write_frame_counter(&mut apu, 0x40);
        let mut seen = 0;
        for _ in 0..29840 {
            apu.tick();
            assert!(!apu.irq_pending());
            if apu.frame_counter.irq_flag {
                seen += 1;
            }
        }
        assert_eq!(seen, 2);
    }
}

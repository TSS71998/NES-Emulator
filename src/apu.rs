use crate::region::{FrameSequencerTiming, Region};

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 25% negated
];

const TRIANGLE_TABLE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

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
    enabled: bool,
    duty: u8,
    duty_pos: u8,
    timer_period: u16,
    timer: u16,
    length: u8,
    length_halt: bool,
    envelope: Envelope,
    sweep_enabled: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_divider: u8,
    sweep_reload: bool,
}

impl Pulse {
    fn new(is_pulse1: bool) -> Self {
        Pulse {
            is_pulse1,
            ..Default::default()
        }
    }

    fn write_control(&mut self, value: u8) {
        self.duty = value >> 6;
        self.length_halt = value & 0x20 != 0;
        self.envelope.write(value);
    }

    fn write_sweep(&mut self, value: u8) {
        self.sweep_enabled = value & 0x80 != 0;
        self.sweep_period = (value >> 4) & 0x07;
        self.sweep_negate = value & 0x08 != 0;
        self.sweep_shift = value & 0x07;
        self.sweep_reload = true;
    }

    fn write_timer_lo(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x0700) | value as u16;
    }

    fn write_timer_hi(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x00ff) | (((value & 0x07) as u16) << 8);
        if self.enabled {
            self.length = LENGTH_TABLE[(value >> 3) as usize];
        }
        self.duty_pos = 0;
        self.envelope.start = true;
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
    }

    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            self.duty_pos = (self.duty_pos + 1) & 7;
        } else {
            self.timer -= 1;
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

    fn is_muted(&self) -> bool {
        self.timer_period < 8 || self.sweep_target() > 0x7ff
    }

    fn clock_sweep(&mut self) {
        if self.sweep_divider == 0 && self.sweep_enabled && self.sweep_shift > 0 && !self.is_muted()
        {
            self.timer_period = self.sweep_target().max(0) as u16;
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }

    fn clock_length(&mut self) {
        if !self.length_halt && self.length > 0 {
            self.length -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.length == 0
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
    enabled: bool,
    control: bool,
    linear_reload_value: u8,
    linear_counter: u8,
    linear_reload: bool,
    timer_period: u16,
    timer: u16,
    seq_pos: u8,
    length: u8,
}

impl Triangle {
    fn write_linear(&mut self, value: u8) {
        self.control = value & 0x80 != 0;
        self.linear_reload_value = value & 0x7f;
    }

    fn write_timer_lo(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x0700) | value as u16;
    }

    fn write_timer_hi(&mut self, value: u8) {
        self.timer_period = (self.timer_period & 0x00ff) | (((value & 0x07) as u16) << 8);
        if self.enabled {
            self.length = LENGTH_TABLE[(value >> 3) as usize];
        }
        self.linear_reload = true;
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
    }

    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            if self.length > 0 && self.linear_counter > 0 && self.timer_period >= 2 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
        } else {
            self.timer -= 1;
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

    fn clock_length(&mut self) {
        if !self.control && self.length > 0 {
            self.length -= 1;
        }
    }

    fn output(&self) -> u8 {
        TRIANGLE_TABLE[self.seq_pos as usize]
    }
}

struct Noise {
    enabled: bool,
    length_halt: bool,
    envelope: Envelope,
    mode: bool,
    timer_period: u16,
    timer: u16,
    shift: u16,
    length: u8,
}

impl Noise {
    fn new() -> Self {
        Noise {
            enabled: false,
            length_halt: false,
            envelope: Envelope::default(),
            mode: false,
            timer_period: Region::Ntsc.noise_periods()[0],
            timer: 0,
            shift: 1,
            length: 0,
        }
    }

    fn write_control(&mut self, value: u8) {
        self.length_halt = value & 0x20 != 0;
        self.envelope.write(value);
    }

    fn write_period(&mut self, value: u8, periods: &[u16; 16]) {
        self.mode = value & 0x80 != 0;
        self.timer_period = periods[(value & 0x0F) as usize];
    }

    fn write_length(&mut self, value: u8) {
        if self.enabled {
            self.length = LENGTH_TABLE[(value >> 3) as usize];
        }
        self.envelope.start = true;
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
    }

    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.timer_period - 1;
            let tap = if self.mode { 6 } else { 1 };
            let feedback = (self.shift & 1) ^ ((self.shift >> tap) & 1);
            self.shift = (self.shift >> 1) | (feedback << 14);
        } else {
            self.timer -= 1;
        }
    }

    fn clock_length(&mut self) {
        if !self.length_halt && self.length > 0 {
            self.length -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.length == 0 || self.shift & 1 == 1 {
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
        }
    }

    fn write_control(&mut self, value: u8, rates: &[u16; 16]) {
        self.irq_enabled = value & 0x80 != 0;
        self.loop_flag = value & 0x40 != 0;
        self.rate = rates[(value & 0x0F) as usize];
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

    fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.bytes_remaining = 0;
        } else if self.bytes_remaining == 0 {
            self.restart();
        }
    }

    fn fetch_addr(&self) -> Option<u16> {
        if self.sample_buffer.is_none() && self.bytes_remaining > 0 {
            Some(self.current_addr)
        } else {
            None
        }
    }

    fn provide_byte(&mut self, byte: u8) {
        self.sample_buffer = Some(byte);
        self.current_addr = if self.current_addr == 0xffff {
            0x8000
        } else {
            self.current_addr + 1
        };
        self.bytes_remaining -= 1;
        if self.bytes_remaining == 0 && self.loop_flag {
            self.restart();
        }
    }

    fn clock_timer(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
            return;
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

pub struct APU {
    pulse1: Pulse,
    pulse2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,

    five_step_mode: bool,
    irq_inhibit: bool,
    frame_cycle: u32,
    odd_cycle: bool,

    pulse_table: [f32; 31],
    tnd_table: [f32; 203],

    region: Region,
    cpu_clock: f64,
    frame_timing: FrameSequencerTiming,

    sample_rate: f64,
    sample_phase: f64,
    sample_sum: f32,
    sample_count: u32,
    rate_adjust: f64,
    high_pass_90: HighPass,
    high_pass_440: HighPass,
    low_pass_14k: LowPass,
    samples: Vec<f32>,
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
            five_step_mode: false,
            irq_inhibit: false,
            frame_cycle: 0,
            odd_cycle: false,
            pulse_table: pulse_table,
            tnd_table: tnd_table,
            region: Region::Ntsc,
            cpu_clock: Region::Ntsc.cpu_clock_hz(),
            frame_timing: Region::Ntsc.frame_sequencer(),
            sample_rate: sample_rate as f64,
            sample_phase: 0.0,
            sample_sum: 0.0,
            sample_count: 0,
            rate_adjust: 1.0,
            high_pass_90: HighPass::new(90.0, sample_rate as f32),
            high_pass_440: HighPass::new(440.0, sample_rate as f32),
            low_pass_14k: LowPass::new(14000.0, sample_rate as f32),
            samples: Vec::with_capacity(sample_rate as usize / 30),
        }
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
        self.frame_timing = region.frame_sequencer();
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
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
                self.pulse1.set_enabled(value & 0x01 != 0);
                self.pulse2.set_enabled(value & 0x02 != 0);
                self.triangle.set_enabled(value & 0x04 != 0);
                self.noise.set_enabled(value & 0x08 != 0);
                self.dmc.set_enabled(value & 0x10 != 0);
            }
            0x4017 => {
                self.five_step_mode = value & 0x80 != 0;
                self.irq_inhibit = value & 0x40 != 0;
                self.frame_cycle = 0;
                // 5-step mode clocks everything immediately on write
                if self.five_step_mode {
                    self.quarter_frame();
                    self.half_frame();
                }
            }
            _ => {}
        }
    }

    pub fn read_status(&mut self) -> u8 {
        let mut status = 0;
        if self.pulse1.length > 0 {
            status |= 0x01;
        }
        if self.pulse2.length > 0 {
            status |= 0x02;
        }
        if self.triangle.length > 0 {
            status |= 0x04;
        }
        if self.noise.length > 0 {
            status |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            status |= 0x10;
        }
        status
    }

    pub fn irq_pending(&self) -> bool {
        false
    }

    pub fn dmc_fetch_addr(&self) -> Option<u16> {
        self.dmc.fetch_addr()
    }

    pub fn dmc_provide_byte(&mut self, byte: u8) {
        self.dmc.provide_byte(byte);
    }

    fn quarter_frame(&mut self) {
        self.pulse1.envelope.clock();
        self.pulse2.envelope.clock();
        self.noise.envelope.clock();
        self.triangle.clock_linear();
    }

    fn half_frame(&mut self) {
        self.pulse1.clock_length();
        self.pulse1.clock_sweep();
        self.pulse2.clock_length();
        self.pulse2.clock_sweep();
        self.triangle.clock_length();
        self.noise.clock_length();
    }

    fn clock_frame_sequencer(&mut self) {
        self.frame_cycle += 1;
        let t = self.frame_timing;
        let last_step = if self.five_step_mode { t.five_step_end } else { t.four_step_end };
        let cycle = self.frame_cycle;
        if cycle == t.steps[0] || cycle == t.steps[2] {
            self.quarter_frame();
        } else if cycle == t.steps[1] || cycle == last_step {
            self.quarter_frame();
            self.half_frame();
        } else if cycle == last_step + 1 {
            self.frame_cycle = 0;
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

        self.triangle.clock_timer();
        self.noise.clock_timer();
        self.dmc.clock_timer();
        if self.odd_cycle {
            self.pulse1.clock_timer();
            self.pulse2.clock_timer();
        }
        self.odd_cycle = !self.odd_cycle;

        self.sample_sum += self.mix();
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_status_reflects_length_counters() {
        let mut apu = APU::new(44100);
        apu.write_register(0x4015, 0x01); // enable pulse 1
        apu.write_register(0x4003, 0x08); // length index 1 -> 254
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
        apu.write_register(0x4003, 0x18); // length 2
        let mut ticks = vec![];
        let mut cycle = 0u32;
        while ticks.len() < 2 {
            let before = apu.pulse1.length;
            apu.tick();
            cycle += 1;
            if apu.pulse1.length != before {
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
}

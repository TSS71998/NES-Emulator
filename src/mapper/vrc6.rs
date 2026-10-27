use super::vrc_irq::VrcIrq;
use super::{Chr, Mapper, bank_offset};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_RAM_SIZE: usize = 0x2000;

/// The APU's scale for one step of VRC6 output: a VRC6 pulse at volume 15
/// is about as loud as an APU pulse at 15 (0.15).
const VRC6_STEP: f32 = 0.15 / 15.0;

/// Mappers 24 and 26: Konami VRC6 (Akumajou Densetsu; Madara and Esper
/// Dream 2 on 26, which swaps the A0 and A1 lines).
///
/// | Address     | Register                                             |
/// |-------------|------------------------------------------------------|
/// | $8000-$8003 | 16 KiB PRG bank at $8000                             |
/// | $9000-$B002 | sound (see [`Vrc6Audio`])                            |
/// | $B003       | PRG-RAM enable (bit 7), CHR A10 (bit 5), mirroring (bits 2-3), CHR mode (bits 0-1) |
/// | $C000-$C003 | 8 KiB PRG bank at $C000 ($E000 is the last bank)     |
/// | $D000-$E003 | CHR banks R0-R7                                      |
/// | $F000-$F002 | IRQ: latch, control, acknowledge (see [`VrcIrq`])    |
///
/// The games only ever use CHR mode 0 (eight 1 KiB banks) with the
/// console's nametables, mirrored by bits 2-3 of $B003 (vertical,
/// horizontal, single screen lower, upper); the other modes' pattern
/// table layouts are here too, but not CHR-ROM nametables.
pub struct Vrc6 {
    swap_lines: bool,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    prg_16k: u8,
    prg_8k: u8,
    control: u8,
    chr_banks: [u8; 8],
    irq: VrcIrq,
    audio: Vrc6Audio,
}

impl Vrc6 {
    pub fn new(rom: Rom) -> Self {
        Vrc6 {
            swap_lines: rom.mapper == 26,
            prg_ram: vec![0; PRG_RAM_SIZE],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,
            prg_16k: 0,
            prg_8k: 0,
            control: 0x20,
            chr_banks: [0; 8],
            irq: VrcIrq::default(),
            audio: Vrc6Audio::default(),
        }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let slot = addr / 0x400;
        let r = &self.chr_banks;
        let pass_a10 = self.control & 0x20 != 0;
        // a 2 KiB bank: R's two halves with A10 from the PPU, or R twice
        let two_k = |reg: u8| {
            if pass_a10 {
                (reg & 0xFE) | (slot & 1) as u8
            } else {
                reg
            }
        };
        let bank = match (self.control & 3, slot) {
            (0, _) => r[slot],
            (1, _) => two_k(r[slot / 2]),
            (_, 0..=3) => r[slot],
            (_, _) => two_k(r[4 + (slot - 4) / 2]),
        } as usize;
        bank_offset(bank, 0x400, self.chr.len()) + (addr & 0x3FF)
    }

    fn ram_enabled(&self) -> bool {
        self.control & 0x80 != 0
    }
}

impl Mapper for Vrc6 {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.u8(self.prg_16k);
        w.u8(self.prg_8k);
        w.u8(self.control);
        w.bytes(&self.chr_banks);
        self.irq.save_state(w);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        self.prg_16k = r.u8();
        self.prg_8k = r.u8();
        self.control = r.u8();
        r.bytes_into(&mut self.chr_banks);
        self.irq.load_state(r);
        self.audio.load_state(r);
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        let len = self.prg_rom.len();
        let byte = |bank: usize, size: usize| {
            Some(self.prg_rom[bank_offset(bank, size, len) + (addr as usize & (size - 1))])
        };
        match addr {
            0x6000..=0x7FFF if self.ram_enabled() => {
                Some(self.prg_ram[addr as usize & (PRG_RAM_SIZE - 1)])
            }
            0x8000..=0xBFFF => byte(self.prg_16k as usize, 0x4000),
            0xC000..=0xDFFF => byte(self.prg_8k as usize, 0x2000),
            0xE000..=0xFFFF => byte(len / 0x2000 - 1, 0x2000),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if (0x6000..0x8000).contains(&addr) {
            if self.ram_enabled() {
                self.prg_ram[addr as usize & (PRG_RAM_SIZE - 1)] = data;
            }
            return;
        }
        let mut register = addr & 3;
        if self.swap_lines {
            register = ((register & 1) << 1) | (register >> 1);
        }
        match (addr & 0xF000, register) {
            (0x8000, _) => self.prg_16k = data & 0x0F,
            (0x9000, 3) => self.audio.write_frequency_control(data),
            (0x9000, _) => self.audio.pulses[0].write(register, data),
            (0xA000, 3) => {}
            (0xA000, _) => self.audio.pulses[1].write(register, data),
            (0xB000, 3) => self.control = data,
            (0xB000, _) => self.audio.saw.write(register, data),
            (0xC000, _) => self.prg_8k = data & 0x1F,
            (0xD000, _) => self.chr_banks[register as usize] = data,
            (0xE000, _) => self.chr_banks[4 + register as usize] = data,
            (0xF000, 0) => self.irq.write_latch(data),
            (0xF000, 1) => self.irq.write_control(data),
            (0xF000, 2) => self.irq.acknowledge(),
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr.read(self.chr_offset(addr))
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        let offset = self.chr_offset(addr);
        self.chr.write(offset, data);
    }

    fn mirroring(&self) -> Mirroring {
        match (self.control >> 2) & 3 {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::SingleScreenLower,
            _ => Mirroring::SingleScreenUpper,
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq.pending()
    }

    fn clocked(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        self.irq.clock();
        self.audio.clock();
    }

    fn audio_output(&self) -> f32 {
        self.audio.output()
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        self.has_battery.then_some(&self.prg_ram[..])
    }

    fn load_battery_ram(&mut self, data: &[u8]) {
        let n = data.len().min(self.prg_ram.len());
        self.prg_ram[..n].copy_from_slice(&data[..n]);
    }
}

/// The VRC6's sound: two pulse channels with 8 duty cycles and a sawtooth,
/// summed by a linear 6-bit DAC.
#[derive(Default)]
pub struct Vrc6Audio {
    pulses: [Vrc6Pulse; 2],
    saw: Vrc6Saw,
    halt: bool,
    /// How far to shift the periods right: 0, 4 or 8 ($9003).
    period_shift: u8,
}

#[derive(Default)]
struct Vrc6Pulse {
    volume: u8,
    duty: u8,
    constant: bool,
    period: u16,
    enabled: bool,
    timer: u16,
    /// 15 down to 0; the output is on while step <= duty.
    step: u8,
}

#[derive(Default)]
struct Vrc6Saw {
    rate: u8,
    period: u16,
    enabled: bool,
    timer: u16,
    /// Counts timer clocks, 0-13: the accumulator adds on every second
    /// one and resets after the 14th.
    step: u8,
    accumulator: u8,
}

impl Vrc6Audio {
    fn write_frequency_control(&mut self, data: u8) {
        self.halt = data & 1 != 0;
        self.period_shift = if data & 4 != 0 {
            8
        } else if data & 2 != 0 {
            4
        } else {
            0
        };
    }

    fn clock(&mut self) {
        if self.halt {
            return;
        }
        let shift = self.period_shift;
        for pulse in &mut self.pulses {
            pulse.clock(shift);
        }
        self.saw.clock(shift);
    }

    fn output(&self) -> f32 {
        let sum = self.pulses[0].output() + self.pulses[1].output() + self.saw.output();
        sum as f32 * VRC6_STEP
    }

    fn save_state(&self, w: &mut StateWriter) {
        for pulse in &self.pulses {
            w.u8(pulse.volume);
            w.u8(pulse.duty);
            w.bool(pulse.constant);
            w.u16(pulse.period);
            w.bool(pulse.enabled);
            w.u16(pulse.timer);
            w.u8(pulse.step);
        }
        let saw = &self.saw;
        w.u8(saw.rate);
        w.u16(saw.period);
        w.bool(saw.enabled);
        w.u16(saw.timer);
        w.u8(saw.step);
        w.u8(saw.accumulator);
        w.bool(self.halt);
        w.u8(self.period_shift);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        for pulse in &mut self.pulses {
            pulse.volume = r.u8() & 0x0F;
            pulse.duty = r.u8() & 7;
            pulse.constant = r.bool();
            pulse.period = r.u16() & 0x0FFF;
            pulse.enabled = r.bool();
            pulse.timer = r.u16() & 0x0FFF;
            pulse.step = r.u8() & 0x0F;
        }
        let saw = &mut self.saw;
        saw.rate = r.u8() & 0x3F;
        saw.period = r.u16() & 0x0FFF;
        saw.enabled = r.bool();
        saw.timer = r.u16() & 0x0FFF;
        saw.step = r.u8() % 14;
        saw.accumulator = r.u8();
        self.halt = r.bool();
        self.period_shift = r.u8().min(8) & 0x0C;
    }
}

impl Vrc6Pulse {
    fn write(&mut self, register: u16, data: u8) {
        match register {
            0 => {
                self.constant = data & 0x80 != 0;
                self.duty = (data >> 4) & 7;
                self.volume = data & 0x0F;
            }
            1 => self.period = (self.period & 0x0F00) | data as u16,
            _ => {
                self.period = (self.period & 0x00FF) | ((data as u16 & 0x0F) << 8);
                self.enabled = data & 0x80 != 0;
                if !self.enabled {
                    self.step = 15;
                }
            }
        }
    }

    fn clock(&mut self, shift: u8) {
        if !self.enabled {
            return;
        }
        if self.timer == 0 {
            self.timer = self.period >> shift;
            self.step = self.step.wrapping_sub(1) & 0x0F;
        } else {
            self.timer -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.enabled && (self.constant || self.step <= self.duty) {
            self.volume
        } else {
            0
        }
    }
}

impl Vrc6Saw {
    fn write(&mut self, register: u16, data: u8) {
        match register {
            0 => self.rate = data & 0x3F,
            1 => self.period = (self.period & 0x0F00) | data as u16,
            _ => {
                self.period = (self.period & 0x00FF) | ((data as u16 & 0x0F) << 8);
                self.enabled = data & 0x80 != 0;
                if !self.enabled {
                    self.accumulator = 0;
                    self.step = 0;
                }
            }
        }
    }

    fn clock(&mut self, shift: u8) {
        if !self.enabled {
            return;
        }
        if self.timer == 0 {
            self.timer = self.period >> shift;
            self.step += 1;
            if self.step == 14 {
                self.step = 0;
                self.accumulator = 0;
            } else if self.step % 2 == 0 {
                self.accumulator = self.accumulator.wrapping_add(self.rate);
            }
        } else {
            self.timer -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.enabled { self.accumulator >> 3 } else { 0 }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    #[test]
    fn test_banks_and_mirroring() {
        let mut m = Vrc6::new(banked_rom(24, 16, 32, false));
        m.cpu_write(0x8000, 3);
        m.cpu_write(0xC000, 9); // 8 KiB bank 9 = 16 KiB bank 4
        m.cpu_write(0xD002, 77);
        m.cpu_write(0xE003, 100);
        assert_eq!(m.cpu_read(0x8000), Some(3));
        assert_eq!(m.cpu_read(0xC000), Some(4));
        assert_eq!(m.cpu_read(0xFFFF), Some(0x8F));
        assert_eq!(m.ppu_read(0x0800), 77);
        assert_eq!(m.ppu_read(0x1C00), 100);
        m.cpu_write(0xB003, 0xA4); // RAM on, horizontal
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        m.cpu_write(0x6000, 0x11);
        assert_eq!(m.cpu_read(0x6000), Some(0x11));
    }

    #[test]
    fn test_vrc6b_swaps_the_address_lines() {
        let mut m = Vrc6::new(banked_rom(26, 16, 32, false));
        m.cpu_write(0xD001, 50); // A0 is bit 1 on VRC6b: R2
        assert_eq!(m.ppu_read(0x0800), 50);
    }

    #[test]
    fn test_pulse_duty_and_period() {
        let mut audio = Vrc6Audio::default();
        audio.pulses[0].write(0, 0x3F); // duty 3 (4/16), volume 15
        audio.pulses[0].write(1, 9); // period 9: a step every 10 cycles
        audio.pulses[0].write(2, 0x80);
        let mut high = 0;
        for _ in 0..160 {
            audio.clock();
            if audio.output() > 0.0 {
                high += 1;
            }
        }
        assert_eq!(high, 40, "4 of 16 steps, 10 cycles each");
    }

    #[test]
    fn test_saw_ramps_and_resets() {
        let mut saw = Vrc6Saw::default();
        saw.write(0, 42);
        saw.write(2, 0x80); // period 0: a clock every cycle
        let mut outputs = vec![];
        for _ in 0..14 {
            saw.clock(0);
            outputs.push(saw.output());
        }
        assert_eq!(outputs, [0, 5, 5, 10, 10, 15, 15, 21, 21, 26, 26, 31, 31, 0]);
    }
}
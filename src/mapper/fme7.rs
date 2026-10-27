use super::{Chr, Mapper, bank_offset, prg_ram_size};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_BANK: usize = 0x2000;
const CHR_BANK: usize = 0x0400;

/// Mapper 69: Sunsoft FME-7 and its relatives 5A/5B (Batman: Return of
/// the Joker, Gimmick!, Hebereke, Barcode World).
///
/// $8000-$9FFF picks a command, and writing $A000-$BFFF runs it:
///
/// | Command | Parameter                                                  |
/// |---------|------------------------------------------------------------|
/// | $0-$7   | 1 KiB CHR bank at $0000, $0400, ... $1C00                  |
/// | $8      | $6000-$7FFF: bank (bits 0-5), RAM instead of ROM (bit 6), RAM enabled (bit 7) |
/// | $9-$B   | 8 KiB PRG bank at $8000, $A000, $C000 ($E000 is the last bank) |
/// | $C      | mirroring: vertical, horizontal, single screen lower/upper |
/// | $D      | IRQ: enable (bit 0), counter running (bit 7); acknowledges |
/// | $E, $F  | IRQ counter, low and high byte                             |
///
/// The IRQ counter counts down every CPU cycle while it runs, and raises
/// the IRQ when it wraps from $0000 to $FFFF. The 5B adds three square-wave
/// channels (see [`Sunsoft5b`]) at $C000 (register select) and $E000 (data).
pub struct Fme7 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    command: u8,
    chr_banks: [u8; 8],
    prg_banks: [u8; 3],
    /// Command 8's value.
    low_bank: u8,
    mirroring: Mirroring,
    irq_enabled: bool,
    counter_enabled: bool,
    counter: u16,
    irq_pending: bool,
    audio: Sunsoft5b,
}

impl Fme7 {
    pub fn new(rom: Rom) -> Self {
        Fme7 {
            prg_ram: vec![0; prg_ram_size(&rom)],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,
            command: 0,
            chr_banks: [0; 8],
            prg_banks: [0; 3],
            low_bank: 0,
            mirroring: Mirroring::Vertical,
            irq_enabled: false,
            counter_enabled: false,
            counter: 0,
            irq_pending: false,
            audio: Sunsoft5b::default(),
        }
    }

    fn prg_rom_byte(&self, bank: usize, addr: u16) -> u8 {
        let len = self.prg_rom.len();
        self.prg_rom[bank_offset(bank, PRG_BANK, len) + (addr as usize & (PRG_BANK - 1))]
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let bank = self.chr_banks[addr / CHR_BANK] as usize;
        bank_offset(bank, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
    }

    fn ram_selected(&self) -> bool {
        self.low_bank & 0x40 != 0
    }

    fn ram_enabled(&self) -> bool {
        self.low_bank & 0xC0 == 0xC0
    }

    fn ram_offset(&self, addr: u16) -> usize {
        let bank = (self.low_bank & 0x3F) as usize;
        bank_offset(bank, PRG_BANK, self.prg_ram.len()) + (addr as usize & (PRG_BANK - 1))
    }

    fn run_command(&mut self, data: u8) {
        match self.command {
            0..=7 => self.chr_banks[self.command as usize] = data,
            8 => self.low_bank = data,
            9..=0xB => self.prg_banks[self.command as usize - 9] = data & 0x3F,
            0xC => {
                self.mirroring = match data & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenLower,
                    _ => Mirroring::SingleScreenUpper,
                }
            }
            0xD => {
                self.irq_enabled = data & 0x01 != 0;
                self.counter_enabled = data & 0x80 != 0;
                self.irq_pending = false;
            }
            0xE => self.counter = (self.counter & 0xFF00) | data as u16,
            _ => self.counter = (self.counter & 0x00FF) | ((data as u16) << 8),
        }
    }
}

impl Mapper for Fme7 {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.u8(self.command);
        w.bytes(&self.chr_banks);
        w.bytes(&self.prg_banks);
        w.u8(self.low_bank);
        w.mirroring(self.mirroring);
        w.bool(self.irq_enabled);
        w.bool(self.counter_enabled);
        w.u16(self.counter);
        w.bool(self.irq_pending);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        self.command = r.u8() & 0x0F;
        r.bytes_into(&mut self.chr_banks);
        r.bytes_into(&mut self.prg_banks);
        self.low_bank = r.u8();
        self.mirroring = r.mirroring();
        self.irq_enabled = r.bool();
        self.counter_enabled = r.bool();
        self.counter = r.u16();
        self.irq_pending = r.bool();
        self.audio.load_state(r);
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF if self.ram_selected() => {
                self.ram_enabled().then(|| self.prg_ram[self.ram_offset(addr)])
            }
            0x6000..=0x7FFF => Some(self.prg_rom_byte((self.low_bank & 0x3F) as usize, addr)),
            0x8000..=0xDFFF => {
                let slot = (addr as usize - 0x8000) / PRG_BANK;
                Some(self.prg_rom_byte(self.prg_banks[slot] as usize, addr))
            }
            0xE000..=0xFFFF => {
                let last = self.prg_rom.len() / PRG_BANK - 1;
                Some(self.prg_rom_byte(last, addr))
            }
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.ram_enabled() {
                    let offset = self.ram_offset(addr);
                    self.prg_ram[offset] = data;
                }
            }
            0x8000..=0x9FFF => self.command = data & 0x0F,
            0xA000..=0xBFFF => self.run_command(data),
            0xC000..=0xDFFF => self.audio.select(data),
            0xE000..=0xFFFF => self.audio.write(data),
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
        self.mirroring
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn clocked(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        if self.counter_enabled {
            self.counter = self.counter.wrapping_sub(1);
            if self.counter == 0xFFFF && self.irq_enabled {
                self.irq_pending = true;
            }
        }
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

/// The Sunsoft 5B's sound: a Yamaha YM2149F (an AY-3-8910 relative) with
/// three square-wave channels, a noise generator and an envelope, mixed
/// together on a logarithmic volume scale. Only Gimmick! uses it.
///
/// Registers (selected through $C000, written through $E000):
/// $00-$05 tone periods (12 bits each), $06 noise period, $07 mixer (bits
/// 0-2 turn tone off, bits 3-5 noise off, for channels A-C), $08-$0A
/// volumes (bit 4: use the envelope), $0B-$0C envelope period, $0D
/// envelope shape.
#[derive(Default)]
pub struct Sunsoft5b {
    select: u8,
    /// A select with any of bits 4-7 set disables writes.
    write_disabled: bool,
    registers: [u8; 16],
    /// Counts CPU cycles: the tone and envelope steps come every 16, the
    /// noise every 32.
    divider: u8,
    tone_counters: [u16; 3],
    tone_high: [bool; 3],
    noise_counter: u8,
    /// The 17-bit noise shift register (bit 0 is the output).
    noise: u32,
    envelope_counter: u16,
    envelope_step: u8,
    envelope_attack: bool,
    envelope_holding: bool,
}

/// The DAC's output for each of the 32 levels: about 1.5 dB apart, 0 and
/// 1 silent. Level 31 (volume 15) is about 0.2 on the APU's scale.
fn level_amplitude(level: u8) -> f32 {
    if level < 2 {
        0.0
    } else {
        0.2 * 10f32.powf((level as f32 - 31.0) * 1.5 / 20.0)
    }
}

impl Sunsoft5b {
    fn select(&mut self, data: u8) {
        self.select = data & 0x0F;
        self.write_disabled = data & 0xF0 != 0;
    }

    fn write(&mut self, data: u8) {
        if self.write_disabled {
            return;
        }
        let register = self.select as usize;
        self.registers[register] = data;
        if register == 0x0D {
            self.envelope_step = 0;
            self.envelope_attack = data & 0x04 != 0;
            self.envelope_holding = false;
            self.envelope_counter = 0;
        }
    }

    fn tone_period(&self, channel: usize) -> u16 {
        let low = self.registers[channel * 2] as u16;
        let high = (self.registers[channel * 2 + 1] & 0x0F) as u16;
        ((high << 8) | low).max(1)
    }

    fn clock(&mut self) {
        self.divider = self.divider.wrapping_add(1);
        if self.divider % 16 != 0 {
            return;
        }
        for channel in 0..3 {
            self.tone_counters[channel] += 1;
            if self.tone_counters[channel] >= self.tone_period(channel) {
                self.tone_counters[channel] = 0;
                self.tone_high[channel] = !self.tone_high[channel];
            }
        }
        if self.divider % 32 == 0 {
            self.noise_counter += 1;
            if self.noise_counter >= (self.registers[6] & 0x1F).max(1) {
                self.noise_counter = 0;
                if self.noise == 0 {
                    self.noise = 1;
                }
                let feedback = (self.noise ^ (self.noise >> 3)) & 1;
                self.noise = (self.noise >> 1) | (feedback << 16);
            }
        }
        let envelope_period =
            (((self.registers[0x0C] as u16) << 8) | self.registers[0x0B] as u16).max(1);
        self.envelope_counter += 1;
        if self.envelope_counter >= envelope_period {
            self.envelope_counter = 0;
            self.step_envelope();
        }
    }

    fn step_envelope(&mut self) {
        if self.envelope_holding {
            return;
        }
        if self.envelope_step < 31 {
            self.envelope_step += 1;
            return;
        }
        let shape = self.registers[0x0D];
        let (cont, alt, hold) = (shape & 8 != 0, shape & 2 != 0, shape & 1 != 0);
        if !cont {
            // one ramp, then silence
            self.envelope_attack = false;
            self.envelope_holding = true;
        } else if hold {
            if alt {
                self.envelope_attack = !self.envelope_attack;
            }
            self.envelope_holding = true;
        } else {
            if alt {
                self.envelope_attack = !self.envelope_attack;
            }
            self.envelope_step = 0;
        }
    }

    fn envelope_level(&self) -> u8 {
        if self.envelope_attack {
            self.envelope_step
        } else {
            31 - self.envelope_step
        }
    }

    fn output(&self) -> f32 {
        let mixer = self.registers[7];
        let noise_high = self.noise & 1 != 0;
        let mut sum = 0.0;
        for channel in 0..3 {
            let tone = self.tone_high[channel] || mixer & (1 << channel) != 0;
            let noise = noise_high || mixer & (8 << channel) != 0;
            if tone && noise {
                let volume = self.registers[8 + channel];
                let level = if volume & 0x10 != 0 {
                    self.envelope_level()
                } else if volume & 0x0F == 0 {
                    0
                } else {
                    (volume & 0x0F) * 2 + 1
                };
                sum += level_amplitude(level);
            }
        }
        sum
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.select);
        w.bool(self.write_disabled);
        w.bytes(&self.registers);
        w.u8(self.divider);
        for channel in 0..3 {
            w.u16(self.tone_counters[channel]);
            w.bool(self.tone_high[channel]);
        }
        w.u8(self.noise_counter);
        w.u32(self.noise);
        w.u16(self.envelope_counter);
        w.u8(self.envelope_step);
        w.bool(self.envelope_attack);
        w.bool(self.envelope_holding);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.select = r.u8() & 0x0F;
        self.write_disabled = r.bool();
        r.bytes_into(&mut self.registers);
        self.divider = r.u8();
        for channel in 0..3 {
            self.tone_counters[channel] = r.u16() & 0x0FFF;
            self.tone_high[channel] = r.bool();
        }
        self.noise_counter = r.u8() & 0x1F;
        self.noise = r.u32() & 0x1FFFF;
        self.envelope_counter = r.u16();
        self.envelope_step = r.u8().min(31);
        self.envelope_attack = r.bool();
        self.envelope_holding = r.bool();
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    fn command(m: &mut Fme7, command: u8, data: u8) {
        m.cpu_write(0x8000, command);
        m.cpu_write(0xA000, data);
    }

    #[test]
    fn test_banks() {
        let mut m = Fme7::new(banked_rom(69, 16, 16, true));
        command(&mut m, 9, 5); // 8 KiB bank 5 = 16 KiB bank 2
        command(&mut m, 0xB, 8);
        command(&mut m, 3, 40);
        assert_eq!(m.cpu_read(0x8000), Some(2));
        assert_eq!(m.cpu_read(0xC000), Some(4));
        assert_eq!(m.cpu_read(0xFFFF), Some(0x8F));
        assert_eq!(m.ppu_read(0x0C00), 40);

        command(&mut m, 8, 3); // ROM bank 3 at $6000
        assert_eq!(m.cpu_read(0x6000), Some(1));
        command(&mut m, 8, 0x40); // RAM, but not enabled: open bus
        assert_eq!(m.cpu_read(0x6000), None);
        command(&mut m, 8, 0xC0);
        m.cpu_write(0x6001, 0x5A);
        assert_eq!(m.cpu_read(0x6001), Some(0x5A));
        assert_eq!(m.battery_ram().unwrap()[1], 0x5A);
    }

    #[test]
    fn test_irq_on_wrap() {
        let mut m = Fme7::new(banked_rom(69, 16, 16, false));
        command(&mut m, 0xE, 2);
        command(&mut m, 0xF, 0);
        command(&mut m, 0xD, 0x81);
        m.cpu_clock();
        m.cpu_clock();
        assert!(!m.irq_pending(), "0 isn't the end: the wrap to $FFFF is");
        m.cpu_clock();
        assert!(m.irq_pending());
        command(&mut m, 0xD, 0x81);
        assert!(!m.irq_pending(), "writing command $D acknowledges");
    }

    #[test]
    fn test_5b_tone_frequency() {
        let mut m = Fme7::new(banked_rom(69, 16, 16, false));
        for (register, value) in [
            (0, 4),        // channel A period 4: toggles every 64 cycles
            (7, 0b111110), // only channel A's tone
            (8, 15),
        ] {
            m.cpu_write(0xC000, register);
            m.cpu_write(0xE000, value);
        }
        let mut changes = 0;
        let mut last = m.audio_output();
        for _ in 0..64 * 10 {
            m.cpu_clock();
            let out = m.audio_output();
            if out != last {
                changes += 1;
                last = out;
            }
        }
        assert_eq!(changes, 10);
        assert!((0.15..0.25).contains(&level_amplitude(31)));
    }

    #[test]
    fn test_5b_writes_need_a_clean_select() {
        let mut audio = Sunsoft5b::default();
        audio.select(0x18);
        audio.write(0x0F);
        assert_eq!(audio.registers[8], 0);
        audio.select(0x08);
        audio.write(0x0F);
        assert_eq!(audio.registers[8], 0x0F);
    }
}
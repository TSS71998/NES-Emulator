use super::vrc_irq::VrcIrq;
use super::{Chr, Mapper, bank_offset, prg_ram_size};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_BANK: usize = 0x2000;
const CHR_BANK: usize = 0x0400;

/// Mappers 21, 22, 23 and 25: Konami VRC2 and VRC4 (Contra and Gradius II
/// in Japan, Ganbare Goemon, Tiny Toon Adventures, Parodius Da!).
///
/// | Address     | Register                                              |
/// |-------------|-------------------------------------------------------|
/// | $8000       | 8 KiB PRG bank at $8000 (at $C000 in VRC4 swap mode)  |
/// | $9000/$9001 | mirroring: vertical, horizontal (VRC4: single screens too) |
/// | $9002/$9003 | VRC4: PRG swap mode (bit 1)                           |
/// | $A000       | 8 KiB PRG bank at $A000                               |
/// | $B000-$E003 | 1 KiB CHR banks 0-7, each as a low and a high nibble  |
/// | $F000-$F003 | VRC4 IRQ: latch low and high nibble, control, acknowledge |
///
/// The second-last 8 KiB bank is fixed at $C000 (or $8000), the last at
/// $E000. The register within each $x000 block comes from two CPU address
/// lines, wired differently on each board: mapper 21 uses A1/A2 or A6/A7,
/// 22 A1/A0, 23 A0/A1 or A2/A3, 25 A1/A0 or A3/A2. Without an NES 2.0
/// submapper both wirings are decoded at once, which works for every game.
/// VRC2a (mapper 22) drops the low bit of the CHR bank numbers.
pub struct Vrc4 {
    vrc2: bool,
    /// The address bits (as masks) that make register bits 0 and 1.
    reg_bits: [u16; 2],
    chr_shift: u8,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    prg_banks: [u8; 2],
    swap_mode: bool,
    chr_banks: [u16; 8],
    mirroring: Mirroring,
    irq: VrcIrq,
}

impl Vrc4 {
    pub fn new(rom: Rom) -> Self {
        const A0: u16 = 1;
        const A1: u16 = 2;
        const A2: u16 = 4;
        const A3: u16 = 8;
        const A6: u16 = 0x40;
        const A7: u16 = 0x80;
        let (vrc2, reg_bits) = match (rom.mapper, rom.submapper) {
            (21, 1) => (false, [A1, A2]),
            (21, 2) => (false, [A6, A7]),
            (21, _) => (false, [A1 | A6, A2 | A7]),
            (22, _) => (true, [A1, A0]),
            (23, 1) => (false, [A0, A1]),
            (23, 2) => (false, [A2, A3]),
            (23, 3) => (true, [A0, A1]),
            (23, _) => (false, [A0 | A2, A1 | A3]),
            (25, 1) => (false, [A1, A0]),
            (25, 2) => (false, [A3, A2]),
            (25, 3) => (true, [A1, A0]),
            _ => (false, [A1 | A3, A0 | A2]),
        };
        Vrc4 {
            vrc2,
            reg_bits,
            chr_shift: if rom.mapper == 22 { 1 } else { 0 },
            prg_ram: vec![0; prg_ram_size(&rom)],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,
            prg_banks: [0, 1],
            swap_mode: false,
            chr_banks: [0; 8],
            mirroring: Mirroring::Vertical,
            irq: VrcIrq::default(),
        }
    }

    /// Which of the four registers in a $x000 block an address is.
    fn register(&self, addr: u16) -> u8 {
        let bit = |mask: u16| (addr & mask != 0) as u8;
        bit(self.reg_bits[0]) | bit(self.reg_bits[1]) << 1
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let banks = self.prg_rom.len() / PRG_BANK;
        let second_last = banks.saturating_sub(2);
        let bank = match ((addr - 0x8000) as usize / PRG_BANK, self.swap_mode) {
            (0, false) | (2, true) => self.prg_banks[0] as usize,
            (0, true) | (2, false) => second_last,
            (1, _) => self.prg_banks[1] as usize,
            _ => banks - 1,
        };
        bank_offset(bank, PRG_BANK, self.prg_rom.len()) + (addr as usize & (PRG_BANK - 1))
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let bank = (self.chr_banks[addr / CHR_BANK] >> self.chr_shift) as usize;
        bank_offset(bank, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
    }
}

impl Mapper for Vrc4 {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.bytes(&self.prg_banks);
        w.bool(self.swap_mode);
        for bank in self.chr_banks {
            w.u16(bank);
        }
        w.mirroring(self.mirroring);
        self.irq.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        r.bytes_into(&mut self.prg_banks);
        self.swap_mode = r.bool();
        for bank in &mut self.chr_banks {
            *bank = r.u16();
        }
        self.mirroring = r.mirroring();
        self.irq.load_state(r);
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF => Some(self.prg_ram[(addr as usize - 0x6000) % self.prg_ram.len()]),
            0x8000..=0xFFFF => Some(self.prg_rom[self.prg_offset(addr)]),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if addr < 0x8000 {
            if addr >= 0x6000 {
                let len = self.prg_ram.len();
                self.prg_ram[(addr as usize - 0x6000) % len] = data;
            }
            return;
        }
        let register = self.register(addr);
        match (addr & 0xF000, register) {
            (0x8000, _) => self.prg_banks[0] = data & 0x1F,
            (0x9000, _) if self.vrc2 => {
                self.mirroring = if data & 1 == 0 {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                };
            }
            (0x9000, 0 | 1) => {
                self.mirroring = match data & 3 {
                    0 => Mirroring::Vertical,
                    1 => Mirroring::Horizontal,
                    2 => Mirroring::SingleScreenLower,
                    _ => Mirroring::SingleScreenUpper,
                };
            }
            (0x9000, _) => self.swap_mode = data & 2 != 0,
            (0xA000, _) => self.prg_banks[1] = data & 0x1F,
            (0xB000..=0xE000, _) => {
                let bank = ((addr >> 12) - 0xB) as usize * 2 + (register >> 1) as usize;
                let value = data as u16 & 0x0F;
                let old = self.chr_banks[bank];
                self.chr_banks[bank] = if register & 1 == 0 {
                    (old & 0x1F0) | value
                } else {
                    let high = if self.vrc2 { value & 0x0F } else { data as u16 & 0x1F };
                    (old & 0x0F) | (high << 4)
                };
            }
            (0xF000, _) if self.vrc2 => {}
            (0xF000, 0) => self.irq.write_latch_low(data),
            (0xF000, 1) => self.irq.write_latch_high(data),
            (0xF000, 2) => self.irq.write_control(data),
            (0xF000, _) => self.irq.acknowledge(),
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
        self.irq.pending()
    }

    fn clocked(&self) -> bool {
        !self.vrc2
    }

    fn cpu_clock(&mut self) {
        self.irq.clock();
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        self.has_battery.then_some(&self.prg_ram[..])
    }

    fn load_battery_ram(&mut self, data: &[u8]) {
        let n = data.len().min(self.prg_ram.len());
        self.prg_ram[..n].copy_from_slice(&data[..n]);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    #[test]
    fn test_vrc4_banks_through_either_wiring() {
        for (mapper, reg1, reg2) in [(21, 0x02, 0x04), (21, 0x40, 0x80), (23, 0x01, 0x02), (25, 0x08, 0x04)] {
            let mut m = Vrc4::new(banked_rom(mapper, 16, 32, false));
            m.cpu_write(0x8000, 6); // 8 KiB bank 6 = 16 KiB bank 3
            m.cpu_write(0xA000, 9);
            m.cpu_write(0xB000 | reg2, 0x05); // bank 1, low nibble
            m.cpu_write(0xB000 | reg2 | reg1, 0x01); // bank 1, high nibble: $15
            assert_eq!(m.cpu_read(0x8000), Some(3), "mapper {mapper}");
            assert_eq!(m.cpu_read(0xA000), Some(4));
            assert_eq!(m.cpu_read(0xC000), Some(15));
            assert_eq!(m.ppu_read(0x0400), 0x15, "mapper {mapper}");
            m.cpu_write(0x9000 | reg2, 0x02); // swap mode
            assert_eq!(m.cpu_read(0x8000), Some(15));
            assert_eq!(m.cpu_read(0xC000), Some(3));
            m.cpu_write(0x9000, 0x03);
            assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
        }
    }

    #[test]
    fn test_vrc2a_halves_chr_banks() {
        let mut m = Vrc4::new(banked_rom(22, 16, 16, false));
        m.cpu_write(0xC000, 0x06); // VRC2a: A1 is bit 0, so $C000 is bank 2's low nibble
        assert_eq!(m.ppu_read(0x0800), 3);
        m.cpu_write(0x9000, 0x03);
        assert_eq!(m.mirroring(), Mirroring::Horizontal, "VRC2 only has bit 0");
        assert!(!m.clocked());
    }

    #[test]
    fn test_vrc4_irq() {
        let mut m = Vrc4::new(banked_rom(25, 16, 16, false));
        m.cpu_write(0xF000, 0x0E); // latch low nibble (25: A1/A3 = bit 0)
        m.cpu_write(0xF002, 0x0F); // latch high nibble: $FE
        m.cpu_write(0xF001, 0x06); // control: enabled, cycle mode
        m.cpu_clock();
        assert!(!m.irq_pending());
        m.cpu_clock();
        assert!(m.irq_pending());
        m.cpu_write(0xF003, 0); // acknowledge
        assert!(!m.irq_pending());
    }
}
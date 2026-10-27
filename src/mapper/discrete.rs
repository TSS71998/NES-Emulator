use super::{Chr, Mapper, bank_offset};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_BANK: usize = 0x4000;
const CHR_BANK: usize = 0x1000;
const PRG_RAM_SIZE: usize = 0x2000;

/// The boards that are a latch or two and some logic chips ("discrete
/// logic"): a write to a register selects the PRG and CHR banks, and
/// sometimes the mirroring. None of them have IRQs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Board {
    /// 11: Color Dreams (Crystal Mines, Spiritual Warfare). $8000-$FFFF:
    /// 32 KiB PRG (bits 0-1), 8 KiB CHR (bits 4-7).
    ColorDreams,
    /// 13: CPROM (Videomation). 16 KiB of CHR-RAM: $0000 fixed to the
    /// first 4 KiB, $1000 switched by writes to $8000-$FFFF (bits 0-1).
    Cprom,
    /// 34: BNROM (Deadly Towers). $8000-$FFFF: 32 KiB PRG. CHR-RAM.
    Bnrom,
    /// 34: NINA-001 (Impossible Mission II). $7FFD: 32 KiB PRG; $7FFE and
    /// $7FFF: 4 KiB CHR at $0000 and $1000. 8 KiB of PRG-RAM.
    Nina001,
    /// 66: GxROM (Super Mario Bros. + Duck Hunt, Dragon Power).
    /// $8000-$FFFF: 32 KiB PRG (bits 4-5), 8 KiB CHR (bits 0-1).
    Gxrom,
    /// 70 and 152: Bandai's simple boards (Family Trainer, Arkanoid II).
    /// $8000-$FFFF: 16 KiB PRG at $8000 (bits 4-6), 8 KiB CHR (bits 0-3);
    /// on 152, bit 7 picks the single screen.
    Bandai { single_screen: bool },
    /// 71: Camerica BF909x (Micro Machines, Fire Hawk). $C000-$FFFF: 16 KiB
    /// PRG at $8000, the last bank fixed at $C000. $9000-$9FFF (Fire Hawk's
    /// board) picks the single screen with bit 4.
    Camerica,
    /// 78: Irem's Holy Diver board and Jaleco's JF-16 (Cosmo Carrier).
    /// $8000-$FFFF: 16 KiB PRG at $8000 (bits 0-2), 8 KiB CHR (bits 4-7),
    /// and bit 3 picks vertical/horizontal mirroring (Holy Diver) or the
    /// single screen (JF-16).
    Irem78 { holy_diver: bool },
    /// 79: NINA-03/NINA-06 (Krazy Kreatures, Tiles of Fate). Writes where
    /// the address matches 010x xxx1 xxxx xxxx ($4100, $4300, ...): 32 KiB
    /// PRG (bit 3), 8 KiB CHR (bits 0-2).
    Nina03,
    /// 87: Jaleco/Konami JF-xx (City Connection, Goonies). $6000-$7FFF:
    /// 8 KiB CHR, with the two bits swapped.
    Jaleco87,
    /// 94: UN1ROM (Senjou no Ookami). $8000-$FFFF: 16 KiB PRG at $8000
    /// (bits 2-4), last bank fixed.
    Un1rom,
    /// 140: Jaleco JF-11/JF-14 (Bio Senshi Dan). $6000-$7FFF: 32 KiB PRG
    /// (bits 4-5), 8 KiB CHR (bits 0-3).
    Jaleco140,
    /// 180: UNROM wired the other way (Crazy Climber): the first bank fixed
    /// at $8000, $C000 switched by writes to $8000-$FFFF.
    CrazyClimber,
    /// 232: Camerica BF9096 (Quattro Adventure). $8000-$BFFF picks a 64 KiB
    /// block (bits 3-4), $C000-$FFFF a 16 KiB bank in it for $8000; $C000
    /// shows the block's last bank.
    Quattro,
}

impl Board {
    /// The board for an iNES mapper number, if it's one of these.
    pub fn for_rom(rom: &Rom) -> Option<Board> {
        Some(match rom.mapper {
            11 => Board::ColorDreams,
            13 => Board::Cprom,
            34 => {
                // NES 2.0 says which; otherwise CHR-ROM bigger than 8 KiB
                // means NINA-001 (BNROM has CHR-RAM)
                match rom.submapper {
                    1 => Board::Nina001,
                    2 => Board::Bnrom,
                    _ if rom.chr_rom.len() > 0x2000 => Board::Nina001,
                    _ => Board::Bnrom,
                }
            }
            66 => Board::Gxrom,
            70 => Board::Bandai { single_screen: false },
            71 => Board::Camerica,
            78 => Board::Irem78 {
                // NES 2.0 submapper 3; old iNES files of Holy Diver set
                // the four-screen bit to tell the boards apart
                holy_diver: rom.submapper == 3
                    || (rom.submapper == 0 && rom.screen_mirroring == Mirroring::FourScreen),
            },
            79 => Board::Nina03,
            87 => Board::Jaleco87,
            94 => Board::Un1rom,
            140 => Board::Jaleco140,
            152 => Board::Bandai { single_screen: true },
            180 => Board::CrazyClimber,
            232 => Board::Quattro,
            _ => return None,
        })
    }
}

/// The discrete-logic boards (see [`Board`]). Banks are kept as two 16 KiB
/// PRG banks ($8000, $C000) and two 4 KiB CHR banks ($0000, $1000), which
/// covers all of them.
pub struct Discrete {
    board: Board,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    header_mirroring: Mirroring,
    mirroring: Mirroring,
    prg: [usize; 2],
    chr_banks: [usize; 2],
    /// Quattro's 64 KiB block.
    outer: usize,
}

impl Discrete {
    pub fn new(rom: Rom, board: Board) -> Self {
        let chr = if board == Board::Cprom {
            Chr::ram(0x4000)
        } else {
            Chr::new(&rom)
        };
        let last = (rom.prg_rom.len() / PRG_BANK).saturating_sub(1);
        let prg = match board {
            Board::Camerica | Board::Bandai { .. } | Board::Irem78 { .. } | Board::Un1rom => {
                [0, last]
            }
            Board::Quattro => [0, 3],
            _ => [0, 1],
        };
        let mirroring = match board {
            Board::Irem78 { .. } | Board::Bandai { single_screen: true } => {
                Mirroring::SingleScreenLower
            }
            _ => rom.screen_mirroring,
        };
        Discrete {
            board,
            prg_ram: vec![0; if board == Board::Nina001 { PRG_RAM_SIZE } else { 0 }],
            chr,
            header_mirroring: rom.screen_mirroring,
            mirroring,
            prg_rom: rom.prg_rom,
            prg,
            chr_banks: [0, 1],
            outer: 0,
        }
    }

    fn set_prg_32k(&mut self, bank: usize) {
        self.prg = [bank * 2, bank * 2 + 1];
    }

    fn set_chr_8k(&mut self, bank: usize) {
        self.chr_banks = [bank * 2, bank * 2 + 1];
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let bank = self.chr_banks[addr / CHR_BANK];
        bank_offset(bank, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
    }

    fn single_screen(upper: bool) -> Mirroring {
        if upper {
            Mirroring::SingleScreenUpper
        } else {
            Mirroring::SingleScreenLower
        }
    }
}

impl Mapper for Discrete {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.mirroring(self.mirroring);
        w.usize(self.prg[0]);
        w.usize(self.prg[1]);
        w.usize(self.chr_banks[0]);
        w.usize(self.chr_banks[1]);
        w.usize(self.outer);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        self.mirroring = r.mirroring();
        self.prg = [r.usize(), r.usize()];
        self.chr_banks = [r.usize(), r.usize()];
        self.outer = r.usize();
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF if !self.prg_ram.is_empty() => {
                Some(self.prg_ram[addr as usize & (PRG_RAM_SIZE - 1)])
            }
            0x8000..=0xFFFF => {
                let bank = self.prg[(addr as usize - 0x8000) / PRG_BANK];
                let offset = bank_offset(bank, PRG_BANK, self.prg_rom.len());
                Some(self.prg_rom[offset + (addr as usize & (PRG_BANK - 1))])
            }
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        let d = data as usize;
        let rom = addr >= 0x8000;
        match self.board {
            Board::ColorDreams if rom => {
                self.set_prg_32k(d & 3);
                self.set_chr_8k(d >> 4);
            }
            Board::Cprom if rom => self.chr_banks[1] = d & 3,
            Board::Bnrom if rom => self.set_prg_32k(d),
            Board::Nina001 => {
                if (0x6000..0x8000).contains(&addr) {
                    self.prg_ram[addr as usize & (PRG_RAM_SIZE - 1)] = data;
                }
                match addr {
                    0x7FFD => self.set_prg_32k(d & 1),
                    0x7FFE => self.chr_banks[0] = d & 0x0F,
                    0x7FFF => self.chr_banks[1] = d & 0x0F,
                    _ => {}
                }
            }
            Board::Gxrom if rom => {
                self.set_prg_32k((d >> 4) & 3);
                self.set_chr_8k(d & 3);
            }
            Board::Bandai { single_screen } if rom => {
                self.prg[0] = (d >> 4) & 7;
                self.set_chr_8k(d & 0x0F);
                if single_screen {
                    self.mirroring = Self::single_screen(d & 0x80 != 0);
                }
            }
            Board::Camerica => match addr {
                0x9000..=0x9FFF => self.mirroring = Self::single_screen(d & 0x10 != 0),
                0xC000..=0xFFFF => self.prg[0] = d,
                _ => {}
            },
            Board::Irem78 { holy_diver } if rom => {
                self.prg[0] = d & 7;
                self.set_chr_8k(d >> 4);
                let bit = d & 8 != 0;
                self.mirroring = match (holy_diver, bit) {
                    (true, false) => Mirroring::Horizontal,
                    (true, true) => Mirroring::Vertical,
                    (false, upper) => Self::single_screen(upper),
                };
            }
            Board::Nina03 if addr & 0xE100 == 0x4100 => {
                self.set_prg_32k((d >> 3) & 1);
                self.set_chr_8k(d & 7);
            }
            Board::Jaleco87 if (0x6000..0x8000).contains(&addr) => {
                self.set_chr_8k(((d & 1) << 1) | ((d >> 1) & 1));
            }
            Board::Un1rom if rom => self.prg[0] = (d >> 2) & 7,
            Board::Jaleco140 if (0x6000..0x8000).contains(&addr) => {
                self.set_prg_32k((d >> 4) & 3);
                self.set_chr_8k(d & 0x0F);
            }
            Board::CrazyClimber if rom => self.prg[1] = d & 7,
            Board::Quattro => {
                match addr {
                    0x8000..=0xBFFF => self.outer = (d >> 3) & 3,
                    0xC000..=0xFFFF => self.prg[0] = (self.outer << 2) | (d & 3),
                    _ => return,
                }
                self.prg = [(self.outer << 2) | (self.prg[0] & 3), (self.outer << 2) | 3];
            }
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
        if self.header_mirroring == Mirroring::FourScreen && !matches!(self.board, Board::Irem78 { .. }) {
            return Mirroring::FourScreen;
        }
        self.mirroring
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    fn board(mapper: u16, prg_16k: usize, chr_8k: usize) -> Discrete {
        let rom = banked_rom(mapper, prg_16k, chr_8k, false);
        let board = Board::for_rom(&rom).unwrap();
        Discrete::new(rom, board)
    }

    /// The 16 KiB PRG banks at $8000 and $C000, and the 1 KiB CHR banks at
    /// $0000 and $1000, by their tags.
    fn banks(m: &Discrete) -> (u8, u8, u8, u8) {
        (
            m.cpu_read(0x8000).unwrap(),
            m.cpu_read(0xC000).unwrap(),
            m.ppu_read(0x0000),
            m.ppu_read(0x1000),
        )
    }

    #[test]
    fn test_color_dreams_and_gxrom() {
        let mut m = board(11, 8, 16);
        m.cpu_write(0x8000, 0x52); // PRG 32K bank 2, CHR 8K bank 5
        assert_eq!(banks(&m), (4, 5, 40, 44));
        let mut m = board(66, 8, 4);
        m.cpu_write(0xFFFF, 0x13); // PRG bank 1, CHR bank 3
        assert_eq!(banks(&m), (2, 3, 24, 28));
    }

    #[test]
    fn test_bnrom_and_nina001() {
        let mut m = board(34, 8, 0);
        assert_eq!(m.board, Board::Bnrom);
        m.cpu_write(0x8000, 3);
        assert_eq!(banks(&m).0, 6);

        let mut m = board(34, 4, 4);
        assert_eq!(m.board, Board::Nina001);
        m.cpu_write(0x7FFD, 1);
        m.cpu_write(0x7FFE, 3);
        m.cpu_write(0x7FFF, 6);
        assert_eq!(banks(&m), (2, 3, 12, 24));
        assert_eq!(m.cpu_read(0x7FFF), Some(6), "the registers are RAM too");
    }

    #[test]
    fn test_fixed_last_bank_boards() {
        let mut m = board(71, 8, 0);
        m.cpu_write(0xC000, 5);
        m.cpu_write(0x9000, 0x10);
        assert_eq!((banks(&m).0, banks(&m).1), (5, 7));
        assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);

        let mut m = board(180, 8, 0);
        m.cpu_write(0x8000, 6);
        assert_eq!((banks(&m).0, banks(&m).1), (0, 6));

        let mut m = board(94, 8, 0);
        m.cpu_write(0x8000, 3 << 2);
        assert_eq!((banks(&m).0, banks(&m).1), (3, 7));
    }

    #[test]
    fn test_holy_diver_mirroring() {
        let mut rom = banked_rom(78, 8, 8, false);
        rom.submapper = 3;
        let mut m = Discrete::new(rom, Board::Irem78 { holy_diver: true });
        m.cpu_write(0x8000, 0x3A); // PRG 2, CHR 3, vertical
        assert_eq!(banks(&m), (2, 7, 24, 28));
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        let mut m = board(78, 8, 8);
        m.cpu_write(0x8000, 0x08);
        assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
    }

    #[test]
    fn test_registers_below_8000() {
        let mut m = board(79, 4, 8);
        m.cpu_write(0x4000, 0x0F); // not a register address
        assert_eq!(banks(&m).2, 0);
        m.cpu_write(0x4100, 0x0D); // PRG 1, CHR 5
        assert_eq!(banks(&m), (2, 3, 40, 44));

        let mut m = board(87, 2, 4);
        m.cpu_write(0x6000, 1); // bits swapped: CHR bank 2
        assert_eq!(banks(&m).2, 16);

        let mut m = board(140, 8, 16);
        m.cpu_write(0x7000, 0x2A); // PRG 2, CHR 10
        assert_eq!(banks(&m), (4, 5, 80, 84));
    }

    #[test]
    fn test_quattro_blocks() {
        let mut m = board(232, 16, 0);
        m.cpu_write(0x8000, 0x10); // block 2
        m.cpu_write(0xC000, 1);
        assert_eq!((banks(&m).0, banks(&m).1), (9, 11));
    }

    #[test]
    fn test_cprom_chr_ram() {
        let mut m = board(13, 2, 0);
        m.cpu_write(0x8000, 2);
        m.ppu_write(0x1000, 0xAB);
        m.cpu_write(0x8000, 0);
        assert_eq!(m.ppu_read(0x1000), 0, "bank 0 is a different 4 KiB");
        assert_eq!(m.ppu_read(0x0000), 0);
        m.cpu_write(0x8000, 2);
        assert_eq!(m.ppu_read(0x1000), 0xAB);
    }
}
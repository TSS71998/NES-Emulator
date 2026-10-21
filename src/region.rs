//! Console regions: NTSC (North America, Japan), PAL (Europe, Australia) and
//! Dendy (the Russian Famiclone). They differ in clock speeds, frame length
//! and some APU timing tables. Everything here derives from each console's
//! master crystal, so the rates are exact rather than rounded.

use crate::cartridge::{Rom, RomFormat, Timing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Ntsc,
    Pal,
    Dendy,
}

/// APU frame sequencer step times, in CPU cycles since the sequence started.
/// The first three steps are quarter frames; `four_step_end` is the last step
/// of 4-step mode (quarter + half frame) and `five_step_end` of 5-step mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSequencerTiming {
    pub steps: [u32; 3],
    pub four_step_end: u32,
    pub five_step_end: u32,
}

const NTSC_NOISE_PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];
const PAL_NOISE_PERIODS: [u16; 16] = [
    4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778,
];

const NTSC_DMC_RATES: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];
const PAL_DMC_RATES: [u16; 16] = [
    398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50,
];

const NTSC_FRAME_SEQUENCER: FrameSequencerTiming = FrameSequencerTiming {
    steps: [7457, 14913, 22371],
    four_step_end: 29829,
    five_step_end: 37281,
};
const PAL_FRAME_SEQUENCER: FrameSequencerTiming = FrameSequencerTiming {
    steps: [8313, 16627, 24939],
    four_step_end: 33253,
    five_step_end: 41565,
};

pub const DOTS_PER_SCANLINE: u32 = 341;

impl Region {
    pub fn name(self) -> &'static str {
        match self {
            Region::Ntsc => "NTSC",
            Region::Pal => "PAL",
            Region::Dendy => "Dendy",
        }
    }

    /// The console's master crystal, in Hz. NTSC's is 236.25 MHz / 11
    /// (6 times the colour subcarrier); PAL and Dendy use 26.6017125 MHz.
    pub fn master_clock_hz(self) -> f64 {
        match self {
            Region::Ntsc => 236_250_000.0 / 11.0,
            Region::Pal | Region::Dendy => 26_601_712.5,
        }
    }

    /// Master clock ticks per CPU cycle.
    pub fn cpu_divider(self) -> u32 {
        match self {
            Region::Ntsc => 12,
            Region::Pal => 16,
            Region::Dendy => 15,
        }
    }

    /// Master clock ticks per PPU dot.
    pub fn ppu_divider(self) -> u32 {
        match self {
            Region::Ntsc => 4,
            Region::Pal | Region::Dendy => 5,
        }
    }

    /// CPU clock in Hz: 1.789773 MHz (NTSC), 1.662607 MHz (PAL), 1.773448 MHz (Dendy).
    pub fn cpu_clock_hz(self) -> f64 {
        self.master_clock_hz() / self.cpu_divider() as f64
    }

    /// PPU dots per CPU cycle: 3 on NTSC and Dendy, 3.2 on PAL.
    pub fn ppu_dots_per_cpu_cycle(self) -> f64 {
        self.cpu_divider() as f64 / self.ppu_divider() as f64
    }

    /// Scanlines per frame, including vblank and the pre-render line.
    pub fn scanlines_per_frame(self) -> u16 {
        match self {
            Region::Ntsc => 262,
            Region::Pal | Region::Dendy => 312,
        }
    }

    /// The scanline on which vblank (and the NMI) starts. Dendy keeps PAL's
    /// 312 lines but starts vblank 50 lines later, so its vblank is as short
    /// as NTSC's and NTSC games run unmodified.
    pub fn vblank_scanline(self) -> u16 {
        match self {
            Region::Ntsc | Region::Pal => 241,
            Region::Dendy => 291,
        }
    }

    /// The last line of the frame, where the PPU prefetches for line 0.
    pub fn prerender_scanline(self) -> u16 {
        self.scanlines_per_frame() - 1
    }

    /// NTSC skips one dot on the pre-render line of every other frame while
    /// rendering is on. PAL and Dendy don't.
    pub fn skips_odd_frame_dot(self) -> bool {
        self == Region::Ntsc
    }

    /// Average PPU dots per frame with rendering on.
    pub fn dots_per_frame(self) -> f64 {
        let dots = (DOTS_PER_SCANLINE * self.scanlines_per_frame() as u32) as f64;
        if self.skips_odd_frame_dot() {
            dots - 0.5
        } else {
            dots
        }
    }

    /// Average CPU cycles per frame: 29780.5 (NTSC), 33247.5 (PAL), 35464 (Dendy).
    pub fn cpu_cycles_per_frame(self) -> f64 {
        self.dots_per_frame() / self.ppu_dots_per_cpu_cycle()
    }

    /// Frame rate: 60.0988 (NTSC), 50.0070 (PAL and Dendy).
    pub fn frames_per_second(self) -> f64 {
        self.master_clock_hz() / (self.ppu_divider() as f64 * self.dots_per_frame())
    }

    /// Noise channel timer periods, in CPU cycles.
    pub fn noise_periods(self) -> &'static [u16; 16] {
        match self {
            Region::Pal => &PAL_NOISE_PERIODS,
            Region::Ntsc | Region::Dendy => &NTSC_NOISE_PERIODS,
        }
    }

    /// DMC output rates, in CPU cycles per bit.
    pub fn dmc_rates(self) -> &'static [u16; 16] {
        match self {
            Region::Pal => &PAL_DMC_RATES,
            Region::Ntsc | Region::Dendy => &NTSC_DMC_RATES,
        }
    }

    /// The Dendy's CPU is an NTSC-style clone, so it uses NTSC APU timings.
    pub fn frame_sequencer(self) -> FrameSequencerTiming {
        match self {
            Region::Pal => PAL_FRAME_SEQUENCER,
            Region::Ntsc | Region::Dendy => NTSC_FRAME_SEQUENCER,
        }
    }

    pub fn parse(s: &str) -> Option<Region> {
        match s.to_ascii_lowercase().as_str() {
            "ntsc" => Some(Region::Ntsc),
            "pal" => Some(Region::Pal),
            "dendy" => Some(Region::Dendy),
            _ => None,
        }
    }

    /// Picks the region for a ROM. A NES 2.0 header states it outright. Old
    /// iNES headers rarely do (the PAL bit is almost never set), so for those
    /// the usual "(E)", "(Europe)" or "(PAL)" tags in the file name count too.
    /// Returns the region and where it came from.
    pub fn detect(rom: &Rom, file_name: &str) -> (Region, &'static str) {
        let from_header = match rom.timing {
            Timing::Pal => Some(Region::Pal),
            Timing::Dendy => Some(Region::Dendy),
            Timing::Ntsc | Timing::MultiRegion => None,
        };
        if let Some(region) = from_header {
            return (region, "ROM header");
        }
        if rom.format == RomFormat::Nes2 {
            return (Region::Ntsc, "ROM header");
        }

        let name = file_name.to_ascii_lowercase();
        let pal_tags = ["(e)", "(europe)", "(pal)", "(eu)", "(a)", "(australia)"];
        if pal_tags.iter().any(|tag| name.contains(tag)) {
            return (Region::Pal, "file name");
        }
        (Region::Ntsc, "default")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    /// The front-loading NES (NES-001), whose reset button resets the PPU
    /// as well as the CPU.
    Nes,
    /// The top-loading NES (NES-101): reset only resets the CPU.
    TopLoader,
    /// The Famicom: reset only resets the CPU, and controller 2 has a
    /// microphone, read on bit 2 of $4016.
    Famicom,
}

impl Console {
    pub fn parse(s: &str) -> Option<Console> {
        match s.to_ascii_lowercase().as_str() {
            "nes" | "frontloader" | "nes-001" => Some(Console::Nes),
            "toploader" | "nes-101" => Some(Console::TopLoader),
            "famicom" | "fc" => Some(Console::Famicom),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Console::Nes => "NES",
            Console::TopLoader => "top-loading NES",
            Console::Famicom => "Famicom",
        }
    }

    pub fn reset_resets_ppu(self) -> bool {
        self == Console::Nes
    }

    /// The bits of a $4016 and a $4017 read that nothing drives, so they
    /// keep the last value on the data bus (open bus). Bits 5-7 never
    /// connect; the top-loader also leaves bit 2 of $4016 open, and the
    /// Famicom bits 3-4 of $4016 (it has no expansion lines there).
    /// https://www.nesdev.org/wiki/Controller_port_registers
    pub fn controller_open_bus(self) -> [u8; 2] {
        match self {
            Console::Nes => [0xE0, 0xE0],
            Console::TopLoader => [0xE4, 0xE0],
            Console::Famicom => [0xF8, 0xE0],
        }
    }

    /// Guesses the console from the file name: Japanese releases ("(J)",
    /// "(Japan)") are Famicom games. Nothing in an iNES or NES 2.0 header
    /// tells a Famicom cartridge from an NES one.
    pub fn detect(file_name: &str) -> (Console, &'static str) {
        let name = file_name.to_ascii_lowercase();
        if ["(j)", "(japan)"].iter().any(|tag| name.contains(tag)) {
            return (Console::Famicom, "file name");
        }
        (Console::Nes, "default")
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn close(a: f64, b: f64, tolerance: f64) -> bool {
        (a - b).abs() < tolerance
    }

    #[test]
    fn test_clock_rates() {
        assert!(close(Region::Ntsc.cpu_clock_hz(), 1_789_772.727, 0.001));
        assert!(close(Region::Pal.cpu_clock_hz(), 1_662_607.031, 0.001));
        assert!(close(Region::Dendy.cpu_clock_hz(), 1_773_447.5, 0.001));
        assert_eq!(Region::Ntsc.ppu_dots_per_cpu_cycle(), 3.0);
        assert_eq!(Region::Pal.ppu_dots_per_cpu_cycle(), 3.2);
        assert_eq!(Region::Dendy.ppu_dots_per_cpu_cycle(), 3.0);
    }

    #[test]
    fn test_frame_timing() {
        assert_eq!(Region::Ntsc.cpu_cycles_per_frame(), 29780.5);
        assert_eq!(Region::Pal.cpu_cycles_per_frame(), 33247.5);
        assert_eq!(Region::Dendy.cpu_cycles_per_frame(), 35464.0);
        assert!(close(Region::Ntsc.frames_per_second(), 60.0988, 0.0001));
        assert!(close(Region::Pal.frames_per_second(), 50.0070, 0.0001));
        assert!(close(Region::Dendy.frames_per_second(), 50.0070, 0.0001));
    }

    #[test]
    fn test_parse() {
        assert_eq!(Region::parse("PAL"), Some(Region::Pal));
        assert_eq!(Region::parse("dendy"), Some(Region::Dendy));
        assert_eq!(Region::parse("secam"), None);
    }

    fn rom(format: RomFormat, timing: Timing) -> Rom {
        let mut rom = crate::mapper::test_util::banked_rom(0, 2, 1, false);
        rom.format = format;
        rom.timing = timing;
        rom
    }

    #[test]
    fn test_detect_from_header() {
        let pal = rom(RomFormat::Nes2, Timing::Pal);
        assert_eq!(
            Region::detect(&pal, "Game (U).nes"),
            (Region::Pal, "ROM header")
        );
        let dendy = rom(RomFormat::Nes2, Timing::Dendy);
        assert_eq!(Region::detect(&dendy, "game.nes").0, Region::Dendy);
        // an iNES header with the PAL bit set
        let ines_pal = rom(RomFormat::INes, Timing::Pal);
        assert_eq!(Region::detect(&ines_pal, "game.nes").0, Region::Pal);
    }

    #[test]
    fn test_nes2_ntsc_header_beats_file_name() {
        let ntsc = rom(RomFormat::Nes2, Timing::Ntsc);
        assert_eq!(
            Region::detect(&ntsc, "Game (Europe).nes"),
            (Region::Ntsc, "ROM header")
        );
    }

    #[test]
    fn test_detect_from_file_name_for_ines() {
        let ines = rom(RomFormat::INes, Timing::Ntsc);
        assert_eq!(
            Region::detect(&ines, "Tetris (E) [!].nes"),
            (Region::Pal, "file name")
        );
        assert_eq!(
            Region::detect(&ines, "Super Mario Bros. 3 (Europe).nes").0,
            Region::Pal
        );
        assert_eq!(
            Region::detect(&ines, "Tetris (U).nes"),
            (Region::Ntsc, "default")
        );
        assert_eq!(Region::detect(&ines, "eagle.nes").0, Region::Ntsc);
    }

    #[test]
    fn test_console_parse_and_detect() {
        assert_eq!(Console::parse("famicom"), Some(Console::Famicom));
        assert_eq!(Console::parse("TopLoader"), Some(Console::TopLoader));
        assert_eq!(Console::parse("nes"), Some(Console::Nes));
        assert_eq!(Console::parse("snes"), None);
        assert_eq!(
            Console::detect("Super Mario Bros. 3 (J).nes"),
            (Console::Famicom, "file name")
        );
        assert_eq!(
            Console::detect("Final Fantasy (Japan).nes").0,
            Console::Famicom
        );
        assert_eq!(
            Console::detect("Final Fantasy (USA).nes"),
            (Console::Nes, "default")
        );
    }
}

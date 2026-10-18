use crate::apu::APU;
use crate::cpu::Mem;
use crate::joypad::Joypad;
use crate::mapper::SharedMapper;
use crate::ppu::NesPPU;
use crate::region::Region;

//  _______________ $10000  _______________
// | PRG-ROM       |       |               |
// | Upper Bank    |       |               |
// |_ _ _ _ _ _ _ _| $C000 | PRG-ROM       |
// | PRG-ROM       |       |               |
// | Lower Bank    |       |               |
// |_______________| $8000 |_______________|
// | SRAM          |       | SRAM          |
// |_______________| $6000 |_______________|
// | Expansion ROM |       | Expansion ROM |
// |_______________| $4020 |_______________|
// | I/O Registers |       |               |
// |_ _ _ _ _ _ _ _| $4000 |               |
// | Mirrors       |       | I/O Registers |
// | $2000-$2007   |       |               |
// |_ _ _ _ _ _ _ _| $2008 |               |
// | I/O Registers |       |               |
// |_______________| $2000 |_______________|
// | Mirrors       |       |               |
// | $0000-$07FF   |       |               |
// |_ _ _ _ _ _ _ _| $0800 |               |
// | RAM           |       | RAM           |
// |_ _ _ _ _ _ _ _| $0200 |               |
// | Stack         |       |               |
// |_ _ _ _ _ _ _ _| $0100 |               |
// | Zero Page     |       |               |
// |_______________| $0000 |_______________|

pub struct Bus<'call> {
    cpu_vram: [u8; 2048],
    mapper: SharedMapper,
    ppu: NesPPU,
    apu: APU,

    need_halt: bool,
    need_dummy_read: bool,
    dmc_dma_running: bool,
    oam_dma_page: Option<u8>,
    dma_joypad_held: bool,

    // Last value driven on the CPU data bus. Reads from addresses nothing
    // responds to return it ("open bus") instead of panicking.
    open_bus: u8,

    cycles: usize,
    region: Region,

    cpu_divider: u64,
    ppu_divider: u64,
    read_point: u64,
    write_point: u64,

    // Master clock ticks (see Region) since power-on, and how far the PPU
    // has been run. Each CPU cycle is split around its memory access, so the
    // PPU has run to the right dot when a register is read or written.
    master_clock: u64,
    ppu_clock: u64,
    /// CPU cycles used so far by the current instruction (see `cpu_read`).
    instruction_cycles: u8,

    // Interrupt lines as the CPU sees them. The CPU samples the NMI line
    // every cycle and latches a rising edge; it acts on interrupts sampled
    // at the end of an instruction's second-to-last cycle.
    nmi_line: bool,
    need_nmi: bool,
    prev_need_nmi: bool,
    run_irq: bool,
    prev_run_irq: bool,
    /// The CPU's I flag, which masks IRQs (see CPU::sync_irq_mask)
    irq_mask: bool,

    gameloop_callback: Box<dyn FnMut(&NesPPU, &mut [Joypad; 2]) + 'call>,
    audio_callback: Option<Box<dyn FnMut(&[f32]) -> f64 + 'call>>,
    joypads: [Joypad; 2],
}

impl<'a> Bus<'a> {
    pub fn new<'call, F>(mapper: SharedMapper, gameloop_callback: F) -> Bus<'call>
    where
        F: FnMut(&NesPPU, &mut [Joypad; 2]) + 'call,
    {
        Bus {
            cpu_vram: [0; 2048],
            ppu: NesPPU::new(mapper.clone()),
            mapper,
            apu: APU::new(44100),
            need_halt: false,
            need_dummy_read: false,
            dmc_dma_running: false,
            oam_dma_page: None,
            dma_joypad_held: false,
            open_bus: 0,
            cycles: 0,
            master_clock: 0,
            ppu_clock: 0,
            instruction_cycles: 0,
            nmi_line: false,
            need_nmi: false,
            prev_need_nmi: false,
            run_irq: false,
            prev_run_irq: false,
            irq_mask: true,
            region: Region::Ntsc,
            cpu_divider: Region::Ntsc.cpu_divider() as u64,
            ppu_divider: Region::Ntsc.ppu_divider() as u64,
            read_point: Region::Ntsc.cpu_divider() as u64 / 2 - 1,
            write_point: Region::Ntsc.cpu_divider() as u64 / 2 + 1,
            gameloop_callback: Box::from(gameloop_callback),
            audio_callback: None,
            joypads: [Joypad::new(), Joypad::new()],
        }
    }

    pub fn set_audio_callback<F>(&mut self, sample_rate: u32, callback: F)
    where
        F: FnMut(&[f32]) -> f64 + 'a,
    {
        self.apu.set_sample_rate(sample_rate);
        self.audio_callback = Some(Box::new(callback));
    }

    pub fn reset(&mut self) {
        self.apu.reset();
        self.ppu.reset();
        self.need_halt = false;
        self.need_dummy_read = false;
        self.dmc_dma_running = false;
        self.oam_dma_page = None;
        self.need_nmi = false;
        self.prev_need_nmi = false;
        self.instruction_cycles = 0;
    }

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.cpu_divider = region.cpu_divider() as u64;
        self.ppu_divider = region.ppu_divider() as u64;
        self.read_point = self.cpu_divider / 2 - 1;
        self.write_point = self.cpu_divider / 2 + 1;
        self.ppu.set_region(region);
        self.apu.set_region(region);
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn cpu_cycles(&self) -> usize {
        self.cycles
    }

    fn cartridge_read(&self, addr: u16) -> u8 {
        self.mapper.borrow().cpu_read(addr).unwrap_or(self.open_bus)
    }

    pub fn tick(&mut self, cycles: u8) {
        for _ in 0..cycles {
            self.idle_cycle();
        }
        self.instruction_cycles = 0;
    }

    pub fn cpu_read(&mut self, addr: u16) -> u8 {
        if self.need_halt {
            self.run_dma(Some(addr));
        }
        self.instruction_cycles = self.instruction_cycles.saturating_add(1);
        self.start_cycle(true);
        let value = self.mem_read(addr);
        self.end_cycle(true);
        value
    }

    pub fn cpu_write(&mut self, addr: u16, data: u8) {
        self.instruction_cycles = self.instruction_cycles.saturating_add(1);
        self.start_cycle(false);
        self.mem_write(addr, data);
        self.end_cycle(false);
    }

    pub fn idle_cycle(&mut self) {
        if self.need_halt {
            self.run_dma(None);
        }
        self.instruction_cycles = self.instruction_cycles.saturating_add(1);
        self.start_cycle(true);
        self.end_cycle(true);
    }

    pub fn instruction_cycles(&self) -> u8 {
        self.instruction_cycles
    }

    pub fn finish_instruction(&mut self, cycles: u8) {
        while self.instruction_cycles < cycles {
            self.idle_cycle();
        }
        self.instruction_cycles = 0;
    }

    /// Runs pending DMA, halting the CPU on the read of `read_addr` (`None`
    /// for a read we don't model the address of). As on hardware (and in
    /// Mesen, which this follows):
    ///
    /// - The first cycle is the halt: the CPU's read happens and is ignored,
    ///   then gets repeated on every cycle the DMA unit isn't using the bus.
    ///   Those repeats have side effects ($2007 advances, $4016 clocks the
    ///   controller), which is what blargg's dmc_dma_during_read4 checks.
    /// - DMA then alternates "get" (read) and "put" (write) cycles, by the
    ///   parity of the CPU cycle count. OAM DMA reads a byte on a get cycle
    ///   and writes it to $2004 on the next put cycle: 256 pairs, plus the
    ///   halt and possibly one alignment cycle, giving 513 or 514 cycles.
    /// - A DMC fetch needs a halt and a dummy cycle, then reads its byte on
    ///   a get cycle: 3 or 4 cycles alone. During OAM DMA the OAM cycles
    ///   count as its halt and dummy, so it usually adds just 2.
    fn run_dma(&mut self, read_addr: Option<u16>) {
        self.dma_joypad_held = false;
        self.dma_cycle(|bus| bus.dma_dummy_read(read_addr));
        self.need_halt = false;

        let mut oam_cycles = 0u16;
        let mut oam_low = 0u8;
        let mut oam_value = 0u8;
        while self.dmc_dma_running || self.oam_dma_page.is_some() {
            let get_cycle = self.cycles % 2 == 0;
            if get_cycle {
                if self.dmc_dma_running && !self.need_halt && !self.need_dummy_read {
                    self.dma_cycle(|bus| {
                        bus.dma_joypad_held = false; // the DMA unit takes the bus
                        if let Some(addr) = bus.apu.dmc_fetch_addr() {
                            let byte = bus.mem_read(addr);
                            bus.apu.dmc_provide_byte(byte);
                        }
                    });
                    self.dmc_dma_running = false;
                } else if let Some(page) = self.oam_dma_page {
                    let addr = (page as u16) << 8 | oam_low as u16;
                    self.dma_cycle(|bus| {
                        bus.dma_joypad_held = false;
                        oam_value = bus.mem_read(addr);
                    });
                    oam_low = oam_low.wrapping_add(1);
                    oam_cycles += 1;
                } else {
                    self.dma_cycle(|bus| bus.dma_dummy_read(read_addr));
                }
            } else if self.oam_dma_page.is_some() && oam_cycles % 2 == 1 {
                self.dma_cycle(|bus| {
                    bus.dma_joypad_held = false;
                    bus.ppu.cpu_write(4, oam_value);
                });
                oam_cycles += 1;
                if oam_cycles == 512 {
                    self.oam_dma_page = None;
                }
            } else {
                self.dma_cycle(|bus| bus.dma_dummy_read(read_addr));
            }
        }
    }

    /// One CPU cycle taken by the DMA unit. A cycle spent on OAM DMA counts
    /// as the DMC's halt or dummy cycle if it needs one.
    fn dma_cycle(&mut self, access: impl FnOnce(&mut Self)) {
        if self.need_halt {
            self.need_halt = false;
        } else if self.need_dummy_read {
            self.need_dummy_read = false;
        }
        self.start_cycle(true);
        access(self);
        self.end_cycle(true);
    }

    /// The halted CPU's read, repeated. The controller ports only see the
    /// first of a run of these: the address stays on $4016 the whole time,
    /// so the controller gets one read signal, not one per cycle. (Reads of
    /// $2007 do repeat, since the PPU acts on every read cycle.)
    fn dma_dummy_read(&mut self, read_addr: Option<u16>) {
        let Some(addr) = read_addr else { return };
        let joypad = addr == 0x4016 || addr == 0x4017;
        if !(joypad && self.dma_joypad_held) {
            self.mem_read(addr);
        }
        self.dma_joypad_held = joypad;
    }

    /// Master clock ticks from the start of a CPU cycle to its access. Reads
    /// land a little before the middle of the cycle and writes a little
    /// after (on NTSC 5 and 7 of 12 ticks), as in Mesen.
    #[inline]
    fn access_point(&self, read: bool) -> u64 {
        if read {
            self.read_point
        } else {
            self.write_point
        }
    }

    fn start_cycle(&mut self, read: bool) {
        self.cycles += 1;
        self.master_clock += self.access_point(read);
        self.run_ppu();

        self.apu.tick();
        if !self.dmc_dma_running && self.apu.dmc_fetch_addr().is_some() {
            self.dmc_dma_running = true;
            self.need_halt = true;
            self.need_dummy_read = true;
        }
    }

    fn end_cycle(&mut self, read: bool) {
        self.master_clock += self.cpu_divider - self.access_point(read);
        self.run_ppu();

        // NMI is edge-triggered: the CPU latches the line going high. What
        // it acts on is the state at the end of the previous cycle.
        self.prev_need_nmi = self.need_nmi;
        let nmi_line = self.ppu.nmi_line();
        if nmi_line && !self.nmi_line {
            self.need_nmi = true;
        }
        self.nmi_line = nmi_line;

        self.prev_run_irq = self.run_irq;
        self.run_irq = !self.irq_mask && self.irq_line();
    }

    fn run_ppu(&mut self) {
        let divider = self.ppu_divider;
        while self.ppu_clock + divider <= self.master_clock {
            self.ppu_clock += divider;
            if self.ppu.step_dot() {
                if let Some(callback) = self.audio_callback.as_mut() {
                    let samples = self.apu.take_samples();
                    let rate_adjust = callback(&samples);
                    self.apu.set_rate_adjust(rate_adjust);
                }
                (self.gameloop_callback)(&self.ppu, &mut self.joypads);
            }
        }
    }

    pub fn take_nmi(&mut self) -> bool {
        if self.prev_need_nmi {
            self.prev_need_nmi = false;
            self.need_nmi = false;
            true
        } else {
            false
        }
    }

    pub fn hijack_nmi(&mut self) -> bool {
        std::mem::take(&mut self.need_nmi)
    }

    pub fn defer_interrupt_poll(&mut self) {
        self.prev_need_nmi = false;
        self.prev_run_irq = false;
    }

    pub fn ignore_new_irq(&mut self) {
        if self.run_irq && !self.prev_run_irq {
            self.run_irq = false;
        }
    }

    pub fn irq_ready(&self) -> bool {
        self.prev_run_irq
    }

    pub fn set_irq_mask(&mut self, masked: bool) {
        self.irq_mask = masked;
    }

    pub fn irq_line(&self) -> bool {
        self.apu.irq_pending() || self.mapper.borrow().irq_pending()
    }

    pub fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize],
            0x4020..=0xFFFF => self.cartridge_read(addr),
            _ => self.open_bus,
        }
    }

    pub fn peek_u16(&self, addr: u16) -> u16 {
        let lo = self.peek(addr) as u16;
        let hi = self.peek(addr.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }
}

impl Mem for Bus<'_> {
    fn mem_read(&mut self, addr: u16) -> u8 {
        if addr == 0x4015 {
            return (self.apu.read_status() & !0x20) | (self.open_bus & 0x20);
        }
        let value = match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => self.ppu.cpu_read(addr & 7),
            0x4016 => (self.open_bus & 0xE0) | self.joypads[0].read(),
            0x4017 => (self.open_bus & 0xE0) | self.joypads[1].read(),

            0x4000..=0x401F => self.open_bus,

            0x4020..=0xFFFF => self.cartridge_read(addr),
        };
        self.open_bus = value;
        value
    }

    fn mem_write(&mut self, addr: u16, data: u8) {
        self.open_bus = data;
        match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize] = data,

            0x2000..=0x3FFF => self.ppu.cpu_write(addr & 7, data),

            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, data),

            0x4014 => {
                self.oam_dma_page = Some(data);
                self.need_halt = true;
            }

            0x4016 => {
                self.joypads[0].write(data);
                self.joypads[1].write(data);
            }

            0x4018..=0x401F => { /* CPU test mode registers */ }

            0x4020..=0xFFFF => {
                self.mapper
                    .borrow_mut()
                    .cpu_write_on_cycle(addr, data, self.cycles);
                self.ppu.refresh_mirroring();
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cartridge::test;

    fn test_bus<'a>() -> Bus<'a> {
        Bus::new(test::test_mapper(), |_ppu, _joypads| {})
    }

    #[test]
    fn test_mem_read_write_to_ram() {
        let mut bus = test_bus();
        bus.mem_write(0x01, 0x55);
        assert_eq!(bus.mem_read(0x01), 0x55);
        assert_eq!(bus.mem_read(0x0801), 0x55);
    }

    #[test]
    fn test_write_only_registers_read_open_bus() {
        let mut bus = test_bus();
        bus.mem_write(0x01, 0xA5);
        assert_eq!(bus.mem_read(0x01), 0xA5);
        assert_eq!(bus.mem_read(0x4000), 0xA5);
        assert_eq!(bus.mem_read(0x5000), 0xA5); // nothing mapped on NROM
    }

    #[test]
    fn test_writes_to_rom_and_ppustatus_are_ignored() {
        let mut bus = test_bus();
        let before = bus.mem_read(0x8000);
        bus.mem_write(0x8000, before.wrapping_add(1));
        bus.mem_write(0x2002, 0xFF);
        assert_eq!(bus.mem_read(0x8000), before);
    }

    #[test]
    fn test_prg_ram_through_bus() {
        let mut bus = test_bus();
        bus.mem_write(0x6000, 0x12);
        assert_eq!(bus.mem_read(0x6000), 0x12);
    }

    #[test]
    fn test_peek_has_no_side_effects() {
        let mut bus = test_bus();
        bus.ppu.status.set_vblank_status(true);
        bus.peek(0x2002);
        assert!(bus.ppu.status.is_in_vblank());
        bus.mem_read(0x2002);
        assert!(!bus.ppu.status.is_in_vblank());
    }

    #[test]
    fn test_controller_2() {
        use crate::joypad::JoypadButton;
        let mut bus = test_bus();
        bus.joypads[1].set_button_pressed_status(JoypadButton::BUTTON_A, true);
        bus.mem_write(0x4016, 1);
        bus.mem_write(0x4016, 0);
        assert_eq!(bus.mem_read(0x4016) & 1, 0);
        assert_eq!(bus.mem_read(0x4017) & 1, 1);
    }

    fn cycles_per_frame(region: Region, frames: usize) -> f64 {
        use std::cell::Cell;
        use std::rc::Rc;
        let count = Rc::new(Cell::new(0usize));
        let seen = count.clone();
        let mut bus = Bus::new(test::test_mapper(), move |_ppu, _joypads| {
            seen.set(seen.get() + 1)
        });
        bus.set_region(region);
        bus.mem_write(0x2001, 0x08); // rendering on, so NTSC's odd-frame skip applies
        while count.get() < 1 {
            bus.tick(1);
        }
        let start = bus.cpu_cycles();
        while count.get() < 1 + frames {
            bus.tick(1);
        }
        (bus.cpu_cycles() - start) as f64 / frames as f64
    }

    #[test]
    fn test_cpu_cycles_per_frame_by_region() {
        for region in [Region::Ntsc, Region::Pal, Region::Dendy] {
            let measured = cycles_per_frame(region, 20);
            let expected = region.cpu_cycles_per_frame();
            assert!(
                (measured - expected).abs() <= 0.5,
                "{:?}: {} cycles per frame, expected {}",
                region,
                measured,
                expected
            );
        }
    }

    #[test]
    fn test_ppu_registers_read_the_ppu_latch_not_cpu_open_bus() {
        let mut bus = test_bus();
        bus.mem_write(0x2003, 0xB2); // any PPU write fills the PPU's latch
        bus.mem_write(0x01, 0x00);
        assert_eq!(bus.mem_read(0x01), 0x00);
        assert_eq!(bus.mem_read(0x2001), 0xB2); // write-only: the latch
        assert_eq!(bus.mem_read(0x2002) & 0x1F, 0x12); // low 5 bits of $2002 too
    }

    #[test]
    fn test_oam_dma_waits_for_a_read_then_copies_the_page() {
        let mut bus = test_bus();
        for i in 0..256u16 {
            bus.mem_write(0x0200 + i, i as u8);
        }
        bus.cpu_write(0x4014, 0x02);
        let start = bus.cpu_cycles();
        bus.cpu_write(0x0000, 0x00); // writes can't be halted
        assert_eq!(bus.cpu_cycles() - start, 1);

        let start = bus.cpu_cycles();
        bus.cpu_read(0x0000); // halts here: 513 or 514 cycles, then the read
        let taken = bus.cpu_cycles() - start;
        assert!(taken == 514 || taken == 515, "took {}", taken);
        assert_eq!(bus.ppu.oam_data[0x01], 0x01);
        assert_eq!(bus.ppu.oam_data[0x02], 0x02 & 0xE3); // attribute byte
        assert_eq!(bus.ppu.oam_data[0xFF], 0xFF);
    }

    #[test]
    fn test_oam_dma_length_depends_on_cycle_parity() {
        let mut lengths = vec![];
        for offset in 0..2 {
            let mut bus = test_bus();
            bus.tick(offset);
            bus.cpu_write(0x4014, 0x02);
            let start = bus.cpu_cycles();
            bus.cpu_read(0x0000);
            lengths.push(bus.cpu_cycles() - start - 1);
        }
        lengths.sort();
        assert_eq!(lengths, vec![513, 514]);
    }
}

use crate::apu::APU;
use crate::cpu::Mem;
use crate::joypad::Joypad;
use crate::mapper::SharedMapper;
use crate::ppu::NesPPU;
use crate::region::{Console, Region};
use crate::state::{StateReader, StateWriter};

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

/// Called once per frame with the finished picture, to read the controllers.
type GameLoopCallback<'call> = Box<dyn FnMut(&NesPPU, &mut [Joypad; 2]) + 'call>;
/// Called once per frame with the audio samples; returns the rate adjustment.
type AudioCallback<'call> = Box<dyn FnMut(&[f32]) -> f64 + 'call>;

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
    abort_dmc_dma: bool,
    // The 2A03 has its own internal data bus too, between the CPU, the DMA
    // unit and the APU/controller registers. It follows the external bus,
    // except that $4015 reads only reach the internal one and DMC fetches
    // only the external one. Bit 5 of $4015 and the floating bits of the
    // controller ports read it.
    internal_bus: u8,
    /// The last value written to $4016, waiting for the controllers to see
    /// it (see start_cycle).
    pending_strobe: Option<u8>,
    last_read_halted: bool,

    cycles: usize,
    region: Region,
    console: Console,

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

    gameloop_callback: GameLoopCallback<'call>,
    audio_callback: Option<AudioCallback<'call>>,
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
            abort_dmc_dma: false,
            internal_bus: 0,
            pending_strobe: None,
            last_read_halted: false,
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
            console: Console::Nes,
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
        if self.console.reset_resets_ppu() {
            self.ppu.reset();
        }
        self.need_halt = false;
        self.need_dummy_read = false;
        self.dmc_dma_running = false;
        self.oam_dma_page = None;
        self.need_nmi = false;
        self.prev_need_nmi = false;
        self.instruction_cycles = 0;
        self.abort_dmc_dma = false;
    }

    fn controller_read(&mut self, addr: u16) -> u8 {
        let port = (addr & 1) as usize;
        let open_bus = self.console.controller_open_bus()[port];
        let mut value = (self.internal_bus & open_bus) | self.joypads[port].read();
        if addr == 0x4016 && self.console == Console::Famicom && self.joypads[1].microphone {
            value |= 0x04;
        }
        value
    }

    pub fn set_console(&mut self, console: Console) {
        self.console = console;
    }

    /// See NesPPU::set_oam_row_corruption.
    pub fn set_oam_row_corruption(&mut self, on: bool) {
        self.ppu.set_oam_row_corruption(on);
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
        self.last_read_halted = self.need_halt;
        if self.need_halt {
            self.run_dma(Some(addr));
        }
        self.instruction_cycles = self.instruction_cycles.saturating_add(1);
        self.start_cycle(true);
        let oam_data = (0x2000..=0x3FFF).contains(&addr) && addr & 7 == 4;
        if oam_data {
            // $2004 is sampled a dot later than the other registers (the
            // PPU's OAM latch updates on the first half of the dot)
            self.master_clock += self.ppu_divider;
            self.run_ppu();
            self.master_clock -= self.ppu_divider;
        }
        let value = self.mem_read(addr);
        self.end_cycle(true);
        if (0x2000..=0x3FFF).contains(&addr) && addr & 7 == 2 {
            return self.late_2002_bits(value);
        }
        value
    }

    /// A $2002 read latches the vblank flag when M2 rises (the read point),
    /// but the sprite 0 hit and overflow flags only when M2 falls, at the end
    /// of the CPU cycle: about 2 PPU dots later. So those two bits are taken
    /// from the PPU as it is now.
    fn late_2002_bits(&mut self, value: u8) -> u8 {
        let value = (value & !0x60) | self.ppu.late_sprite_flags();
        self.ppu.refresh_status_latch(value);
        self.open_bus = value;
        self.internal_bus = value;
        value
    }

    pub fn last_read_halted(&self) -> bool {
        self.last_read_halted
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
        self.need_halt = false;
        self.start_cycle(true);
        if self.abort_dmc_dma {
            self.abort_dmc_dma = false;
            self.dmc_dma_running = false;
            self.need_dummy_read = false;
            if self.oam_dma_page.is_none() {
                self.end_cycle(true);
                return;
            }
        }
        self.dma_dummy_read(read_addr);
        self.end_cycle(true);

        let mut oam_cycles = 0u16;
        let mut oam_low = 0u8;
        let mut oam_value = 0u8;
        let mut prev_addr = read_addr.unwrap_or(0);
        while self.dmc_dma_running || self.oam_dma_page.is_some() {
            let get_cycle = self.cycles % 2 == 0;
            if get_cycle {
                if self.dmc_dma_running && !self.need_halt && !self.need_dummy_read {
                    self.dma_cycle(|bus| {
                        bus.dma_joypad_held = false; // the DMA unit takes the bus
                        let internal = bus.internal_bus;
                        let addr = bus.apu.dmc_addr();
                        let byte = bus.dma_read(addr, read_addr, &mut prev_addr);
                        bus.internal_bus = internal;
                        bus.apu.dmc_provide_byte(byte);
                    });
                    self.dmc_dma_running = false;
                    self.abort_dmc_dma = false;
                } else if let Some(page) = self.oam_dma_page {
                    let addr = (page as u16) << 8 | oam_low as u16;
                    self.dma_cycle(|bus| {
                        bus.dma_joypad_held = false;
                        oam_value = bus.dma_read(addr, read_addr, &mut prev_addr);
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
        if self.abort_dmc_dma {
            self.abort_dmc_dma = false;
            self.dmc_dma_running = false;
            self.need_halt = false;
            self.need_dummy_read = false;
        } else if self.need_halt {
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

    /// The DMC was turned off. A fetch the CPU hasn't halted for yet is
    /// cancelled outright; one under way stops at the start of the next DMA
    /// cycle (if that's its read cycle, the read still happens, but the
    /// byte is thrown away). Follows Mesen's NesCpu::StopDmcTransfer.
    fn stop_dmc_dma(&mut self) {
        if !self.dmc_dma_running {
            return;
        }
        if self.need_halt {
            self.dmc_dma_running = false;
            self.need_halt = self.oam_dma_page.is_some();
            self.need_dummy_read = false;
        } else {
            self.abort_dmc_dma = true;
        }
    }

    /// A read by the DMA unit. The 2A03's APU and controller registers only
    /// answer while the CPU's own address is in $4000-$401F, and then they
    /// decode just the low 5 bits of the bus address. So:
    ///
    /// - if the halted CPU was reading $4000-$401F, a DMA read of any
    ///   address also reads the internal register at $4000 + (address & $1F):
    ///   $4015 (clearing the frame interrupt), or a controller port. If RAM,
    ///   the PPU or the cartridge answers the address as well, the two
    ///   collide: the port's driven bits win and its floating bits come
    ///   from the outside value (DMC DMA Bus Conflicts).
    /// - otherwise nothing answers a DMA read of $4000-$401F: open bus.
    ///
    /// `prev_addr` is the last register read, since a second read of the
    /// same controller port in a row doesn't clock it again (NES only).
    /// See https://github.com/100thCoin/AccuracyCoin ("APU Register
    /// Activation") and Mesen's NesCpu::ProcessDmaRead.
    fn dma_read(&mut self, addr: u16, cpu_addr: Option<u16>, prev_addr: &mut u16) -> u8 {
        let registers_active = cpu_addr.is_some_and(|a| a & 0xFFE0 == 0x4000);
        if !registers_active {
            *prev_addr = addr;
            if (0x4000..=0x401F).contains(&addr) {
                self.internal_bus = self.open_bus;
                return self.open_bus;
            }
            return self.mem_read(addr);
        }
        let register = 0x4000 | (addr & 0x1F);
        let driven = register != addr && !(0x4000..0x6000).contains(&addr);
        let value = match register {
            0x4015 => {
                let value = self.mem_read(0x4015);
                if driven {
                    let internal = self.internal_bus;
                    self.mem_read(addr);
                    self.internal_bus = internal;
                }
                value
            }
            0x4016 | 0x4017 => {
                let nes = self.console != Console::Famicom;
                let value = if self.region == Region::Pal || (nes && *prev_addr == register) {
                    self.internal_bus
                } else {
                    self.controller_read(register)
                };
                let value = if driven {
                    let open = self.console.controller_open_bus()[(register & 1) as usize];
                    let outside = self.mem_read(addr);
                    (outside & open) | (value & !open)
                } else {
                    value
                };
                self.open_bus = value;
                self.internal_bus = value;
                value
            }
            _ => self.mem_read(addr),
        };
        *prev_addr = register;
        value
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
        if self.cycles % 2 == 0 {
            if let Some(value) = self.pending_strobe.take() {
                self.joypads[0].write(value);
                self.joypads[1].write(value);
            }
        }
        self.master_clock += self.access_point(read);
        self.run_ppu();

        self.apu.tick();
        if self.apu.take_dmc_stop() {
            self.stop_dmc_dma();
        }
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

    pub fn irq_poll(&self) -> bool {
        self.prev_run_irq
    }

    pub fn set_irq_poll(&mut self, irq: bool) {
        self.prev_run_irq = irq;
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
            let value = (self.apu.read_status() & !0x20) | (self.internal_bus & 0x20);
            self.internal_bus = value;
            return value;
        }
        let value = match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => self.ppu.cpu_read(addr & 7),

            0x4016 | 0x4017 => self.controller_read(addr),

            0x4000..=0x401F => self.open_bus,

            0x4020..=0xFFFF => self.cartridge_read(addr),
        };
        self.open_bus = value;
        self.internal_bus = value;
        value
    }

    fn mem_write(&mut self, addr: u16, data: u8) {
        self.open_bus = data;
        self.internal_bus = data;
        match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize] = data,

            0x2000..=0x3FFF => self.ppu.cpu_write(addr & 7, data),

            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, data),

            0x4014 => {
                self.oam_dma_page = Some(data);
                self.need_halt = true;
            }

            0x4016 => self.pending_strobe = Some(data),

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

impl Bus<'_> {
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.cpu_vram);
        w.bool(self.need_halt);
        w.bool(self.need_dummy_read);
        w.bool(self.dmc_dma_running);
        w.option_u8(self.oam_dma_page);
        w.bool(self.dma_joypad_held);
        w.u8(self.open_bus);
        w.bool(self.abort_dmc_dma);
        w.u8(self.internal_bus);
        w.option_u8(self.pending_strobe);
        w.bool(self.last_read_halted);
        w.usize(self.cycles);
        w.u64(self.master_clock);
        w.u64(self.ppu_clock);
        w.u8(self.instruction_cycles);
        w.bool(self.nmi_line);
        w.bool(self.need_nmi);
        w.bool(self.prev_need_nmi);
        w.bool(self.run_irq);
        w.bool(self.prev_run_irq);
        w.bool(self.irq_mask);
        for joypad in &self.joypads {
            joypad.save_state(w);
        }
        self.mapper.borrow().save_state(w);
        self.ppu.save_state(w);
        self.apu.save_state(w);
    }

    pub fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.cpu_vram);
        self.need_halt = r.bool();
        self.need_dummy_read = r.bool();
        self.dmc_dma_running = r.bool();
        self.oam_dma_page = r.option_u8();
        self.dma_joypad_held = r.bool();
        self.open_bus = r.u8();
        self.abort_dmc_dma = r.bool();
        self.internal_bus = r.u8();
        self.pending_strobe = r.option_u8();
        self.last_read_halted = r.bool();
        self.cycles = r.usize();
        self.master_clock = r.u64();
        self.ppu_clock = r.u64();
        self.instruction_cycles = r.u8();
        self.nmi_line = r.bool();
        self.need_nmi = r.bool();
        self.prev_need_nmi = r.bool();
        self.run_irq = r.bool();
        self.prev_run_irq = r.bool();
        self.irq_mask = r.bool();
        for joypad in &mut self.joypads {
            joypad.load_state(r);
        }
        self.mapper.borrow_mut().load_state(r);
        self.ppu.load_state(r);
        self.ppu.refresh_mirroring();
        self.apu.load_state(r);
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
        bus.cpu_write(0x4016, 1);
        bus.cpu_write(0x4016, 0);
        bus.idle_cycle();
        assert_eq!(bus.cpu_read(0x4016) & 1, 0);
        assert_eq!(bus.cpu_read(0x4017) & 1, 1);
    }

    #[test]
    fn test_reset_button_resets_the_ppu_only_on_the_front_loader() {
        for (console, resets) in [
            (Console::Nes, true),
            (Console::TopLoader, false),
            (Console::Famicom, false),
        ] {
            let mut bus = test_bus();
            bus.set_console(console);
            bus.mem_write(0x2000, 0x80);
            bus.reset();
            assert_eq!(bus.ppu.ctrl.bits() == 0, resets, "{:?}", console);
        }
    }

    #[test]
    fn test_controller_open_bus_bits_per_console() {
        for (console, expected) in [
            (Console::Nes, [0xE0, 0xE0]),
            (Console::TopLoader, [0xE4, 0xE0]),
            (Console::Famicom, [0xF8, 0xE0]),
        ] {
            let mut bus = test_bus();
            bus.set_console(console);
            bus.mem_write(0x4016, 0); // no buttons: data bit 0 reads 0
            for (port, addr) in [0x4016u16, 0x4017].into_iter().enumerate() {
                bus.internal_bus = 0xFF;
                assert_eq!(
                    bus.mem_read(addr),
                    expected[port],
                    "{:?} {:04X}",
                    console,
                    addr
                );
            }
        }
    }

    #[test]
    fn test_famicom_microphone() {
        let mut bus = test_bus();
        bus.joypads[1].microphone = true;
        bus.internal_bus = 0;
        assert_eq!(bus.mem_read(0x4016) & 0x04, 0, "only the Famicom has one");
        bus.set_console(Console::Famicom);
        assert_eq!(bus.mem_read(0x4016) & 0x04, 0x04);
        assert_eq!(bus.mem_read(0x4017) & 0x04, 0, "it is read on $4016");
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

    #[test]
    fn test_strobe_reaches_the_controllers_on_a_get_to_put_transition() {
        use crate::joypad::JoypadButton;
        // a read-modify-write instruction writes 1 then 0 on back-to-back
        // cycles: the controllers see the 1 only if it was written on a get
        // cycle (odd-numbered here), so the next cycle is a put cycle
        for strobed in [true, false] {
            let mut bus = test_bus();
            bus.joypads[0].set_button_pressed_status(JoypadButton::BUTTON_A, true);
            // read all 8 buttons, so a read returns 1 until strobed again
            for _ in 0..8 {
                bus.cpu_read(0x4016);
            }
            let get_next = bus.cycles % 2 == 0;
            if get_next != strobed {
                bus.idle_cycle();
            }
            bus.cpu_write(0x4016, 1);
            bus.cpu_write(0x4016, 0);
            bus.idle_cycle();
            bus.idle_cycle();
            // strobed: reads restart at A (pressed) then B (not pressed);
            // not strobed: still past the 8th button, which reads 1
            assert_eq!(bus.cpu_read(0x4016) & 1, 1);
            let b = bus.cpu_read(0x4016) & 1;
            assert_eq!(b, if strobed { 0 } else { 1 }, "strobed: {strobed}");
        }
    }

    /// Starts a 17-byte DMC sample and runs until its first fetch is due.
    fn bus_with_dmc_fetch_due<'a>() -> Bus<'a> {
        let mut bus = test_bus();
        bus.cpu_write(0x4013, 0x01);
        bus.cpu_write(0x4015, 0x10);
        while !bus.dmc_dma_running {
            bus.cpu_write(0x0000, 0x00);
        }
        bus
    }

    #[test]
    fn test_dmc_fetch_takes_three_or_four_cycles() {
        let mut bus = bus_with_dmc_fetch_due();
        let start = bus.cpu_cycles();
        bus.cpu_read(0x0000);
        let taken = bus.cpu_cycles() - start - 1;
        assert!(taken == 3 || taken == 4, "took {}", taken);
    }

    #[test]
    fn test_dmc_turned_off_before_the_halt_cancels_the_fetch() {
        let mut bus = bus_with_dmc_fetch_due();
        // writes can't be halted, so the DMC stops before the CPU does
        bus.cpu_write(0x4015, 0x00);
        for _ in 0..3 {
            bus.cpu_write(0x0000, 0x00);
        }
        let start = bus.cpu_cycles();
        bus.cpu_read(0x0000);
        assert_eq!(bus.cpu_cycles() - start, 1);
    }

    #[test]
    fn test_dmc_turned_off_as_the_fetch_starts_halts_for_one_cycle() {
        // with the DMC turned off 0-2 cycles before the CPU's read, the
        // fetch is either cut short after its halt cycle, or completes
        let mut taken = vec![];
        for writes in 0..3 {
            let mut bus = bus_with_dmc_fetch_due();
            bus.cpu_write(0x4015, 0x00);
            for _ in 0..writes {
                bus.cpu_write(0x0000, 0x00);
            }
            let start = bus.cpu_cycles();
            bus.cpu_read(0x0000);
            taken.push(bus.cpu_cycles() - start - 1);
        }
        assert!(taken.contains(&1), "{:?}", taken);
        assert!(taken.iter().all(|&t| t == 1 || t >= 3), "{:?}", taken);
    }

    #[test]
    fn test_dma_read_conflicts_with_a_controller_port() {
        use crate::joypad::JoypadButton;
        let mut bus = test_bus();
        bus.joypads[0].set_button_pressed_status(JoypadButton::BUTTON_A, true);
        bus.joypads[0].write(1);
        bus.joypads[0].write(0);
        // $0016 is RAM, and $4016 answers too while the CPU is at $4000:
        // the port's bit 0 (A held) wins, its floating bits come from RAM
        for (ram, expected) in [(0x00, 0x01), (0xFF, 0xE1)] {
            bus.mem_write(0x0016, ram);
            let mut prev = 0x4000;
            assert_eq!(bus.dma_read(0x0016, Some(0x4000), &mut prev), expected);
            bus.joypads[0].write(1); // A again for the next read
            bus.joypads[0].write(0);
        }
        // with the CPU elsewhere, only RAM answers
        let mut prev = 0;
        assert_eq!(bus.dma_read(0x0016, Some(0x0200), &mut prev), 0xFF);
    }
}

use crate::apu::APU;
use crate::cpu::Mem;
use crate::joypad::Joypad;
use crate::mapper::SharedMapper;
use crate::region::Region;
use crate::ppu::NesPPU;
use crate::ppu::PPU;

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

    stall_cycles: usize,
    oam_dma_pending: bool,

    // Last value driven on the CPU data bus. Reads from addresses nothing
    // responds to return it ("open bus") instead of panicking.
    open_bus: u8,

    cycles: usize,
    region: Region,
    ppu_clock_remainder: u32,
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
            stall_cycles: 0,
            oam_dma_pending: false,
            open_bus: 0,
            cycles: 0,
            region: Region::Ntsc,
            ppu_clock_remainder: 0,
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

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.ppu.set_region(region);
        self.apu.set_region(region);
        self.ppu_clock_remainder = 0;
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
            self.step_cycle();
        }

        if self.oam_dma_pending {
            self.oam_dma_pending = false;
            self.stall_cycles += 513 + self.cycles % 2;
        }

        while self.stall_cycles > 0 {
            self.stall_cycles -= 1;
            self.step_cycle();
        }
    }

    fn step_cycle(&mut self) {
        self.cycles += 1;
        self.apu.tick();

        if let Some(addr) = self.apu.dmc_fetch_addr() {
            let byte = self.cartridge_read(addr);
            self.apu.dmc_provide_byte(byte);
            self.stall_cycles += 4;
        }

        self.ppu_clock_remainder += self.region.cpu_divider();
        let dots = self.ppu_clock_remainder / self.region.cpu_divider();
        self.ppu_clock_remainder %= self.region.ppu_divider();
        let new_frame = self.ppu.tick(dots as u8);

        if new_frame {
            if let Some(callback) = self.audio_callback.as_mut() {
                let samples = self.apu.take_samples();
                let rate_adjust = callback(&samples);
                self.apu.set_rate_adjust(rate_adjust);
            }
            (self.gameloop_callback)(&self.ppu, &mut self.joypads);
        }
    }

    pub fn poll_nmi_status(&mut self) -> bool {
        self.ppu.poll_nmi_interrupt()
    }

    pub fn irq_pending(&self) -> bool {
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
        let value = match addr {
            0x0000..=0x1FFF => self.cpu_vram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => match addr & 0x2007 {
                0x2002 => (self.ppu.read_status() & 0xE0) | (self.open_bus & 0x1F),
                0x2004 => self.ppu.read_oam_data(),
                0x2007 => self.ppu.read_data(),
                _ => self.open_bus,
            },

            0x4015 => self.apu.read_status(),
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

            0x2000..=0x3FFF => match addr & 0x2007 {
                0x2000 => self.ppu.write_to_ctrl(data),
                0x2001 => self.ppu.write_to_mask(data),
                0x2002 => { /* PPUSTATUS is read-only */ }
                0x2003 => self.ppu.write_to_oam_addr(data),
                0x2004 => self.ppu.write_to_oam_data(data),
                0x2005 => self.ppu.write_to_scroll(data),
                0x2006 => self.ppu.write_to_ppu_addr(data),
                0x2007 => self.ppu.write_to_data(data),
                _ => unreachable!(),
            },

            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, data),

            0x4014 => {
                let mut buffer: [u8; 256] = [0; 256];
                let hi: u16 = (data as u16) << 8;
                for i in 0..256u16 {
                    buffer[i as usize] = self.mem_read(hi + i);
                }

                self.ppu.write_oam_dma(&buffer);

                self.oam_dma_pending = true;
            }

            0x4016 => {
                self.joypads[0].write(data);
                self.joypads[1].write(data);
            }

            0x4018..=0x401F => { /* CPU test mode registers */ }

            0x4020..=0xFFFF => self.mapper.borrow_mut().cpu_write(addr, data),
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
        assert_eq!(bus.mem_read(0x2000), 0xA5);
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
        let mut bus = Bus::new(test::test_mapper(), move |_ppu, _joypads| seen.set(seen.get() + 1));
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
                region, measured, expected
            );
        }
    }
}

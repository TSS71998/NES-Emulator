use crate::bus::Bus;
use crate::opcodes;

bitflags! {
    /// # Status Register (P) https://www.nesdev.org/wiki/Status_flags
    ///
    ///  7 6 5 4 3 2 1 0
    ///  N V _ B D I Z C
    ///  | |   | | | | +--- Carry Flag
    ///  | |   | | | +----- Zero Flag
    ///  | |   | | +------- Interrupt Disable
    ///  | |   | +--------- Decimal Mode (not used on NES)
    ///  | |   +----------- Break Command
    ///  | +--------------- Overflow Flag
    ///  +----------------- Negative Flag
    ///
    pub struct CpuFlags: u8 {
        const CARRY              = 0b00000001;
        const ZERO               = 0b00000010;
        const INTERRUPT_DISABLE  = 0b00000100;
        const DECIMAL_MODE       = 0b00001000;
        const BREAK              = 0b00010000;
        const BREAK2             = 0b00100000;
        const OVERFLOW           = 0b01000000;
        const NEGATIVE           = 0b10000000;
    }
}

const STACK: u16 = 0x0100;
const STACK_RESET: u8 = 0xfd;

pub struct CPU<'a> {
    pub register_a: u8,
    pub register_x: u8,
    pub register_y: u8,
    pub status: CpuFlags,
    pub program_counter: u16,
    pub stack_pointer: u8,
    pub bus: Bus<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum AddressingMode {
    Immediate,
    ZeroPage,
    ZeroPage_X,
    ZeroPage_Y,
    Absolute,
    Absolute_X,
    Absolute_Y,
    Indirect_X,
    Indirect_Y,
    NoneAddressing,
}

pub trait Mem {
    fn mem_read(&mut self, addr: u16) -> u8;

    fn mem_write(&mut self, addr: u16, data: u8);

    fn mem_read_u16(&mut self, pos: u16) -> u16 {
        let lo = self.mem_read(pos) as u16;
        let hi = self.mem_read(pos.wrapping_add(1)) as u16;
        (hi << 8) | lo
    }

    fn mem_write_u16(&mut self, pos: u16, data: u16) {
        let hi = (data >> 8) as u8;
        let lo = (data & 0xff) as u8;
        self.mem_write(pos, lo);
        self.mem_write(pos.wrapping_add(1), hi);
    }
}

impl Mem for CPU<'_> {
    fn mem_read(&mut self, addr: u16) -> u8 {
        self.bus.mem_read(addr)
    }

    fn mem_write(&mut self, addr: u16, data: u8) {
        self.bus.mem_write(addr, data)
    }
    fn mem_read_u16(&mut self, addr: u16) -> u16 {
        self.bus.mem_read_u16(addr)
    }

    fn mem_write_u16(&mut self, addr: u16, data: u16) {
        self.bus.mem_write_u16(addr, data)
    }
}
fn page_cross(addr1: u16, addr2: u16) -> bool {
    addr1 & 0xFF00 != addr2 & 0xFF00
}

mod interrupt {
    #[derive(PartialEq, Eq)]
    pub enum InterruptType {
        NMI,
        IRQ,
        BRK
    }

    #[derive(PartialEq, Eq)]
    pub(super) struct Interrupt {
        pub(super) itype: InterruptType,
        pub(super) vector_addr: u16,
        pub(super) b_flag_mask: u8,
        pub(super) cpu_cycles: u8,
    }

    pub(super) const NMI: Interrupt = Interrupt {
        itype: InterruptType::NMI,
        vector_addr: 0xfffA,
        b_flag_mask: 0b00100000,
        // 2 dummy reads, 3 stack pushes, 2 vector reads
        cpu_cycles: 7,
    };

    pub(super) const IRQ: Interrupt = Interrupt {
        itype: InterruptType::IRQ,
        vector_addr: 0xFFFE,
        b_flag_mask: 0b00100000,
        cpu_cycles: 7
    };
    // BRK pushes the B flag, which is how a handler tells it apart from IRQ.
    pub(super) const BRK: Interrupt = Interrupt {
        itype: InterruptType::BRK,
        vector_addr: 0xFFFE,
        b_flag_mask: 0b00110000,
        // the 7 cycles are already in the opcode table
        cpu_cycles: 0
    };
}

/// Resolves an operand address. `read` is the memory reader, so execution can
/// use real (side-effecting) reads and the tracer can use `Bus::peek`.
/// Returns (address, page_cross flag).
fn resolve_address(
    mode: &AddressingMode,
    addr: u16,
    x: u8,
    y: u8,
    mut read: impl FnMut(u16) -> u8
) -> (u16, bool) {

    fn pointer(read: &mut impl FnMut(u16) -> u8, lo: u16, hi: u16) -> u16 {
        let lo = read(lo) as u16;
        let hi = read(hi) as u16;
        hi << 8 | lo
    }

    match mode {
        AddressingMode::ZeroPage => (read(addr) as u16, false),
        AddressingMode::ZeroPage_X => (read(addr).wrapping_add(x) as u16, false),
        AddressingMode::ZeroPage_Y => (read(addr).wrapping_add(y) as u16, false),
        AddressingMode::Absolute => (pointer(&mut read, addr, addr.wrapping_add(1)), false),
        AddressingMode::Absolute_X => {
            let base = pointer(&mut read, addr, addr.wrapping_add(1));
            let addr = base.wrapping_add(x as u16);
            (addr, page_cross(base, addr))
        }
        AddressingMode::Absolute_Y => {
            let base = pointer(&mut read, addr, addr.wrapping_add(1));
            let addr = base.wrapping_add(y as u16);
            (addr, page_cross(base, addr))
        }
        AddressingMode::Indirect_X => {
            let ptr = read(addr).wrapping_add(x);
            (pointer(&mut read, ptr as u16, ptr.wrapping_add(1) as u16), false)
        }
        AddressingMode::Indirect_Y => {
            let base = read(addr);
            let deref_base = pointer(&mut read, base as u16, base.wrapping_add(1) as u16);
            let deref = deref_base.wrapping_add(y as u16);
            (deref, page_cross(deref, deref_base))
        }
        _ => {
            panic!("mode {:?} is not supported", mode);
        }
    }

}

impl<'a> CPU<'a> {
    pub fn new<'b>(bus: Bus<'b>) -> CPU<'b> {
        CPU {
            register_a: 0,
            register_x: 0,
            register_y: 0,
            stack_pointer: STACK_RESET,
            program_counter: 0x8000,
            status: CpuFlags::from_bits_truncate(0b100100),
            bus: bus,
        }
    }

    /// Operand address for `mode` with the operand bytes at `addr`, using
    /// real bus reads. Returns (address, page_cross flag).
    pub fn get_absolute_address(&mut self, mode: &AddressingMode, addr: u16) -> (u16, bool) {
        let (x, y) = (self.register_x, self.register_y);
        resolve_address(mode, addr, x, y, |a| self.bus.mem_read(a))
    }

    pub fn peek_absolute_address(&self, mode: &AddressingMode, addr: u16) -> u16 {
        resolve_address(mode, addr, self.register_x, self.register_y, |a| self.bus.peek(a)).0
    }

    fn get_operand_address(&mut self, mode: &AddressingMode) -> (u16, bool) {
        match mode {
            AddressingMode::Immediate => (self.program_counter, false),
            _ => self.get_absolute_address(mode, self.program_counter),
        }
    }

    fn ldy(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        self.register_y = data;
        self.update_zero_and_negative_flags(self.register_y);
        if page_cross {
            self.bus.tick(1);
        }
    }

    fn ldx(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        self.register_x = data;
        self.update_zero_and_negative_flags(self.register_x);
        if page_cross {
            self.bus.tick(1);
        }
    }

    fn lda(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(&mode);
        let value = self.mem_read(addr);
        self.set_register_a(value);
        if page_cross {
            self.bus.tick(1);
        }
    }

    fn sta(&mut self, mode: &AddressingMode) {
        let (addr, _) = self.get_operand_address(mode);
        self.mem_write(addr, self.register_a);
    }

    fn set_register_a(&mut self, value: u8) {
        self.register_a = value;
        self.update_zero_and_negative_flags(self.register_a);
    }

    fn and(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        self.set_register_a(data & self.register_a);

        if page_cross {
            self.bus.tick(1);
        }
    }

    fn eor(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        self.set_register_a(data ^ self.register_a);

        if page_cross {
            self.bus.tick(1);
        }
    }

    fn ora(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        self.set_register_a(data | self.register_a);
        if page_cross {
            self.bus.tick(1);
        }
    }

    fn tax(&mut self) {
        self.register_x = self.register_a;
        self.update_zero_and_negative_flags(self.register_x);
    }

    fn update_zero_and_negative_flags(&mut self, result: u8) {
        if result == 0 {
            self.status.insert(CpuFlags::ZERO);
        } else {
            self.status.remove(CpuFlags::ZERO);
        }

        if result >> 7 == 1 {
            self.status.insert(CpuFlags::NEGATIVE);
        } else {
            self.status.remove(CpuFlags::NEGATIVE);
        }
    }

    fn update_negative_flags(&mut self, result: u8) {
        if result >> 7 == 1 {
            self.status.insert(CpuFlags::NEGATIVE)
        } else {
            self.status.remove(CpuFlags::NEGATIVE)
        }
    }

    fn inx(&mut self) {
        self.register_x = self.register_x.wrapping_add(1);
        self.update_zero_and_negative_flags(self.register_x);
    }

    fn iny(&mut self) {
        self.register_y = self.register_y.wrapping_add(1);
        self.update_zero_and_negative_flags(self.register_y);
    }

    pub fn load(&mut self, program: Vec<u8>) {
        for i in 0..(program.len() as u16) {
            self.mem_write(0x0600 + i, program[i as usize]);
        }
        // self.mem_write_u16(0xFFFC, 0x8600);
    }

    pub fn reset(&mut self) {
        self.register_a = 0;
        self.register_x = 0;
        self.register_y = 0;
        self.stack_pointer = STACK_RESET;
        self.status = CpuFlags::from_bits_truncate(0b100100);
        // self.memory = [0; 0xFFFF];

        self.program_counter = self.mem_read_u16(0xFFFC);
    }

    fn set_carry_flag(&mut self) {
        self.status.insert(CpuFlags::CARRY)
    }

    fn clear_carry_flag(&mut self) {
        self.status.remove(CpuFlags::CARRY)
    }

    /// note: ignoring decimal mode
    /// http://www.righto.com/2012/12/the-6502-overflow-flag-explained.html
    fn add_to_register_a(&mut self, data: u8) {
        let sum = self.register_a as u16
            + data as u16
            + (if self.status.contains(CpuFlags::CARRY) {
                1
            } else {
                0
            }) as u16;

        let carry = sum > 0xff;

        if carry {
            self.status.insert(CpuFlags::CARRY);
        } else {
            self.status.remove(CpuFlags::CARRY);
        }

        let result = sum as u8;

        if (data ^ result) & (result ^ self.register_a) & 0x80 != 0 {
            self.status.insert(CpuFlags::OVERFLOW);
        } else {
            self.status.remove(CpuFlags::OVERFLOW)
        }

        self.set_register_a(result);
    }

    fn sub_from_register_a(&mut self, data: u8) {
        self.add_to_register_a(((data as i8).wrapping_neg().wrapping_sub(1)) as u8);
    }

    fn and_with_register_a(&mut self, data: u8) {
        self.set_register_a(data & self.register_a);
    }

    fn xor_with_register_a(&mut self, data: u8) {
        self.set_register_a(data ^ self.register_a);
    }

    fn or_with_register_a(&mut self, data: u8) {
        self.set_register_a(data | self.register_a);
    }

    fn sbc(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(&mode);
        let data = self.mem_read(addr);
        self.add_to_register_a(((data as i8).wrapping_neg().wrapping_sub(1)) as u8);

        if page_cross {
            self.bus.tick(1);
        }
    }

    fn adc(&mut self, mode: &AddressingMode) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let value = self.mem_read(addr);
        self.add_to_register_a(value);

        if page_cross {
            self.bus.tick(1);
        }
    }

    fn stack_pop(&mut self) -> u8 {
        self.stack_pointer = self.stack_pointer.wrapping_add(1);
        self.mem_read((STACK as u16) + self.stack_pointer as u16)
    }

    fn stack_push(&mut self, data: u8) {
        self.mem_write((STACK as u16) + self.stack_pointer as u16, data);
        self.stack_pointer = self.stack_pointer.wrapping_sub(1)
    }

    fn stack_push_u16(&mut self, data: u16) {
        let hi = (data >> 8) as u8;
        let lo = (data & 0xff) as u8;
        self.stack_push(hi);
        self.stack_push(lo);
    }

    fn stack_pop_u16(&mut self) -> u16 {
        let lo = self.stack_pop() as u16;
        let hi = self.stack_pop() as u16;

        hi << 8 | lo
    }

    fn asl_accumulator(&mut self) {
        let mut data = self.register_a;
        if data >> 7 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data << 1;
        self.set_register_a(data)
    }

    fn asl(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        if data >> 7 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data << 1;
        self.mem_write(addr, data);
        self.update_zero_and_negative_flags(data);
        data
    }

    fn lsr_accumulator(&mut self) {
        let mut data = self.register_a;
        if data & 1 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data >> 1;
        self.set_register_a(data)
    }

    fn lsr(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        if data & 1 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data >> 1;
        self.mem_write(addr, data);
        self.update_zero_and_negative_flags(data);
        data
    }

    fn rol(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        let old_carry = self.status.contains(CpuFlags::CARRY);

        if data >> 7 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data << 1;
        if old_carry {
            data = data | 1;
        }
        self.mem_write(addr, data);
        self.update_negative_flags(data);
        data
    }

    fn rol_accumulator(&mut self) {
        let mut data = self.register_a;
        let old_carry = self.status.contains(CpuFlags::CARRY);

        if data >> 7 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data << 1;
        if old_carry {
            data = data | 1;
        }
        self.set_register_a(data);
    }

    fn ror(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        let old_carry = self.status.contains(CpuFlags::CARRY);

        if data & 1 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data >> 1;
        if old_carry {
            data = data | 0b10000000;
        }
        self.mem_write(addr, data);
        self.update_negative_flags(data);
        data
    }

    fn ror_accumulator(&mut self) {
        let mut data = self.register_a;
        let old_carry = self.status.contains(CpuFlags::CARRY);

        if data & 1 == 1 {
            self.set_carry_flag();
        } else {
            self.clear_carry_flag();
        }
        data = data >> 1;
        if old_carry {
            data = data | 0b10000000;
        }
        self.set_register_a(data);
    }

    fn inc(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        data = data.wrapping_add(1);
        self.mem_write(addr, data);
        self.update_zero_and_negative_flags(data);
        data
    }

    fn dey(&mut self) {
        self.register_y = self.register_y.wrapping_sub(1);
        self.update_zero_and_negative_flags(self.register_y);
    }

    fn dex(&mut self) {
        self.register_x = self.register_x.wrapping_sub(1);
        self.update_zero_and_negative_flags(self.register_x);
    }

    fn dec(&mut self, mode: &AddressingMode) -> u8 {
        let (addr, _) = self.get_operand_address(mode);
        let mut data = self.mem_read(addr);
        data = data.wrapping_sub(1);
        self.mem_write(addr, data);
        self.update_zero_and_negative_flags(data);
        data
    }

    fn pla(&mut self) {
        let data = self.stack_pop();
        self.set_register_a(data);
    }

    fn plp(&mut self) {
        self.status.bits = self.stack_pop();
        self.status.remove(CpuFlags::BREAK);
        self.status.insert(CpuFlags::BREAK2);
    }

    fn php(&mut self) {
        //https://www.nesdev.org/wiki/Status_flags
        let mut flags = self.status.clone();
        flags.insert(CpuFlags::BREAK);
        flags.insert(CpuFlags::BREAK2);
        self.stack_push(flags.bits());
    }

    fn bit(&mut self, mode: &AddressingMode) {
        let (addr, _) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        let and = self.register_a & data;
        if and == 0 {
            self.status.insert(CpuFlags::ZERO);
        } else {
            self.status.remove(CpuFlags::ZERO);
        }

        self.status.set(CpuFlags::NEGATIVE, data & 0b10000000 > 0);
        self.status.set(CpuFlags::OVERFLOW, data & 0b01000000 > 0);
    }

    fn compare(&mut self, mode: &AddressingMode, compare_with: u8) {
        let (addr, page_cross) = self.get_operand_address(mode);
        let data = self.mem_read(addr);
        if data <= compare_with {
            self.status.insert(CpuFlags::CARRY);
        } else {
            self.status.remove(CpuFlags::CARRY);
        }

        self.update_zero_and_negative_flags(compare_with.wrapping_sub(data));

        if page_cross {
            self.bus.tick(1);
        }
    }

    fn branch(&mut self, condition: bool) {
        if condition {
            self.bus.tick(1);

            let jump: i8 = self.mem_read(self.program_counter) as i8;
            let jump_addr = self
                .program_counter
                .wrapping_add(1)
                .wrapping_add(jump as u16);

            if self.program_counter.wrapping_add(1) & 0xFF00 != jump_addr & 0xFF00 {
                self.bus.tick(1);
            }

            self.program_counter = jump_addr;
        }
    }

    fn interrupt(&mut self, interrupt: interrupt::Interrupt) {
        self.stack_push_u16(self.program_counter);
        let mut flag = self.status.clone();
        flag.set(CpuFlags::BREAK, interrupt.b_flag_mask & 0b010000 != 0);
        flag.set(CpuFlags::BREAK2, interrupt.b_flag_mask & 0b100000 != 0);

        self.stack_push(flag.bits);
        self.status.insert(CpuFlags::INTERRUPT_DISABLE);

        self.bus.tick(interrupt.cpu_cycles);
        self.program_counter = self.mem_read_u16(interrupt.vector_addr);
    }

    pub fn run(&mut self) {
        loop {
            self.step();
        }
    }

    pub fn run_with_callback<F>(&mut self, mut callback: F)
    where
        F: FnMut(&mut CPU),
    {
        loop {
            self.service_interrupts();
            callback(self);
            self.execute_instruction();
        }
    }
    
    pub fn step(&mut self) {
        self.service_interrupts();
        self.execute_instruction();
    }

    fn service_interrupts(&mut self) {
        if self.bus.poll_nmi_status() {
            self.interrupt(interrupt::NMI);
        } else if self.bus.irq_pending() && !self.status.contains(CpuFlags::INTERRUPT_DISABLE) {
            self.interrupt(interrupt::IRQ);
        }
    }

    pub fn execute_instruction(&mut self) {
        let code = self.mem_read(self.program_counter);
        self.program_counter = self.program_counter.wrapping_add(1);
        let program_counter_state = self.program_counter;

        let opcode = opcodes::lookup(code)
            .unwrap_or_else(|| panic!("OpCode {:x} is not recognized", code));

        match code {
            0xa9 | 0xa5 | 0xb5 | 0xad | 0xbd | 0xb9 | 0xa1 | 0xb1 => {
                self.lda(&opcode.mode);
            }

            0xAA => self.tax(),

            0xe8 => self.inx(),

            /* BRK */
            0x00 => {
                // BRK skips a padding byte, so the return address is BRK + 2
                self.program_counter = self.program_counter.wrapping_add(1);
                self.interrupt(interrupt::BRK);
            },
            /* CLD */ 
            0xd8 => self.status.remove(CpuFlags::DECIMAL_MODE),

            /* CLI */ 
            0x58 => self.status.remove(CpuFlags::INTERRUPT_DISABLE),

            /* CLV */ 
            0xb8 => self.status.remove(CpuFlags::OVERFLOW),

            /* CLC */ 
            0x18 => self.clear_carry_flag(),

            /* SEC */ 
            0x38 => self.set_carry_flag(),

            /* SEI */ 
            0x78 => self.status.insert(CpuFlags::INTERRUPT_DISABLE),

            /* SED */ 
            0xf8 => self.status.insert(CpuFlags::DECIMAL_MODE),

            /* PHA */ 
            0x48 => self.stack_push(self.register_a),

            /* PLA */
            0x68 => {
                self.pla();
            }

            /* PHP */
            0x08 => {
                self.php();
            }

            /* PLP */
            0x28 => {
                self.plp();
            }

            /* ADC */
            0x69 | 0x65 | 0x75 | 0x6d | 0x7d | 0x79 | 0x61 | 0x71 => {
                self.adc(&opcode.mode);
            }

            /* SBC */
            0xe9 | 0xe5 | 0xf5 | 0xed | 0xfd | 0xf9 | 0xe1 | 0xf1 => {
                self.sbc(&opcode.mode);
            }

            /* AND */
            0x29 | 0x25 | 0x35 | 0x2d | 0x3d | 0x39 | 0x21 | 0x31 => {
                self.and(&opcode.mode);
            }

            /* EOR */
            0x49 | 0x45 | 0x55 | 0x4d | 0x5d | 0x59 | 0x41 | 0x51 => {
                self.eor(&opcode.mode);
            }

            /* ORA */
            0x09 | 0x05 | 0x15 | 0x0d | 0x1d | 0x19 | 0x01 | 0x11 => {
                self.ora(&opcode.mode);
            }

            /* LSR */ 
            0x4a => self.lsr_accumulator(),

            /* LSR */
            0x46 | 0x56 | 0x4e | 0x5e => {
                self.lsr(&opcode.mode);
            }

            /*ASL*/ 
            0x0a => self.asl_accumulator(),

            /* ASL */
            0x06 | 0x16 | 0x0e | 0x1e => {
                self.asl(&opcode.mode);
            }

            /*ROL*/ 
            0x2a => self.rol_accumulator(),

            /* ROL */
            0x26 | 0x36 | 0x2e | 0x3e => {
                self.rol(&opcode.mode);
            }

            /* ROR */ 
            0x6a => self.ror_accumulator(),

            /* ROR */
            0x66 | 0x76 | 0x6e | 0x7e => {
                self.ror(&opcode.mode);
            }

            /* INC */
            0xe6 | 0xf6 | 0xee | 0xfe => {
                self.inc(&opcode.mode);
            }

            /* INY */
            0xc8 => self.iny(),

            /* DEC */
            0xc6 | 0xd6 | 0xce | 0xde => {
                self.dec(&opcode.mode);
            }

            /* DEX */
            0xca => {
                self.dex();
            }

            /* DEY */
            0x88 => {
                self.dey();
            }

            /* CMP */
            0xc9 | 0xc5 | 0xd5 | 0xcd | 0xdd | 0xd9 | 0xc1 | 0xd1 => {
                self.compare(&opcode.mode, self.register_a);
            }

            /* CPY */
            0xc0 | 0xc4 | 0xcc => {
                self.compare(&opcode.mode, self.register_y);
            }

            /* CPX */
            0xe0 | 0xe4 | 0xec => self.compare(&opcode.mode, self.register_x),

            /* JMP Absolute */
            0x4c => {
                let mem_address = self.mem_read_u16(self.program_counter);
                self.program_counter = mem_address;
            }

            /* JMP Indirect */
            0x6c => {
                let mem_address = self.mem_read_u16(self.program_counter);
                // let indirect_ref = self.mem_read_u16(mem_address);
                //6502 bug mode with with page boundary:
                //  if address $3000 contains $40, $30FF contains $80, and $3100 contains $50,
                // the result of JMP ($30FF) will be a transfer of control to $4080 rather than $5080 as you intended
                // i.e. the 6502 took the low byte of the address from $30FF and the high byte from $3000

                let indirect_ref = if mem_address & 0x00FF == 0x00FF {
                    let lo = self.mem_read(mem_address);
                    let hi = self.mem_read(mem_address & 0xFF00);
                    (hi as u16) << 8 | (lo as u16)
                } else {
                    self.mem_read_u16(mem_address)
                };

                self.program_counter = indirect_ref;
            }

            /* JSR */
            0x20 => {
                self.stack_push_u16(self.program_counter.wrapping_add(1));
                let target_address = self.mem_read_u16(self.program_counter);
                self.program_counter = target_address
            }

            /* RTS */
            0x60 => {
                self.program_counter = self.stack_pop_u16() + 1;
            }

            /* RTI */
            0x40 => {
                self.status.bits = self.stack_pop();
                self.status.remove(CpuFlags::BREAK);
                self.status.insert(CpuFlags::BREAK2);

                self.program_counter = self.stack_pop_u16();
            }

            /* BNE */
            0xd0 => {
                self.branch(!self.status.contains(CpuFlags::ZERO));
            }

            /* BVS */
            0x70 => {
                self.branch(self.status.contains(CpuFlags::OVERFLOW));
            }

            /* BVC */
            0x50 => {
                self.branch(!self.status.contains(CpuFlags::OVERFLOW));
            }

            /* BPL */
            0x10 => {
                self.branch(!self.status.contains(CpuFlags::NEGATIVE));
            }

            /* BMI */
            0x30 => {
                self.branch(self.status.contains(CpuFlags::NEGATIVE));
            }

            /* BEQ */
            0xf0 => {
                self.branch(self.status.contains(CpuFlags::ZERO));
            }

            /* BCS */
            0xb0 => {
                self.branch(self.status.contains(CpuFlags::CARRY));
            }

            /* BCC */
            0x90 => {
                self.branch(!self.status.contains(CpuFlags::CARRY));
            }

            /* BIT */
            0x24 | 0x2c => {
                self.bit(&opcode.mode);
            }

            /* STA */
            0x85 | 0x95 | 0x8d | 0x9d | 0x99 | 0x81 | 0x91 => {
                self.sta(&opcode.mode);
            }

            /* STX */
            0x86 | 0x96 | 0x8e => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                self.mem_write(addr, self.register_x);
            }

            /* STY */
            0x84 | 0x94 | 0x8c => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                self.mem_write(addr, self.register_y);
            }

            /* LDX */
            0xa2 | 0xa6 | 0xb6 | 0xae | 0xbe => {
                self.ldx(&opcode.mode);
            }

            /* LDY */
            0xa0 | 0xa4 | 0xb4 | 0xac | 0xbc => {
                self.ldy(&opcode.mode);
            }

            /* NOP */
            0xea => {
                //do nothing
            }

            /* TAY */
            0xa8 => {
                self.register_y = self.register_a;
                self.update_zero_and_negative_flags(self.register_y);
            }

            /* TSX */
            0xba => {
                self.register_x = self.stack_pointer;
                self.update_zero_and_negative_flags(self.register_x);
            }

            /* TXA */
            0x8a => {
                self.register_a = self.register_x;
                self.update_zero_and_negative_flags(self.register_a);
            }

            /* TXS */
            0x9a => {
                self.stack_pointer = self.register_x;
            }

            /* TYA */
            0x98 => {
                self.register_a = self.register_y;
                self.update_zero_and_negative_flags(self.register_a);
            }

            /* unofficial */

            /* DCP */
            0xc7 | 0xd7 | 0xCF | 0xdF | 0xdb | 0xd3 | 0xc3 => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let mut data = self.mem_read(addr);
                data = data.wrapping_sub(1);
                self.mem_write(addr, data);
                // self._update_zero_and_negative_flags(data);
                if data <= self.register_a {
                    self.status.insert(CpuFlags::CARRY);
                }

                self.update_zero_and_negative_flags(self.register_a.wrapping_sub(data));
            }

            /* RLA */
            0x27 | 0x37 | 0x2F | 0x3F | 0x3b | 0x33 | 0x23 => {
                let data = self.rol(&opcode.mode);
                self.and_with_register_a(data);
            }

            /* SLO */ //todo tests
            0x07 | 0x17 | 0x0F | 0x1f | 0x1b | 0x03 | 0x13 => {
                let data = self.asl(&opcode.mode);
                self.or_with_register_a(data);
            }

            /* SRE */ //todo tests
            0x47 | 0x57 | 0x4F | 0x5f | 0x5b | 0x43 | 0x53 => {
                let data = self.lsr(&opcode.mode);
                self.xor_with_register_a(data);
            }

            /* SKB */
            0x80 | 0x82 | 0x89 | 0xc2 | 0xe2 => {
                /* 2 byte NOP (immediate ) */
                // todo: might be worth doing the read
            }

            /* AXS */
            0xCB => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                let x_and_a = self.register_x & self.register_a;
                let result = x_and_a.wrapping_sub(data);

                if data <= x_and_a {
                    self.status.insert(CpuFlags::CARRY);
                }
                self.update_zero_and_negative_flags(result);

                self.register_x = result;
            }

            /* ARR */
            0x6B => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.and_with_register_a(data);
                self.ror_accumulator();
                //todo: registers
                let result = self.register_a;
                let bit_5 = (result >> 5) & 1;
                let bit_6 = (result >> 6) & 1;

                if bit_6 == 1 {
                    self.status.insert(CpuFlags::CARRY)
                } else {
                    self.status.remove(CpuFlags::CARRY)
                }

                if bit_5 ^ bit_6 == 1 {
                    self.status.insert(CpuFlags::OVERFLOW);
                } else {
                    self.status.remove(CpuFlags::OVERFLOW);
                }

                self.update_zero_and_negative_flags(result);
            }

            /* unofficial SBC */
            0xeb => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.sub_from_register_a(data);
            }

            /* ANC */
            0x0b | 0x2b => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.and_with_register_a(data);
                if self.status.contains(CpuFlags::NEGATIVE) {
                    self.status.insert(CpuFlags::CARRY);
                } else {
                    self.status.remove(CpuFlags::CARRY);
                }
            }

            /* ALR */
            0x4b => {
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.and_with_register_a(data);
                self.lsr_accumulator();
            }

            //todo: test for everything below

            /* NOP read */
            0x04 | 0x44 | 0x64 | 0x14 | 0x34 | 0x54 | 0x74 | 0xd4 | 0xf4 | 0x0c | 0x1c
            | 0x3c | 0x5c | 0x7c | 0xdc | 0xfc => {
                let (addr, page_cross) = self.get_operand_address(&opcode.mode);
                let _data = self.mem_read(addr);
                if page_cross {
                    self.bus.tick(1);
                }
                /* do nothing */
            }

            /* RRA */
            0x67 | 0x77 | 0x6f | 0x7f | 0x7b | 0x63 | 0x73 => {
                let data = self.ror(&opcode.mode);
                self.add_to_register_a(data);
            }

            /* ISB */
            0xe7 | 0xf7 | 0xef | 0xff | 0xfb | 0xe3 | 0xf3 => {
                let data = self.inc(&opcode.mode);
                self.sub_from_register_a(data);
            }

            /* NOPs */
            0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xb2 | 0xd2
            | 0xf2 => { /* do nothing */ }

            0x1a | 0x3a | 0x5a | 0x7a | 0xda | 0xfa => { /* do nothing */ }

            /* LAX */
            0xa7 | 0xb7 | 0xaf | 0xbf | 0xa3 | 0xb3 => {
                let (addr, page_cross) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.set_register_a(data);
                self.register_x = self.register_a;
                if page_cross {
                    self.bus.tick(1);
                }
            }

            /* SAX */
            0x87 | 0x97 | 0x8f | 0x83 => {
                let data = self.register_a & self.register_x;
                let (addr, _) = self.get_operand_address(&opcode.mode);
                self.mem_write(addr, data);
            }

            /* LXA */
            0xab => {
                self.lda(&opcode.mode);
                self.tax();
            }

            /* XAA */
            0x8b => {
                self.register_a = self.register_x;
                self.update_zero_and_negative_flags(self.register_a);
                let (addr, _) = self.get_operand_address(&opcode.mode);
                let data = self.mem_read(addr);
                self.and_with_register_a(data);
            }

            /* LAS */
            0xbb => {
                let (addr, page_cross) = self.get_operand_address(&opcode.mode);
                let mut data = self.mem_read(addr);
                data = data & self.stack_pointer;
                self.register_a = data;
                self.register_x = data;
                self.stack_pointer = data;
                self.update_zero_and_negative_flags(data);
                if page_cross {
                    self.bus.tick(1);
                }
            }

            /* TAS */
            0x9b => {
                let data = self.register_a & self.register_x;
                self.stack_pointer = data;
                let mem_address =
                    self.mem_read_u16(self.program_counter).wrapping_add(self.register_y as u16);

                let data = ((mem_address >> 8) as u8).wrapping_add(1) & self.stack_pointer;
                self.mem_write(mem_address, data)
            }

            /* AHX  Indirect Y */
            0x93 => {
                let (mem_address, _) = self.get_operand_address(&AddressingMode::Indirect_Y);
                let data = self.register_a & self.register_x & (mem_address >> 8) as u8;
                self.mem_write(mem_address, data)
            }

            /* AHX Absolute Y*/
            0x9f => {
                let mem_address = self.mem_read_u16(self.program_counter).wrapping_add(self.register_y as u16);
                let data = self.register_a & self.register_x & (mem_address >> 8) as u8;
                self.mem_write(mem_address, data)
            }

            /* SHX */
            0x9e => {
                let mem_address = self.mem_read_u16(self.program_counter).wrapping_add(self.register_y as u16);
                let data = self.register_x & ((mem_address >> 8) as u8).wrapping_add(1);
                self.mem_write(mem_address, data)
            }

            /* SHY */
            0x9c => {
                let mem_address = self.mem_read_u16(self.program_counter).wrapping_add(self.register_x as u16);
                let data = self.register_y & ((mem_address >> 8) as u8).wrapping_add(1);
                self.mem_write(mem_address, data)
            }
        }

        self.bus.tick(opcode.cycles);

        if program_counter_state == self.program_counter {
            self.program_counter = self.program_counter.wrapping_add((opcode.len - 1) as u16);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cartridge::test;

    fn cpu_with_program<'a>(program: Vec<u8>) -> CPU<'a> {
        CPU::new(Bus::new(test::test_mapper_containing(program), |_ppu, _joypads| {}))
    }

    fn run_steps(cpu: &mut CPU, n: usize) {
        for _ in 0..n {
            cpu.step();
        }
    }

    #[test]
    fn test_0xa9_lda_immediate_load_data() {
        let mut cpu = cpu_with_program(vec![0xa9, 0x05]);
        cpu.step();

        assert_eq!(cpu.register_a, 5);
        assert!(cpu.status.bits() & 0b0000_0010 == 0b00);
        assert!(cpu.status.bits() & 0b1000_0000 == 0);
    }

    #[test]
    fn test_0xaa_tax_move_a_to_x() {
        let mut cpu = cpu_with_program(vec![0xaa]);
        cpu.register_a = 10;
        cpu.step();
        
        assert_eq!(cpu.register_x, 10)
    }

    #[test]
    fn test_5_ops_working_together() {
        let mut cpu = cpu_with_program(vec![0xa9, 0xc0, 0xaa, 0xe8]);
        run_steps(&mut cpu, 3);

        assert_eq!(cpu.register_x, 0xc1)
    }

    #[test]
    fn test_inx_overflow() {
        let mut cpu = cpu_with_program(vec![0xe8, 0xe8]);
        cpu.register_x = 0xff;
        run_steps(&mut cpu, 2);

        assert_eq!(cpu.register_x, 1)
    }

    #[test]
    fn test_lda_from_memory() {
        let mut cpu = cpu_with_program(vec![0xa5, 0x10]);
        cpu.mem_write(0x10, 0x55);
        cpu.step();

        assert_eq!(cpu.register_a, 0x55);
    }

    #[test]
    fn test_brk_jumps_through_irq_vector() {
        let mut program = vec![0x00, 0xea]; // BRK at $8000
        program.resize(0x8000, 0);
        program[0x7ffe] = 0x34; // IRQ/BRK vector at $FFFE = $1234
        program[0x7fff] = 0x12;
        let mut cpu = cpu_with_program(program);
        cpu.status = CpuFlags::from_bits_truncate(0b0010_0000);

        cpu.step();

        assert_eq!(cpu.program_counter, 0x1234);
        assert_eq!(cpu.stack_pointer, STACK_RESET - 3);
        assert!(cpu.status.contains(CpuFlags::INTERRUPT_DISABLE));
        // return address is BRK + 2, and the pushed flags have B set
        assert_eq!(cpu.mem_read(0x01fd), 0x80);
        assert_eq!(cpu.mem_read(0x01fc), 0x02);
        assert_eq!(cpu.mem_read(0x01fb), 0b0011_0000);
    }

    #[test]
    fn test_rti_returns_after_brk_padding_byte() {
        let mut program = vec![0x00, 0xff, 0xe8]; // BRK, padding, INX
        program.resize(0x8000, 0);
        program[0x0100] = 0x40; // RTI at $8100
        program[0x7ffe] = 0x00;
        program[0x7fff] = 0x81;
        let mut cpu = cpu_with_program(program);

        run_steps(&mut cpu, 3); // BRK, RTI, INX

        assert_eq!(cpu.register_x, 1);
        assert_eq!(cpu.program_counter, 0x8003);
    }

    #[test]
    fn test_mem_read_u16_wraps_at_top_of_memory() {
        let mut program = vec![];
        program.resize(0x8000, 0);
        program[0x7fff] = 0xAB; // $FFFF
        let mut cpu = cpu_with_program(program);
        cpu.mem_write(0x0000, 0xCD);

        assert_eq!(cpu.mem_read_u16(0xFFFF), 0xCDAB);
    }

    /// Enables rendering, then spins at $8006. The IRQ handler at $9000
    /// counts interrupts in $10 and acknowledges with a write to $E000.
    fn irq_test_cpu<'a>(
        irq_every: usize,
        disable_interrupts: bool,
    ) -> (CPU<'a>, std::rc::Rc<std::cell::RefCell<crate::mapper::test_util::ScanlineCounter>>) {
        use crate::mapper::test_util::ScanlineCounter;
        let mut prg = vec![0; 0x8000];
        let main = [
            0xA9, 0x08, // LDA #$08
            0x8D, 0x01, 0x20, // STA $2001 (show background)
            if disable_interrupts { 0x78 } else { 0x58 }, // SEI / CLI
            0x4C, 0x06, 0x80, // JMP $8006
        ];
        prg[..main.len()].copy_from_slice(&main);
        let handler = [
            0xE6, 0x10, // INC $10
            0x8D, 0x00, 0xE0, // STA $E000 (acknowledge)
            0x40, // RTI
        ];
        prg[0x1000..0x1000 + handler.len()].copy_from_slice(&handler);
        prg[0x7FFE] = 0x00; // IRQ vector -> $9000
        prg[0x7FFF] = 0x90;

        let cart = std::rc::Rc::new(std::cell::RefCell::new(ScanlineCounter::new(prg, Some(irq_every))));
        let cpu = CPU::new(Bus::new(cart.clone(), |_ppu, _joypads| {}));
        (cpu, cart)
    }

    #[test]
    fn test_mapper_irq_is_taken_and_acknowledged() {
        let (mut cpu, cart) = irq_test_cpu(20, false);
        run_steps(&mut cpu, 3000);

        let scanlines = cart.borrow().scanlines;
        assert!(scanlines >= 60, "only {} scanlines ran", scanlines);
        // one handler run per IRQ: the handler acknowledges, so IRQs don't repeat
        assert_eq!(cpu.mem_read(0x10) as usize, scanlines / 20);
        // RTI restored the main loop with interrupts enabled
        assert!(!cpu.status.contains(CpuFlags::INTERRUPT_DISABLE));
        assert!((0x8006..=0x8008).contains(&cpu.program_counter));
    }

    #[test]
    fn test_mapper_irq_is_masked_by_i_flag() {
        let (mut cpu, cart) = irq_test_cpu(20, true);
        run_steps(&mut cpu, 3000);

        assert!(cart.borrow().irq, "the cartridge should be holding IRQ low");
        assert_eq!(cpu.mem_read(0x10), 0);
    }
}

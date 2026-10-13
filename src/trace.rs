use crate::cpu::AddressingMode;
use crate::cpu::CPU;
use crate::opcodes;

/// Formats the instruction at the program counter in nestest log style.
/// Uses `Bus::peek`, so tracing doesn't change emulator state (reading
/// $2002 or $4016 through the normal path would).
pub fn trace(cpu: &CPU) -> String {
    let bus = &cpu.bus;
    let code = bus.peek(cpu.program_counter);
    let ops = opcodes::lookup(code).unwrap();

    let begin = cpu.program_counter;
    let mut hex_dump = vec![];
    hex_dump.push(code);

    let (mem_addr, stored_value) = match ops.mode {
        AddressingMode::Immediate | AddressingMode::NoneAddressing => (0, 0),
        _ => {
            let addr = cpu.peek_absolute_address(&ops.mode, begin.wrapping_add(1));
            (addr, bus.peek(addr))
        }
    };

    let tmp = match ops.len {
        1 => match ops.code {
            0x0A | 0x4A | 0x2A | 0x6A => format!("A "),
            _ => String::from(""),
        },
        2 => {
            let address: u8 = bus.peek(begin.wrapping_add(1));

            hex_dump.push(address);

            match ops.mode {
                AddressingMode::Immediate => format!("#${:02x}", address),
                AddressingMode::ZeroPage => format!("${:02x} = {:02x}", mem_addr, stored_value),
                AddressingMode::ZeroPage_X => format!(
                    "${:02x},X @ {:02x} = {:02x}",
                    address, mem_addr, stored_value
                ),
                AddressingMode::ZeroPage_Y => format!(
                    "${:02x},Y @ {:02x} = {:02x}",
                    address, mem_addr, stored_value
                ),
                AddressingMode::Indirect_X => format!(
                    "(${:02x},X) @ {:02x} = {:04x} = {:02x}",
                    address,
                    (address.wrapping_add(cpu.register_x)),
                    mem_addr,
                    stored_value
                ),
                AddressingMode::Indirect_Y => format!(
                    "(${:02x}),Y = {:04x} @ {:04x} = {:02x}",
                    address,
                    (mem_addr.wrapping_sub(cpu.register_y as u16)),
                    mem_addr,
                    stored_value
                ),
                AddressingMode::NoneAddressing => {
                    // assuming local jumps: BNE, BVS, etc....
                    let address: usize =
                        (begin as usize + 2).wrapping_add((address as i8) as usize);
                    format!("${:04x}", address)
                }

                _ => panic!(
                    "unexpected addressing mode {:?} has ops-len 2. code {:02x}",
                    ops.mode, ops.code
                ),
            }
        }
        3 => {
            let address_lo = bus.peek(begin.wrapping_add(1));
            let address_hi = bus.peek(begin.wrapping_add(2));
            hex_dump.push(address_lo);
            hex_dump.push(address_hi);

            let address = bus.peek_u16(begin.wrapping_add(1));

            match ops.mode {
                AddressingMode::NoneAddressing => {
                    if ops.code == 0x6C {
                        let jmp_addr = if address & 0x00FF == 0x00FF {
                            let lo = bus.peek(address);
                            let hi = bus.peek(address & 0xFF00);
                            (hi as u16) << 8 | (lo as u16)
                        } else {
                            bus.peek_u16(address)
                        };

                        format!("(${:04x}) = {:04x}", address, jmp_addr)
                    } else {
                        format!("${:04x}", address)
                    }
                }
                AddressingMode::Absolute => format!("${:04x} = {:02x}", mem_addr, stored_value),
                AddressingMode::Absolute_X => format!(
                    "${:04x},X @ {:04x} = {:02x}",
                    address, mem_addr, stored_value
                ),
                AddressingMode::Absolute_Y => format!(
                    "${:04x},Y @ {:04x} = {:02x}",
                    address, mem_addr, stored_value
                ),
                _ => panic!(
                    "unexpected addressing mode {:?} has ops-len 3. code {:02x}",
                    ops.mode, ops.code
                ),
            }
        }
        _ => String::from(""),
    };
    let hex_str = hex_dump
        .iter()
        .map(|z| format!("{:02x}", z))
        .collect::<Vec<String>>()
        .join(" ");
    let asm_str = format!("{:04x}  {:8} {: >4} {}", begin, hex_str, ops.mnemonic, tmp)
        .trim()
        .to_string();

    format!(
        "{:47} A:{:02x} X:{:02x} Y:{:02x} P:{:02x} SP:{:02x}",
        asm_str, cpu.register_a, cpu.register_x, cpu.register_y, cpu.status, cpu.stack_pointer,
    )
    .to_ascii_uppercase()
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::bus::Bus;
    use crate::cartridge::test::test_mapper;
    use crate::cpu::Mem;

    #[test]
    fn test_format_trace() {
        let mut bus = Bus::new(test_mapper(), |_ppu, _joypads| {});
        bus.mem_write(100, 0xa2);
        bus.mem_write(101, 0x01);
        bus.mem_write(102, 0xca);
        bus.mem_write(103, 0x88);

        let mut cpu = CPU::new(bus);
        cpu.program_counter = 0x64;
        cpu.register_a = 1;
        cpu.register_x = 2;
        cpu.register_y = 3;
        let mut result: Vec<String> = vec![];

        for _ in 0..3 {
            result.push(trace(&cpu));
            cpu.step();
        }
        assert_eq!(
            "0064  A2 01     LDX #$01                        A:01 X:02 Y:03 P:24 SP:FD",
            result[0]
        );
        assert_eq!(
            "0066  CA        DEX                             A:01 X:01 Y:03 P:24 SP:FD",
            result[1]
        );
        assert_eq!(
            "0067  88        DEY                             A:01 X:00 Y:03 P:26 SP:FD",
            result[2]
        );
    }

    #[test]
    fn test_format_mem_access() {
        let mut bus = Bus::new(test_mapper(), |_ppu, _joypads| {});
        bus.mem_write(100, 0x11);
        bus.mem_write(101, 0x33);

        bus.mem_write(0x33, 00);
        bus.mem_write(0x34, 04);

        bus.mem_write(0x400, 0xAA);

        let mut cpu = CPU::new(bus);
        cpu.program_counter = 0x64;
        cpu.register_y = 0;
        
        assert_eq!(
            "0064  11 33     ORA ($33),Y = 0400 @ 0400 = AA  A:00 X:00 Y:00 P:24 SP:FD",
            trace(&cpu)
        );
    }

    #[test]
    fn test_trace_of_ppu_store_does_not_panic_or_touch_ppu() {
        let mut bus = Bus::new(test_mapper(), |_ppu, _joypads| {});
        // STA $2002 used to read $2002 for the "= xx" part of the trace
        bus.mem_write(100, 0x8d);
        bus.mem_write(101, 0x02);
        bus.mem_write(102, 0x20);
        let mut cpu = CPU::new(bus);
        cpu.program_counter = 0x64;
        assert!(trace(&cpu).starts_with("0064  8D 02 20  STA $2002"));
    }
}

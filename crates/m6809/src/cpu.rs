//! The core. One instruction per `step`, one bus call per machine cycle.

use crate::bus::Bus;
use crate::{cc, Registers};

#[derive(Clone, Debug, Default)]
pub struct Cpu {
    pub regs: Registers,
    /// Level of the IRQ input. Sampled between instructions, never mid-one.
    pub irq: bool,
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a pending interrupt if one is asserted and not masked.
    ///
    /// Returns true if a handler was entered, in which case no instruction
    /// runs this step. IRQ is level-sensitive and sampled between
    /// instructions, so a device holding the line low is served repeatedly
    /// until it stops — which is what lets the VIA drive the frame.
    ///
    /// Not exercised by the verification corpus, which holds the interrupt
    /// lines clear throughout: this is datasheet behaviour, and the tests
    /// below are the only thing standing behind it.
    fn take_interrupt(&mut self, bus: &mut impl Bus) -> bool {
        if !self.irq || self.regs.flag(cc::I) {
            return false;
        }
        self.dead(bus);
        self.vma(bus);
        self.regs.set_flag(cc::E, true);
        self.push_state(bus);
        self.vma(bus);
        self.regs.set_flag(cc::I, true);
        let target = self.read16(bus, 0xFFF8);
        self.vma(bus);
        self.regs.pc = target;
        true
    }

    /// Execute one instruction, driving `bus` once per machine cycle.
    ///
    /// Panics on an opcode that is not implemented yet — a core that quietly
    /// did nothing would pass a test that never noticed it.
    pub fn step(&mut self, bus: &mut impl Bus) {
        if self.take_interrupt(bus) {
            return;
        }
        let opcode = self.fetch(bus);
        match opcode {
            // NOP: fetch, then one dead cycle.
            0x12 | 0x1B => self.dead(bus),

            // Loads and stores. The addressing mode decides the cycles; the
            // operation only decides which register and how wide.
            0x86 => { let v = self.imm8(bus); self.load8(v, |r, v| r.a = v) }
            0x96 => { let a = self.direct(bus); let v = bus.read(a); self.load8(v, |r, v| r.a = v) }
            0xB6 => { let a = self.extended(bus); let v = bus.read(a); self.load8(v, |r, v| r.a = v) }
            0xC6 => { let v = self.imm8(bus); self.load8(v, |r, v| r.b = v) }
            0xD6 => { let a = self.direct(bus); let v = bus.read(a); self.load8(v, |r, v| r.b = v) }
            0xF6 => { let a = self.extended(bus); let v = bus.read(a); self.load8(v, |r, v| r.b = v) }

            0x97 => { let a = self.direct(bus); self.store8(bus, a, self.regs.a) }
            0xB7 => { let a = self.extended(bus); self.store8(bus, a, self.regs.a) }
            0xD7 => { let a = self.direct(bus); self.store8(bus, a, self.regs.b) }
            0xF7 => { let a = self.extended(bus); self.store8(bus, a, self.regs.b) }

            0xCC => { let v = self.imm16(bus); self.load16(v, |r, v| r.set_d(v)) }
            0xDC => { let a = self.direct(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.set_d(v)) }
            0xFC => { let a = self.extended(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.set_d(v)) }
            0x8E => { let v = self.imm16(bus); self.load16(v, |r, v| r.x = v) }
            0x9E => { let a = self.direct(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.x = v) }
            0xBE => { let a = self.extended(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.x = v) }
            0xCE => { let v = self.imm16(bus); self.load16(v, |r, v| r.u = v) }
            0xDE => { let a = self.direct(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.u = v) }
            0xFE => { let a = self.extended(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.u = v) }

            0xDD => { let a = self.direct(bus); self.store16(bus, a, self.regs.d()) }
            0xFD => { let a = self.extended(bus); self.store16(bus, a, self.regs.d()) }
            0x9F => { let a = self.direct(bus); self.store16(bus, a, self.regs.x) }
            0xBF => { let a = self.extended(bus); self.store16(bus, a, self.regs.x) }
            0xDF => { let a = self.direct(bus); self.store16(bus, a, self.regs.u) }
            0xFF => { let a = self.extended(bus); self.store16(bus, a, self.regs.u) }

            // 8-bit ALU. The addressing modes are shared with the loads
            // above; only the operation and its flags differ.
            0x8B | 0x9B | 0xBB => { let v = self.operand8(bus, opcode); let r = self.add8(self.regs.a, v, false); self.regs.a = r }
            0xCB | 0xDB | 0xFB => { let v = self.operand8(bus, opcode); let r = self.add8(self.regs.b, v, false); self.regs.b = r }
            0x89 | 0x99 | 0xB9 => { let v = self.operand8(bus, opcode); let c = self.regs.flag(cc::C); let r = self.add8(self.regs.a, v, c); self.regs.a = r }
            0xC9 | 0xD9 | 0xF9 => { let v = self.operand8(bus, opcode); let c = self.regs.flag(cc::C); let r = self.add8(self.regs.b, v, c); self.regs.b = r }
            0x80 | 0x90 | 0xB0 => { let v = self.operand8(bus, opcode); let r = self.sub8(self.regs.a, v, false); self.regs.a = r }
            0xC0 | 0xD0 | 0xF0 => { let v = self.operand8(bus, opcode); let r = self.sub8(self.regs.b, v, false); self.regs.b = r }
            0x82 | 0x92 | 0xB2 => { let v = self.operand8(bus, opcode); let c = self.regs.flag(cc::C); let r = self.sub8(self.regs.a, v, c); self.regs.a = r }
            0xC2 | 0xD2 | 0xF2 => { let v = self.operand8(bus, opcode); let c = self.regs.flag(cc::C); let r = self.sub8(self.regs.b, v, c); self.regs.b = r }
            // Compare is a subtract that keeps only the flags.
            0x81 | 0x91 | 0xB1 => { let v = self.operand8(bus, opcode); self.sub8(self.regs.a, v, false); }
            0xC1 | 0xD1 | 0xF1 => { let v = self.operand8(bus, opcode); self.sub8(self.regs.b, v, false); }

            0x84 | 0x94 | 0xB4 => { let v = self.operand8(bus, opcode); let r = self.regs.a & v; self.logic8(r); self.regs.a = r }
            0xC4 | 0xD4 | 0xF4 => { let v = self.operand8(bus, opcode); let r = self.regs.b & v; self.logic8(r); self.regs.b = r }
            0x8A | 0x9A | 0xBA => { let v = self.operand8(bus, opcode); let r = self.regs.a | v; self.logic8(r); self.regs.a = r }
            0xCA | 0xDA | 0xFA => { let v = self.operand8(bus, opcode); let r = self.regs.b | v; self.logic8(r); self.regs.b = r }
            0x88 | 0x98 | 0xB8 => { let v = self.operand8(bus, opcode); let r = self.regs.a ^ v; self.logic8(r); self.regs.a = r }
            0xC8 | 0xD8 | 0xF8 => { let v = self.operand8(bus, opcode); let r = self.regs.b ^ v; self.logic8(r); self.regs.b = r }
            // BIT is AND keeping only the flags.
            0x85 | 0x95 | 0xB5 => { let v = self.operand8(bus, opcode); let r = self.regs.a & v; self.logic8(r) }
            0xC5 | 0xD5 | 0xF5 => { let v = self.operand8(bus, opcode); let r = self.regs.b & v; self.logic8(r) }

            // Mask writes straight into CC, then one internal cycle.
            0x1A => { let m = self.fetch(bus); self.regs.cc |= m; self.dead(bus) }
            0x1C => { let m = self.fetch(bus); self.regs.cc &= m; self.dead(bus) }

            // 16-bit arithmetic. Unlike the 16-bit loads these end with an
            // internal cycle, and none of them touches H.
            0xC3 | 0xD3 | 0xF3 => { let v = self.operand16(bus, opcode); let r = self.add16(self.regs.d(), v); self.regs.set_d(r); self.dead(bus) }
            0x83 | 0x93 | 0xB3 => { let v = self.operand16(bus, opcode); let r = self.sub16(self.regs.d(), v); self.regs.set_d(r); self.dead(bus) }
            0x8C | 0x9C | 0xBC => { let v = self.operand16(bus, opcode); self.sub16(self.regs.x, v); self.dead(bus) }

            // JMP: the address is formed and then simply taken.
            0x0E => { let a = self.direct(bus); self.regs.pc = a }
            0x7E => { let a = self.extended(bus); self.regs.pc = a }

            // Read-modify-write on memory, direct and extended.
            0x00 | 0x01 | 0x02 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 | 0x08 | 0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x0F => {
                let address = self.direct(bus);
                self.rmw(bus, address, opcode & 0x0F)
            }
            0x70 | 0x71 | 0x72 | 0x73 | 0x74 | 0x75 | 0x76 | 0x77 | 0x78 | 0x79 | 0x7A | 0x7B | 0x7C | 0x7D | 0x7F => {
                let address = self.extended(bus);
                self.rmw(bus, address, opcode & 0x0F)
            }

            // The same operations on an accumulator instead of memory: fetch,
            // transform, one internal cycle. TST stores nothing either way.
            0x40..=0x4F => {
                let result = self.transform(opcode & 0x0F, self.regs.a);
                if let Some(r) = result {
                    self.regs.a = r;
                }
                self.settle(bus, result.is_some())
            }
            0x50..=0x5F => {
                let result = self.transform(opcode & 0x0F, self.regs.b);
                if let Some(r) = result {
                    self.regs.b = r;
                }
                self.settle(bus, result.is_some())
            }

            // MUL: A×B into D. The long multiply shows up as nine VMA cycles,
            // which is what makes it the slowest instruction on the part.
            0x3D => {
                let product = u16::from(self.regs.a) * u16::from(self.regs.b);
                self.regs.set_d(product);
                self.regs.set_flag(cc::Z, product == 0);
                // C comes from bit 7 of the low half — it is there so a
                // following ADC rounds the result rather than truncating it.
                self.regs.set_flag(cc::C, product & 0x80 != 0);
                self.dead(bus);
                for _ in 0..9 {
                    self.vma(bus);
                }
            }
            // SEX: B's sign fills A, and the flags describe the whole of D.
            0x1D => {
                self.regs.a = if self.regs.b & 0x80 != 0 { 0xFF } else { 0x00 };
                let d = self.regs.d();
                self.regs.set_nz16(d);
                self.dead(bus)
            }
            0x19 => { self.daa(); self.dead(bus) }
            // ABX: B is unsigned here, unlike every other offset on the part.
            0x3A => {
                self.regs.x = self.regs.x.wrapping_add(u16::from(self.regs.b));
                self.dead(bus);
                self.vma(bus)
            }

            // LEA: compute an indexed address and keep it. Only the pointer
            // registers set Z; loading a stack pointer must not disturb the
            // flags a following branch is about to read.
            0x30 => { let a = self.indexed(bus); self.vma(bus); self.regs.x = a; let z = a == 0; self.regs.set_flag(cc::Z, z) }
            0x31 => { let a = self.indexed(bus); self.vma(bus); self.regs.y = a; let z = a == 0; self.regs.set_flag(cc::Z, z) }
            0x32 => { let a = self.indexed(bus); self.vma(bus); self.regs.s = a }
            0x33 => { let a = self.indexed(bus); self.vma(bus); self.regs.u = a }

            // The indexed columns of everything above. Same addressing, same
            // operations; only the operand fetch differs.
            0xA6 => { let a = self.indexed(bus); let v = bus.read(a); self.load8(v, |r, v| r.a = v) }
            0xE6 => { let a = self.indexed(bus); let v = bus.read(a); self.load8(v, |r, v| r.b = v) }
            0xA7 => { let a = self.indexed(bus); self.store8(bus, a, self.regs.a) }
            0xE7 => { let a = self.indexed(bus); self.store8(bus, a, self.regs.b) }
            0xAE => { let a = self.indexed(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.x = v) }
            0xEE => { let a = self.indexed(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.u = v) }
            0xEC => { let a = self.indexed(bus); let v = self.read16(bus, a); self.load16(v, |r, v| r.set_d(v)) }
            0xAF => { let a = self.indexed(bus); self.store16(bus, a, self.regs.x) }
            0xEF => { let a = self.indexed(bus); self.store16(bus, a, self.regs.u) }
            0xED => { let a = self.indexed(bus); self.store16(bus, a, self.regs.d()) }

            0xA0 => { let v = self.indexed8(bus); let r = self.sub8(self.regs.a, v, false); self.regs.a = r }
            0xE0 => { let v = self.indexed8(bus); let r = self.sub8(self.regs.b, v, false); self.regs.b = r }
            0xA1 => { let v = self.indexed8(bus); self.sub8(self.regs.a, v, false); }
            0xE1 => { let v = self.indexed8(bus); self.sub8(self.regs.b, v, false); }
            0xA2 => { let v = self.indexed8(bus); let c = self.regs.flag(cc::C); let r = self.sub8(self.regs.a, v, c); self.regs.a = r }
            0xE2 => { let v = self.indexed8(bus); let c = self.regs.flag(cc::C); let r = self.sub8(self.regs.b, v, c); self.regs.b = r }
            0xA4 => { let v = self.indexed8(bus); let r = self.regs.a & v; self.logic8(r); self.regs.a = r }
            0xE4 => { let v = self.indexed8(bus); let r = self.regs.b & v; self.logic8(r); self.regs.b = r }
            0xA5 => { let v = self.indexed8(bus); let r = self.regs.a & v; self.logic8(r) }
            0xE5 => { let v = self.indexed8(bus); let r = self.regs.b & v; self.logic8(r) }
            0xA8 => { let v = self.indexed8(bus); let r = self.regs.a ^ v; self.logic8(r); self.regs.a = r }
            0xE8 => { let v = self.indexed8(bus); let r = self.regs.b ^ v; self.logic8(r); self.regs.b = r }
            0xA9 => { let v = self.indexed8(bus); let c = self.regs.flag(cc::C); let r = self.add8(self.regs.a, v, c); self.regs.a = r }
            0xE9 => { let v = self.indexed8(bus); let c = self.regs.flag(cc::C); let r = self.add8(self.regs.b, v, c); self.regs.b = r }
            0xAA => { let v = self.indexed8(bus); let r = self.regs.a | v; self.logic8(r); self.regs.a = r }
            0xEA => { let v = self.indexed8(bus); let r = self.regs.b | v; self.logic8(r); self.regs.b = r }
            0xAB => { let v = self.indexed8(bus); let r = self.add8(self.regs.a, v, false); self.regs.a = r }
            0xEB => { let v = self.indexed8(bus); let r = self.add8(self.regs.b, v, false); self.regs.b = r }

            // 16-bit arithmetic keeps its trailing internal cycle here too.
            0xA3 => { let v = self.indexed16(bus); let r = self.sub16(self.regs.d(), v); self.regs.set_d(r); self.dead(bus) }
            0xE3 => { let v = self.indexed16(bus); let r = self.add16(self.regs.d(), v); self.regs.set_d(r); self.dead(bus) }
            0xAC => { let v = self.indexed16(bus); self.sub16(self.regs.x, v); self.dead(bus) }

            // JSR: the return address is pushed low byte first, so it sits in
            // memory high-byte-lowest and RTS can pull it as a word.
            0xAD => { let t = self.indexed(bus); self.call(bus, t) }

            0x6E => { let a = self.indexed(bus); self.regs.pc = a }
            0x60 | 0x61 | 0x62 | 0x63 | 0x64 | 0x65 | 0x66 | 0x67 | 0x68 | 0x69 | 0x6A | 0x6B | 0x6C | 0x6D | 0x6F => {
                let address = self.indexed(bus);
                self.rmw(bus, address, opcode & 0x0F)
            }

            // Page 2. The prefix is a fetch of its own, and every page-2
            // instruction pays for it.
            0x10 => { let second = self.fetch(bus); self.page10(bus, second) }

            // Short branches: an 8-bit displacement, and the same three cycles
            // whether or not the branch is taken.
            0x20..=0x2F => {
                let offset = self.fetch(bus) as i8;
                self.vma(bus);
                if self.condition(opcode) {
                    self.regs.pc = self.regs.pc.wrapping_add(offset as u16);
                }
            }
            // LBRA is unprefixed and unconditional.
            0x16 => {
                let offset = self.imm16(bus);
                self.vma(bus);
                self.vma(bus);
                self.regs.pc = self.regs.pc.wrapping_add(offset);
            }

            // Branch to subroutine: the same displacement as a branch, plus
            // the return address on the stack.
            0x8D => {
                let offset = self.fetch(bus) as i8;
                self.vma(bus);
                self.vma(bus);
                self.vma(bus);
                let ret = self.regs.pc;
                self.push16(bus, ret);
                self.regs.pc = self.regs.pc.wrapping_add(offset as u16);
            }
            0x17 => {
                let offset = self.imm16(bus);
                for _ in 0..4 {
                    self.vma(bus);
                }
                let ret = self.regs.pc;
                self.push16(bus, ret);
                self.regs.pc = self.regs.pc.wrapping_add(offset);
            }
            0x9D => { let t = self.direct(bus); self.call(bus, t) }
            0xBD => { let t = self.extended(bus); self.call(bus, t) }

            // RTS ends with a read at the stack pointer it has just moved to —
            // the part has nothing left to do and the bus runs anyway.
            0x39 => {
                self.dead(bus);
                let ret = self.pull16(bus);
                bus.read(self.regs.s);
                self.regs.pc = ret;
            }

            0x34 => self.push_mask(bus, true),
            0x35 => self.pull_mask(bus, true),
            0x36 => self.push_mask(bus, false),
            0x37 => self.pull_mask(bus, false),

            0x1E | 0x1F => {
                let postbyte = self.fetch(bus);
                let (src, dst) = (postbyte >> 4, postbyte & 0x0F);
                let from = self.transfer_read(src);
                let to = self.transfer_read(dst);
                // Destination first, then source. The order shows when the two
                // overlap — exchanging D with A writes A, then D writes over
                // its high half — so it is not free to choose.
                self.transfer_write(dst, from);
                if opcode == 0x1E {
                    self.transfer_write(src, to);
                }
                // The exchange costs two cycles more than the copy.
                for _ in 0..if opcode == 0x1E { 6 } else { 4 } {
                    self.vma(bus);
                }
            }

            0x11 => { let second = self.fetch(bus); self.page11(bus, second) }

            // SWI stacks everything and masks both interrupt lines; its two
            // siblings stack everything but leave the masks alone, so they can
            // be used from inside an interrupt handler.
            0x3F => self.software_interrupt(bus, 0xFFFA, true),

            // Undocumented. The one opcode here that is not a function of the
            // registers alone: it ANDs CC with the byte *after* the opcode,
            // read without advancing PC — so the byte it consumes is also the
            // next instruction — shifts that left, and drops the old Z into V.
            0x18 => {
                let stream = bus.read(self.regs.pc);
                let z = (self.regs.cc & cc::Z) >> 1;
                self.regs.cc = ((self.regs.cc & stream) << 1) | z;
            }

            // Undocumented store-immediate. The destination is the immediate
            // operand itself, in the instruction stream. The 8-bit forms are
            // modelled as reading it and keeping only the flags; the 16-bit
            // forms read the first byte and write the register's low half over
            // the second (ARCHITECTURE.md §5 — the reference core's behaviour,
            // which is what the corpus records).
            0x87 => { let v = self.regs.a; self.store_immediate8(bus, v) }
            0xC7 => { let v = self.regs.b; self.store_immediate8(bus, v) }
            0x8F => { let v = self.regs.x; self.store_immediate16(bus, v) }
            0xCF => { let v = self.regs.u; self.store_immediate16(bus, v) }

            // Undocumented ANDCC. The extra cycle is the part's only internal
            // one: the corpus records it with neither read nor write asserted
            // and nothing on the address bus.
            0x38 => {
                bus.internal();
                let mask = self.fetch(bus);
                self.regs.cc &= mask;
                self.dead(bus)
            }

            // Undocumented. Stacks the whole frame like SWI but vectors through
            // RESET and leaves the condition codes exactly as it found them —
            // no E, no masking.
            0x3E => self.stack_and_vector(bus, 0xFFFE),

            // RTI: the stacked E flag says how much was saved. A fast interrupt
            // stacked only CC and PC, and restoring twelve bytes for it would
            // walk the stack.
            0x3B => {
                self.dead(bus);
                self.regs.cc = bus.read(self.regs.s);
                self.regs.s = self.regs.s.wrapping_add(1);
                if self.regs.flag(cc::E) {
                    self.pull_state(bus);
                } else {
                    let pc = self.pull16(bus);
                    self.regs.pc = pc;
                }
                bus.read(self.regs.s);
            }

            _ => panic!("opcode {opcode:#04x} is not implemented"),
        }
    }

    /// Direct page: one operand byte, low; DP supplies the high byte. The dead
    /// cycle sits between forming the address and using it.
    fn direct(&mut self, bus: &mut impl Bus) -> u16 {
        let low = self.fetch(bus);
        let address = u16::from(self.regs.dp) << 8 | u16::from(low);
        self.vma(bus);
        address
    }

    /// Extended: the full address follows the opcode, high byte first.
    fn extended(&mut self, bus: &mut impl Bus) -> u16 {
        let high = self.fetch(bus);
        let low = self.fetch(bus);
        self.vma(bus);
        u16::from(high) << 8 | u16::from(low)
    }

    fn imm8(&mut self, bus: &mut impl Bus) -> u8 {
        self.fetch(bus)
    }

    fn imm16(&mut self, bus: &mut impl Bus) -> u16 {
        let high = self.fetch(bus);
        let low = self.fetch(bus);
        u16::from(high) << 8 | u16::from(low)
    }

    fn read16(&mut self, bus: &mut impl Bus, address: u16) -> u16 {
        let high = bus.read(address);
        let low = bus.read(address.wrapping_add(1));
        u16::from(high) << 8 | u16::from(low)
    }

    /// Loads and stores both report the value in N and Z and clear V; neither
    /// touches C.
    fn load8(&mut self, value: u8, into: fn(&mut Registers, u8)) {
        into(&mut self.regs, value);
        self.regs.set_nz8(value);
        self.regs.set_flag(cc::V, false);
    }

    fn load16(&mut self, value: u16, into: fn(&mut Registers, u16)) {
        into(&mut self.regs, value);
        self.regs.set_nz16(value);
        self.regs.set_flag(cc::V, false);
    }

    fn store8(&mut self, bus: &mut impl Bus, address: u16, value: u8) {
        bus.write(address, value);
        self.regs.set_nz8(value);
        self.regs.set_flag(cc::V, false);
    }

    fn store16(&mut self, bus: &mut impl Bus, address: u16, value: u16) {
        bus.write(address, (value >> 8) as u8);
        bus.write(address.wrapping_add(1), value as u8);
        self.regs.set_nz16(value);
        self.regs.set_flag(cc::V, false);
    }

    /// Fetch an 8-bit operand in whichever mode the opcode's column selects:
    /// `0x8_`/`0xC_` immediate, `0x9_`/`0xD_` direct, `0xB_`/`0xF_` extended.
    /// Column is bits 4-5 of the opcode, shared across the whole ALU block.
    fn operand8(&mut self, bus: &mut impl Bus, opcode: u8) -> u8 {
        match opcode & 0x30 {
            0x00 => self.imm8(bus),
            0x10 => { let a = self.direct(bus); bus.read(a) }
            _ => { let a = self.extended(bus); bus.read(a) }
        }
    }

    /// Page-2 opcodes, reached through the `0x10` prefix.
    fn page10(&mut self, bus: &mut impl Bus, opcode: u8) {
        match opcode {
            // Long branches. Unlike the short forms these cost an extra cycle
            // when taken, which is the one timing difference a cycle-counted
            // loop can feel.
            0x20..=0x2F => {
                let offset = self.imm16(bus);
                self.vma(bus);
                if self.condition(opcode) {
                    self.vma(bus);
                    self.regs.pc = self.regs.pc.wrapping_add(offset);
                }
            }
            0x3F => self.software_interrupt(bus, 0xFFF4, false),
            // Undocumented, and the siblings of page 1's 0x3E: the whole frame
            // is stacked and the vector taken, but CC is left as it was found.
            0x3E => self.stack_and_vector(bus, 0xFFF4),
            0x87 => { let v = self.regs.a; self.store_immediate8(bus, v) }
            0xC7 => { let v = self.regs.b; self.store_immediate8(bus, v) }
            0x8F => { let v = self.regs.y; self.store_immediate16(bus, v) }
            0xCF => { let v = self.regs.s; self.store_immediate16(bus, v) }
            // Undocumented ADDD that never stores its sum: the flags are the
            // whole effect, which makes it a compare with the wrong arithmetic.
            0xC3 | 0xD3 | 0xE3 | 0xF3 => { let v = self.wide_operand(bus, opcode); self.add16(self.regs.d(), v); self.dead(bus) }

            // 16-bit compares and the Y/S loads and stores. The prefix has
            // already been paid for; from here the shapes match page 1.
            0x83 | 0x93 | 0xA3 | 0xB3 => { let v = self.wide_operand(bus, opcode); self.sub16(self.regs.d(), v); self.dead(bus) }
            0x8C | 0x9C | 0xAC | 0xBC => { let v = self.wide_operand(bus, opcode); self.sub16(self.regs.y, v); self.dead(bus) }
            0x8E | 0x9E | 0xAE | 0xBE => { let v = self.wide_operand(bus, opcode); self.load16(v, |r, v| r.y = v) }
            0xCE | 0xDE | 0xEE | 0xFE => { let v = self.wide_operand(bus, opcode); self.load16(v, |r, v| r.s = v) }
            0x9F | 0xAF | 0xBF => { let a = self.wide_address(bus, opcode); self.store16(bus, a, self.regs.y) }
            0xDF | 0xEF | 0xFF => { let a = self.wide_address(bus, opcode); self.store16(bus, a, self.regs.s) }

            _ => panic!("opcode 10 {opcode:#04x} is not implemented"),
        }
    }

    /// Page-3 opcodes, reached through the `0x11` prefix.
    fn page11(&mut self, bus: &mut impl Bus, opcode: u8) {
        match opcode {
            0x3F => self.software_interrupt(bus, 0xFFF2, false),
            0x3E => self.stack_and_vector(bus, 0xFFF6),
            0x87 => { let v = self.regs.a; self.store_immediate8(bus, v) }
            0xC7 => { let v = self.regs.b; self.store_immediate8(bus, v) }
            0x8F => { let v = self.regs.x; self.store_immediate16(bus, v) }
            0xCF => { let v = self.regs.u; self.store_immediate16(bus, v) }
            // The same sumless add, on U.
            0xC3 | 0xD3 | 0xE3 | 0xF3 => { let v = self.wide_operand(bus, opcode); self.add16(self.regs.u, v); self.dead(bus) }
            0x83 | 0x93 | 0xA3 | 0xB3 => { let v = self.wide_operand(bus, opcode); self.sub16(self.regs.u, v); self.dead(bus) }
            0x8C | 0x9C | 0xAC | 0xBC => { let v = self.wide_operand(bus, opcode); self.sub16(self.regs.s, v); self.dead(bus) }
            _ => panic!("opcode 11 {opcode:#04x} is not implemented"),
        }
    }

    /// Stack the whole machine state and vector through `vector`, leaving CC
    /// exactly as it was found — no E, no masking. The undocumented 0x3E and
    /// its two page-prefixed siblings all do this and differ only in vector.
    fn stack_and_vector(&mut self, bus: &mut impl Bus, vector: u16) {
        self.dead(bus);
        self.vma(bus);
        self.push_state(bus);
        self.vma(bus);
        let target = self.read16(bus, vector);
        self.vma(bus);
        self.regs.pc = target;
    }

    /// Undocumented store-immediate, 8-bit: read the operand byte and keep
    /// only the flags the store would have set.
    fn store_immediate8(&mut self, bus: &mut impl Bus, value: u8) {
        self.fetch(bus);
        self.logic8(value);
    }

    /// Undocumented store-immediate, 16-bit: read the first operand byte, then
    /// write the register's low half over the second, in the instruction
    /// stream. Only the low half — the high one is never stored anywhere.
    fn store_immediate16(&mut self, bus: &mut impl Bus, value: u16) {
        self.fetch(bus);
        let at = self.regs.pc;
        self.regs.pc = self.regs.pc.wrapping_add(1);
        bus.write(at, value as u8);
        self.regs.set_nz16(value);
        self.regs.set_flag(cc::V, false);
    }

    /// Stack the whole machine state and vector through `vector`.
    ///
    /// `mask` is set only for SWI itself: SWI2 and SWI3 deliberately leave I
    /// and F alone so they remain usable from inside a handler.
    fn software_interrupt(&mut self, bus: &mut impl Bus, vector: u16, mask: bool) {
        self.dead(bus);
        self.vma(bus);
        // E marks the frame as complete, and it is set before the push so the
        // stacked copy says what was stacked.
        self.regs.set_flag(cc::E, true);
        self.push_state(bus);
        self.vma(bus);
        if mask {
            self.regs.set_flag(cc::I, true);
            self.regs.set_flag(cc::F, true);
        }
        let target = self.read16(bus, vector);
        self.vma(bus);
        self.regs.pc = target;
    }

    /// Push the full twelve-byte frame onto the hardware stack.
    fn push_state(&mut self, bus: &mut impl Bus) {
        let (pc, u, y, x) = (self.regs.pc, self.regs.u, self.regs.y, self.regs.x);
        for value in [pc, u, y, x] {
            self.regs.s = self.regs.s.wrapping_sub(1);
            bus.write(self.regs.s, value as u8);
            self.regs.s = self.regs.s.wrapping_sub(1);
            bus.write(self.regs.s, (value >> 8) as u8);
        }
        for value in [self.regs.dp, self.regs.b, self.regs.a, self.regs.cc] {
            self.regs.s = self.regs.s.wrapping_sub(1);
            bus.write(self.regs.s, value);
        }
    }

    /// Pull the frame back, CC already restored by the caller.
    fn pull_state(&mut self, bus: &mut impl Bus) {
        for slot in 0..3 {
            let value = bus.read(self.regs.s);
            self.regs.s = self.regs.s.wrapping_add(1);
            match slot {
                0 => self.regs.a = value,
                1 => self.regs.b = value,
                _ => self.regs.dp = value,
            }
        }
        for slot in 0..4 {
            let value = self.pull16(bus);
            match slot {
                0 => self.regs.x = value,
                1 => self.regs.y = value,
                2 => self.regs.u = value,
                _ => self.regs.pc = value,
            }
        }
    }

    /// Whether a branch of this opcode's low nibble is taken. Shared by the
    /// short and long forms, which differ only in displacement width.
    fn condition(&self, opcode: u8) -> bool {
        let (n, z, v, c) = (
            self.regs.flag(cc::N),
            self.regs.flag(cc::Z),
            self.regs.flag(cc::V),
            self.regs.flag(cc::C),
        );
        match opcode & 0x0F {
            0x0 => true,
            0x1 => false,
            0x2 => !c && !z,        // higher
            0x3 => c || z,          // lower or same
            0x4 => !c,              // carry clear / higher or same
            0x5 => c,               // carry set / lower
            0x6 => !z,
            0x7 => z,
            0x8 => !v,
            0x9 => v,
            0xA => !n,
            0xB => n,
            0xC => n == v,          // greater or equal, signed
            0xD => n != v,          // less than, signed
            0xE => !z && n == v,    // greater than, signed
            _ => z || n != v,       // less or equal, signed
        }
    }

    /// A 16-bit operand across all four addressing modes, selected by the
    /// opcode's column: `0x8_` immediate, `0x9_` direct, `0xA_` indexed,
    /// `0xB_` extended (and the same in the `0xC_`–`0xF_` half).
    fn wide_operand(&mut self, bus: &mut impl Bus, opcode: u8) -> u16 {
        match opcode & 0x30 {
            0x00 => self.imm16(bus),
            0x10 => { let a = self.direct(bus); self.read16(bus, a) }
            0x20 => self.indexed16(bus),
            _ => { let a = self.extended(bus); self.read16(bus, a) }
        }
    }

    /// The address a store's column selects. Immediate has no meaning here.
    fn wide_address(&mut self, bus: &mut impl Bus, opcode: u8) -> u16 {
        match opcode & 0x30 {
            0x10 => self.direct(bus),
            0x20 => self.indexed(bus),
            _ => self.extended(bus),
        }
    }

    /// An indexed 8-bit operand: the held-back cycle is the read.
    fn indexed8(&mut self, bus: &mut impl Bus) -> u8 {
        let address = self.indexed(bus);
        bus.read(address)
    }

    /// An indexed 16-bit operand.
    fn indexed16(&mut self, bus: &mut impl Bus) -> u16 {
        let address = self.indexed(bus);
        self.read16(bus, address)
    }

    /// Read a register for TFR/EXG, widened to 16 bits.
    ///
    /// An 8-bit register arrives with `0xFF` in the high half rather than a
    /// copy of itself, and an undefined encoding reads as all ones. That is the
    /// behaviour observed on real hardware and recorded by the corpus; it
    /// differs from MAME's, which duplicates the byte. The vectors decide.
    fn transfer_read(&self, code: u8) -> u16 {
        match code {
            0x0 => self.regs.d(),
            0x1 => self.regs.x,
            0x2 => self.regs.y,
            0x3 => self.regs.u,
            0x4 => self.regs.s,
            0x5 => self.regs.pc,
            0x8 => 0xFF00 | u16::from(self.regs.a),
            0x9 => 0xFF00 | u16::from(self.regs.b),
            0xA => 0xFF00 | u16::from(self.regs.cc),
            0xB => 0xFF00 | u16::from(self.regs.dp),
            _ => 0xFFFF,
        }
    }

    /// Write a register for TFR/EXG. An 8-bit destination keeps the low half.
    fn transfer_write(&mut self, code: u8, value: u16) {
        match code {
            0x0 => self.regs.set_d(value),
            0x1 => self.regs.x = value,
            0x2 => self.regs.y = value,
            0x3 => self.regs.u = value,
            0x4 => self.regs.s = value,
            0x5 => self.regs.pc = value,
            0x8 => self.regs.a = value as u8,
            0x9 => self.regs.b = value as u8,
            0xA => self.regs.cc = value as u8,
            0xB => self.regs.dp = value as u8,
            _ => {}
        }
    }

    /// Push the registers a mask names, PC first and CC last so a pull walking
    /// upward meets them in the opposite order.
    ///
    /// Bit 6 means "the other stack pointer": U when pushing onto S, S when
    /// pushing onto U. Each 16-bit register goes low byte first, so it lands
    /// high-byte-lowest and reads back as a word.
    fn push_mask(&mut self, bus: &mut impl Bus, hardware: bool) {
        let mask = self.fetch(bus);
        self.vma(bus);
        self.vma(bus);
        let mut sp = if hardware { self.regs.s } else { self.regs.u };
        bus.read(sp);

        let other = if hardware { self.regs.u } else { self.regs.s };
        let wide = [
            (0x80, self.regs.pc),
            (0x40, other),
            (0x20, self.regs.y),
            (0x10, self.regs.x),
        ];
        for (bit, value) in wide {
            if mask & bit != 0 {
                sp = sp.wrapping_sub(1);
                bus.write(sp, value as u8);
                sp = sp.wrapping_sub(1);
                bus.write(sp, (value >> 8) as u8);
            }
        }
        for (bit, value) in [(0x08, self.regs.dp), (0x04, self.regs.b), (0x02, self.regs.a), (0x01, self.regs.cc)] {
            if mask & bit != 0 {
                sp = sp.wrapping_sub(1);
                bus.write(sp, value);
            }
        }
        if hardware {
            self.regs.s = sp;
        } else {
            self.regs.u = sp;
        }
    }

    /// Pull the registers a mask names, CC first and PC last, then one last
    /// read at the pointer it finished on.
    fn pull_mask(&mut self, bus: &mut impl Bus, hardware: bool) {
        let mask = self.fetch(bus);
        self.vma(bus);
        self.vma(bus);
        let mut sp = if hardware { self.regs.s } else { self.regs.u };

        for bit in [0x01, 0x02, 0x04, 0x08] {
            if mask & bit != 0 {
                let value = bus.read(sp);
                sp = sp.wrapping_add(1);
                match bit {
                    0x01 => self.regs.cc = value,
                    0x02 => self.regs.a = value,
                    0x04 => self.regs.b = value,
                    _ => self.regs.dp = value,
                }
            }
        }
        for bit in [0x10, 0x20, 0x40, 0x80] {
            if mask & bit != 0 {
                let high = bus.read(sp);
                let low = bus.read(sp.wrapping_add(1));
                sp = sp.wrapping_add(2);
                let value = u16::from(high) << 8 | u16::from(low);
                match bit {
                    0x10 => self.regs.x = value,
                    0x20 => self.regs.y = value,
                    0x40 => {
                        if hardware {
                            self.regs.u = value;
                        } else {
                            self.regs.s = value;
                        }
                    }
                    _ => self.regs.pc = value,
                }
            }
        }
        if hardware {
            self.regs.s = sp;
        } else {
            self.regs.u = sp;
        }
        let end = if hardware { self.regs.s } else { self.regs.u };
        bus.read(end);
    }

    /// Jump to a subroutine at an address already formed.
    fn call(&mut self, bus: &mut impl Bus, target: u16) {
        self.dead(bus);
        self.vma(bus);
        let ret = self.regs.pc;
        self.push16(bus, ret);
        self.regs.pc = target;
    }

    fn pull16(&mut self, bus: &mut impl Bus) -> u16 {
        let high = bus.read(self.regs.s);
        let low = bus.read(self.regs.s.wrapping_add(1));
        self.regs.s = self.regs.s.wrapping_add(2);
        u16::from(high) << 8 | u16::from(low)
    }

    /// Push a word onto the hardware stack, low byte first so the word reads
    /// back big-endian like everything else on the part.
    fn push16(&mut self, bus: &mut impl Bus, value: u16) {
        self.regs.s = self.regs.s.wrapping_sub(1);
        bus.write(self.regs.s, value as u8);
        self.regs.s = self.regs.s.wrapping_sub(1);
        bus.write(self.regs.s, (value >> 8) as u8);
    }

    /// The register a postbyte's bits 5-6 select.
    fn index_reg(&self, postbyte: u8) -> u16 {
        match postbyte >> 5 & 3 {
            0 => self.regs.x,
            1 => self.regs.y,
            2 => self.regs.u,
            _ => self.regs.s,
        }
    }

    fn set_index_reg(&mut self, postbyte: u8, value: u16) {
        match postbyte >> 5 & 3 {
            0 => self.regs.x = value,
            1 => self.regs.y = value,
            2 => self.regs.u = value,
            _ => self.regs.s = value,
        }
    }

    /// Resolve an indexed postbyte to an effective address, emitting the
    /// cycles it costs.
    ///
    /// The cycle counts are the datasheet's table and were re-derived from the
    /// corpus, including the encodings the datasheet leaves undefined — modes
    /// 7, A and E, and indirection on the single-step auto-increment forms,
    /// all of which the part executes and the vectors record. Indirection is
    /// regular throughout: it replaces the form's final internal cycle with a
    /// 16-bit fetch through the address and two more internal cycles.
    fn indexed(&mut self, bus: &mut impl Bus) -> u16 {
        let postbyte = self.fetch(bus);

        // The short form is an offset in the postbyte itself, five bits signed,
        // and has no indirect variant.
        if postbyte & 0x80 == 0 {
            let offset = ((postbyte & 0x1F) as i32) << 27 >> 27;
            let address = self.index_reg(postbyte).wrapping_add(offset as u16);
            self.dead(bus);
            self.vma(bus);
            return address;
        }

        let indirect = postbyte & 0x10 != 0;
        let base = self.index_reg(postbyte);
        // (instruction-stream reads after the postbyte, internal cycles)
        let (address, pc_reads, vmas): (u16, u16, u32) = match postbyte & 0x0F {
            0x0 => { self.set_index_reg(postbyte, base.wrapping_add(1)); (base, 1, 3) }
            0x1 => { self.set_index_reg(postbyte, base.wrapping_add(2)); (base, 1, 4) }
            0x2 => { let a = base.wrapping_sub(1); self.set_index_reg(postbyte, a); (a, 1, 3) }
            0x3 => { let a = base.wrapping_sub(2); self.set_index_reg(postbyte, a); (a, 1, 4) }
            0x4 => (base, 1, 1),
            0x5 => (base.wrapping_add(i16::from(self.regs.b as i8) as u16), 1, 2),
            0x6 => (base.wrapping_add(i16::from(self.regs.a as i8) as u16), 1, 2),
            // Undefined encodings. The part yields zero rather than the base
            // register — behaviour no document states and only the vectors show.
            0x7 => (0, 0, 1),
            0x8 => { let n = self.fetch(bus); (base.wrapping_add(i16::from(n as i8) as u16), 1, 1) }
            0x9 => { let n = self.imm16(bus); (base.wrapping_add(n), 0, 4) }
            0xA => (0, 0, 1),
            0xB => (base.wrapping_add(self.regs.d()), 2, 4),
            0xC => { let n = self.fetch(bus); (self.regs.pc.wrapping_add(i16::from(n as i8) as u16), 0, 2) }
            0xD => { let n = self.imm16(bus); (self.regs.pc.wrapping_add(n), 0, 5) }
            0xE => (0, 0, 1),
            _ => { let n = self.imm16(bus); (n, 0, 2) }
        };

        // Successive instruction-stream reads walk forward, so mode B's pair
        // lands on PC and PC+1 rather than twice on PC. PC itself does not
        // move: these are discarded reads, not fetches.
        for offset in 0..pc_reads {
            bus.read(self.regs.pc.wrapping_add(offset));
        }
        // One internal cycle is held back: it is the slot the operation itself
        // occupies — a read for a load, a write for a store, a VMA for LEA
        // which has nothing to fetch. Every indexed opcode costs the same up to
        // that point, which is why the whole 0xA0/0xE0 block shares this.
        for _ in 0..vmas - 1 {
            self.vma(bus);
        }
        if indirect {
            let through = self.read16(bus, address);
            self.vma(bus);
            return through;
        }
        address
    }

    /// Decimal adjust: fold A back into two BCD digits after an add.
    ///
    /// Each nibble is corrected if it carried — H and C record those carries —
    /// or if it holds a value no decimal digit can.
    fn daa(&mut self) {
        let a = self.regs.a;
        let carry = self.regs.flag(cc::C);
        let mut correction = 0u8;
        if self.regs.flag(cc::H) || a & 0x0F > 9 {
            correction |= 0x06;
        }
        if carry || a >> 4 > 9 || (a >> 4 == 9 && a & 0x0F > 9) {
            correction |= 0x60;
        }
        let wide = u16::from(a) + u16::from(correction);
        self.regs.a = wide as u8;
        // The carry latches: a correction can set it, but an add that already
        // carried does not lose it.
        self.regs.set_flag(cc::C, carry || wide > 0xFF);
        self.regs.set_flag(cc::V, false);
        let result = self.regs.a;
        self.regs.set_nz8(result);
    }

    /// The trailing cycle of an inherent read-modify-write. An operation that
    /// stores its result re-reads the instruction stream; TST, which stores
    /// nothing, takes a VMA cycle instead — the same substitution it makes in
    /// the memory form.
    fn settle(&mut self, bus: &mut impl Bus, stored: bool) {
        if stored {
            self.dead(bus);
        } else {
            self.vma(bus);
        }
    }

    /// Read the target, transform it, write it back.
    ///
    /// TST is the odd one: it wants the flags but not the store, and pays for
    /// the cycles it does not use with two VMA reads rather than the usual
    /// internal cycle and write.
    fn rmw(&mut self, bus: &mut impl Bus, address: u16, op: u8) {
        let value = bus.read(address);
        let result = self.transform(op, value);
        match result {
            Some(result) => {
                self.dead(bus);
                bus.write(address, result);
            }
            None => {
                self.vma(bus);
                self.vma(bus);
            }
        }
    }

    /// The shared read-modify-write operations, by opcode low nibble. `None`
    /// means the result is not stored.
    fn transform(&mut self, op: u8, v: u8) -> Option<u8> {
        let carry = self.regs.flag(cc::C);
        // Two encodings the datasheet leaves out really are aliases: 1 is NEG
        // and 5 is LSR. The other two only look like aliases — B decrements
        // but also reports the borrow in C, and E clears but leaves C alone —
        // so they have arms of their own below.
        let op = match op {
            0x1 => 0x0,
            0x5 => 0x4,
            other => other,
        };
        let result = match op {
            // NEG. V marks the one value that cannot be negated; C is set for
            // anything non-zero, which is the borrow out of 0 - v.
            0x0 => {
                let r = 0u8.wrapping_sub(v);
                self.regs.set_flag(cc::V, v == 0x80);
                self.regs.set_flag(cc::C, v != 0);
                Some(r)
            }
            // Negate with carry: a subtract-with-borrow from zero, which is
            // what NEG would be if it did not ignore the incoming carry.
            0x2 => Some(self.sub8(0, v, carry)),
            0x3 => {
                self.regs.set_flag(cc::V, false);
                self.regs.set_flag(cc::C, true);
                Some(!v)
            }
            // LSR/ROR/ASR shift a bit out into C and leave V alone.
            0x4 => {
                self.regs.set_flag(cc::C, v & 1 != 0);
                Some(v >> 1)
            }
            0x6 => {
                let r = v >> 1 | u8::from(carry) << 7;
                self.regs.set_flag(cc::C, v & 1 != 0);
                Some(r)
            }
            0x7 => {
                self.regs.set_flag(cc::C, v & 1 != 0);
                Some(v >> 1 | v & 0x80)
            }
            // ASL/ROL do set V, from the two bits that end up straddling the
            // sign after the shift.
            0x8 => {
                let r = v << 1;
                self.regs.set_flag(cc::C, v & 0x80 != 0);
                self.regs.set_flag(cc::V, (v ^ v << 1) & 0x80 != 0);
                Some(r)
            }
            0x9 => {
                let r = v << 1 | u8::from(carry);
                self.regs.set_flag(cc::C, v & 0x80 != 0);
                self.regs.set_flag(cc::V, (v ^ v << 1) & 0x80 != 0);
                Some(r)
            }
            // DEC/INC leave C alone; V marks the wrap across the sign.
            0xA => {
                self.regs.set_flag(cc::V, v == 0x80);
                Some(v.wrapping_sub(1))
            }
            // The undocumented decrement is not quite an alias: it reports the
            // borrow out of v-1 in C, which the documented one leaves alone.
            0xB => {
                self.regs.set_flag(cc::V, v == 0x80);
                self.regs.set_flag(cc::C, v != 0);
                Some(v.wrapping_sub(1))
            }
            0xC => {
                self.regs.set_flag(cc::V, v == 0x7F);
                Some(v.wrapping_add(1))
            }
            0xD => {
                self.regs.set_flag(cc::V, false);
                None
            }
            0xF => {
                self.regs.set_flag(cc::V, false);
                self.regs.set_flag(cc::C, false);
                Some(0)
            }
            // The undocumented clear leaves the carry as it found it.
            0xE => {
                self.regs.set_flag(cc::V, false);
                Some(0)
            }
            _ => panic!("not a read-modify-write operation: {op:#x}"),
        };
        self.regs.set_nz8(result.unwrap_or(v));
        result
    }

    /// Fetch a 16-bit operand in whichever mode the opcode's column selects.
    fn operand16(&mut self, bus: &mut impl Bus, opcode: u8) -> u16 {
        match opcode & 0x30 {
            0x00 => self.imm16(bus),
            0x10 => { let a = self.direct(bus); self.read16(bus, a) }
            _ => { let a = self.extended(bus); self.read16(bus, a) }
        }
    }

    fn add16(&mut self, left: u16, right: u16) -> u16 {
        let wide = u32::from(left) + u32::from(right);
        let result = wide as u16;
        self.regs.set_flag(cc::C, wide > 0xFFFF);
        self.regs.set_flag(cc::V, ((left ^ result) & (right ^ result) & 0x8000) != 0);
        self.regs.set_nz16(result);
        result
    }

    fn sub16(&mut self, left: u16, right: u16) -> u16 {
        let wide = u32::from(left).wrapping_sub(u32::from(right));
        let result = wide as u16;
        self.regs.set_flag(cc::C, wide & 0x1_0000 != 0);
        self.regs.set_flag(cc::V, ((left ^ right) & (left ^ result) & 0x8000) != 0);
        self.regs.set_nz16(result);
        result
    }

    /// Add, with carry in. H is the carry out of bit 3 — a BCD artefact that
    /// DAA later reads, and one the vectors check.
    fn add8(&mut self, left: u8, right: u8, carry: bool) -> u8 {
        let c = u16::from(carry);
        let wide = u16::from(left) + u16::from(right) + c;
        let result = wide as u8;
        let half = (left & 0x0F) + (right & 0x0F) + c as u8;
        self.regs.set_flag(cc::H, half > 0x0F);
        self.regs.set_flag(cc::C, wide > 0xFF);
        // Overflow: both operands agreed in sign and the result disagreed.
        self.regs.set_flag(cc::V, ((left ^ result) & (right ^ result) & 0x80) != 0);
        self.regs.set_nz8(result);
        result
    }

    /// Subtract, with borrow in. C is a *borrow* here, set when the subtrahend
    /// exceeded the minuend.
    fn sub8(&mut self, left: u8, right: u8, borrow: bool) -> u8 {
        let b = u16::from(borrow);
        let wide = u16::from(left).wrapping_sub(u16::from(right)).wrapping_sub(b);
        let result = wide as u8;
        self.regs.set_flag(cc::C, wide & 0x100 != 0);
        self.regs.set_flag(cc::V, ((left ^ right) & (left ^ result) & 0x80) != 0);
        self.regs.set_nz8(result);
        result
    }

    /// AND/OR/EOR and BIT: N and Z from the result, V cleared, C untouched.
    fn logic8(&mut self, result: u8) {
        self.regs.set_nz8(result);
        self.regs.set_flag(cc::V, false);
    }

    /// A VMA cycle: no valid address, so the part reads `0xFFFF` and discards
    /// it. Distinct from [`Self::dead`], which re-reads the instruction stream.
    fn vma(&mut self, bus: &mut impl Bus) {
        bus.read(0xFFFF);
    }

    /// Read the byte at PC and advance.
    fn fetch(&mut self, bus: &mut impl Bus) -> u8 {
        let value = bus.read(self.regs.pc);
        self.regs.pc = self.regs.pc.wrapping_add(1);
        value
    }

    /// A dead cycle. The part is doing internal work, but the bus is still
    /// driven and the read still happens — on the 6809 from the instruction
    /// stream at PC, or from `0xFFFF` on a VMA cycle. The value is discarded.
    /// These are recorded by the verification corpus exactly like any other
    /// read, which is why they cannot be skipped (ARCHITECTURE.md §5).
    fn dead(&mut self, bus: &mut impl Bus) {
        bus.read(self.regs.pc);
    }
}

#[cfg(test)]
mod interrupt_tests {
    use super::*;
    use crate::bus::RecordingBus;

    fn machine() -> (Cpu, RecordingBus) {
        let mut bus = RecordingBus::new();
        // The IRQ vector points somewhere recognisable.
        bus.poke(0xFFF8, 0x40);
        bus.poke(0xFFF9, 0x00);
        let mut cpu = Cpu::new();
        cpu.regs.pc = 0x1234;
        cpu.regs.s = 0x2000;
        cpu.regs.a = 0xAA;
        cpu.regs.b = 0xBB;
        cpu.regs.dp = 0xCC;
        cpu.regs.x = 0x1111;
        cpu.regs.y = 0x2222;
        cpu.regs.u = 0x3333;
        cpu.regs.cc = 0x00;
        (cpu, bus)
    }

    #[test]
    fn a_masked_interrupt_is_not_taken() {
        let (mut cpu, mut bus) = machine();
        cpu.irq = true;
        cpu.regs.set_flag(cc::I, true);
        bus.poke(0x1234, 0x12); // NOP
        cpu.step(&mut bus);
        assert_eq!(cpu.regs.pc, 0x1235, "it should have run the NOP instead");
    }

    #[test]
    fn taking_an_interrupt_stacks_everything_and_vectors() {
        let (mut cpu, mut bus) = machine();
        cpu.irq = true;
        cpu.step(&mut bus);

        assert_eq!(cpu.regs.pc, 0x4000, "PC comes from the vector at $FFF8");
        assert_eq!(cpu.regs.s, 0x2000 - 12, "a full frame is twelve bytes");
        assert!(cpu.regs.flag(cc::I), "further interrupts are masked");
        assert!(cpu.regs.flag(cc::E), "the frame is marked complete");
        // The stacked CC says the frame was complete, so RTI knows to restore
        // all of it.
        assert_eq!(bus.peek(0x2000 - 12) & cc::E, cc::E);
        // The line is sampled before the fetch, so the interrupted address is
        // the instruction that had not yet run — not the one after it.
        assert_eq!(bus.peek(0x2000 - 1), 0x34, "return address low byte");
        assert_eq!(bus.peek(0x2000 - 2), 0x12, "return address high byte");
    }

    #[test]
    fn an_interrupt_and_its_return_leave_the_machine_as_it_was() {
        let (mut cpu, mut bus) = machine();
        let before = cpu.regs;
        cpu.irq = true;
        cpu.step(&mut bus);

        cpu.irq = false;
        bus.poke(0x4000, 0x3B); // RTI
        cpu.step(&mut bus);

        // Everything comes back except E, which the entry set to record that a
        // full frame was stacked — RTI restores the CC it finds, E and all.
        assert_eq!(cpu.regs.pc, before.pc, "returned to the interrupted instruction");
        assert_eq!(cpu.regs.s, before.s);
        assert_eq!(
            (cpu.regs.a, cpu.regs.b, cpu.regs.dp, cpu.regs.x, cpu.regs.y, cpu.regs.u),
            (before.a, before.b, before.dp, before.x, before.y, before.u)
        );
        assert_eq!(cpu.regs.cc, before.cc | cc::E);
    }

    #[test]
    fn a_held_line_interrupts_again_once_unmasked() {
        let (mut cpu, mut bus) = machine();
        cpu.irq = true;
        cpu.step(&mut bus);
        let first = cpu.regs.s;

        // Still asserted, but I is set, so the handler runs undisturbed.
        bus.poke(0x4000, 0x12); // NOP
        cpu.step(&mut bus);
        assert_eq!(cpu.regs.s, first, "nothing more was stacked");

        // Clearing the mask lets the level assert again — this is what makes
        // a device that holds the line get served repeatedly.
        cpu.regs.set_flag(cc::I, false);
        cpu.step(&mut bus);
        assert_eq!(cpu.regs.s, first - 12, "a second frame");
    }
}

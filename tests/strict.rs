//! Strict mode (DTEMU_STRICT): an access to an unmapped page raises an access
//! error with the faulting instruction's PC in the frame.

use dtemu::bus::Bus;
use dtemu::cpu::Cpu;

#[test]
fn null_read_takes_vector_2() {
    const CODE: u32 = 0x4010_0000;
    const HANDLER: u32 = 0x4011_0000;
    let mut c = Cpu::new(Bus::new(128));
    c.bus.set_strict(true);
    // move.l (0x10).w,d0 ; nop ; nop
    c.bus.poke_bytes(CODE, &[0x20, 0x38, 0x00, 0x10, 0x4e, 0x71, 0x4e, 0x71]);
    c.bus.poke_bytes(HANDLER, &[0x4e, 0x71, 0x60, 0xfe]); // nop ; bra.s *
    c.vbr = 0x4000_0000;
    c.bus.poke32(c.vbr + 2 * 4, HANDLER);
    c.sr = 0x2700;
    c.a[7] = 0x4080_0000;
    c.pc = CODE;
    c.run(c.icount() + 3);
    assert_eq!(c.bus.strict_log.len(), 1);
    let frame_pc = c.bus.peek32(c.a[7] + 4);
    assert_eq!(frame_pc, CODE, "the frame holds the faulting instruction");
    assert_eq!((c.bus.peek32(c.a[7]) >> 18) & 0xFF, 2, "vector 2");
    assert!((HANDLER..HANDLER + 4).contains(&c.pc));
}

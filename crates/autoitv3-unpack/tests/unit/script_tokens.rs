//! Unit tests for `script::tokens`'s deassembler and its float spelling.
//!
//! Kept out of `tokens.rs` so the module reads as implementation;
//! `#[path]` pulls the file back in as a unit-test module, which is what
//! lets it reach private state the module does not expose.

use super::*;

/// Encode one token stream the way the compiler does, so the deassembler
/// can be tested without a sample binary.
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new(lines: u32) -> Self {
        Encoder { bytes: lines.to_le_bytes().to_vec() }
    }

    fn op(&mut self, opcode: u8) -> &mut Self {
        self.bytes.push(opcode);
        self
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// A length-prefixed XOR-obfuscated UTF-16 string.
    fn string(&mut self, text: &str) -> &mut Self {
        let units: Vec<u16> = text.encode_utf16().collect();
        let key = units.len() as u32;
        self.u32(key);
        for unit in units {
            self.bytes.extend_from_slice(&(unit ^ key as u16).to_le_bytes());
        }
        self
    }

    fn keyword(&mut self, name: &str) -> &mut Self {
        self.op(0x30).string(name)
    }

    fn end_line(&mut self) -> &mut Self {
        self.op(0x7F)
    }

    fn done(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

#[test]
fn a_call_and_a_string_come_back_out() {
    let mut e = Encoder::new(1);
    e.op(0x33).string("x"); // $x
    e.op(0x41); // =
    e.op(0x31).string("MsgBox"); // function name
    e.op(0x47); // (
    e.op(0x36).string("hi \"there\""); // string literal, quotes doubled
    e.op(0x40); // ,
    e.op(0x05).u32(7); // 7
    e.op(0x48); // )
    e.end_line();
    // Tokens are joined with single spaces; this is not cosmetic but the
    // reference's output, so it is what a recovered script looks like.
    assert_eq!(deassemble(&e.done()).unwrap(), "$x = MsgBox ( \"hi \"\"there\"\"\" , 7 )\r\n");
}

#[test]
fn a_builtin_function_index_resolves_through_the_table() {
    // 0x01 indexes FUNCTIONS; `Abs` is entry 0.
    let mut e = Encoder::new(1);
    e.op(0x01).u32(0);
    e.op(0x47);
    e.op(0x05).u32(3);
    e.op(0x48);
    e.end_line();
    assert_eq!(deassemble(&e.done()).unwrap(), "Abs ( 3 )\r\n");
}

#[test]
fn keywords_drive_the_indentation() {
    let mut e = Encoder::new(3);
    e.keyword("If").op(0x05).u32(1).keyword("Then");
    e.end_line();
    e.op(0x33).string("x");
    e.end_line();
    e.keyword("EndIf");
    e.end_line();
    assert_eq!(deassemble(&e.done()).unwrap(), "If 1 Then\r\n\t$x\r\nEndIf\r\n");
}

#[test]
fn a_single_line_if_does_not_indent_its_body() {
    let mut e = Encoder::new(1);
    e.keyword("If").op(0x05).u32(1).keyword("Then");
    e.op(0x33).string("x");
    e.op(0x41);
    e.op(0x05).u32(2);
    e.end_line();
    assert_eq!(deassemble(&e.done()).unwrap(), "If 1 Then $x = 2\r\n");
}

#[test]
fn an_unknown_opcode_is_an_error_rather_than_silence() {
    let mut e = Encoder::new(1);
    e.op(0xAA);
    e.end_line();
    assert!(deassemble(&e.done()).is_err());
}

/// `repr(float)` in CPython, captured as bit patterns so the test does not
/// depend on a Python install.
#[test]
fn floats_are_spelled_the_way_python_spells_them() {
    let cases: [(u64, &str); 30] = [
        (0x0000000000000000, "0.0"),
        (0x8000000000000000, "-0.0"),
        (0x3ff0000000000000, "1.0"),
        (0xbff0000000000000, "-1.0"),
        (0x3fe0000000000000, "0.5"),
        (0x3fb999999999999a, "0.1"),
        (0x3ff8000000000000, "1.5"),
        (0x4059000000000000, "100.0"),
        (0x419d6f3454000000, "123456789.0"),
        (0x430c6bf526340000, "1000000000000000.0"),
        (0x4341c37937e08000, "1e+16"),
        (0x4376345785d8a000, "1e+17"),
        (0x3ff0000000000001, "1.0000000000000002"),
        (0x3f1a36e2eb1c432d, "0.0001"),
        (0x3ee4f8b588e368f1, "1e-05"),
        (0x3f202e4b6ce5dc68, "0.00012345"),
        (0x54b249ad2594c37d, "1e+100"),
        (0x2b2bff2ee48e0530, "1e-100"),
        (0x400921fb54442d11, "3.14159265358979"),
        (0x3df12e0be826d695, "2.5e-10"),
        (0x43118b54f22aeb00, "1234567890123456.0"),
        (0x4345ee2a2eb5a5c4, "1.2345678901234568e+16"),
        (0x4340000000000000, "9007199254740992.0"),
        (0x7fefffffffffffff, "1.7976931348623157e+308"),
        (0x0000000000000001, "5e-324"),
        (0x3fd3333333333334, "0.30000000000000004"),
        (0x3fd5555555555555, "0.3333333333333333"),
        (0x3fe5555555555555, "0.6666666666666666"),
        (0xbe80c6f7a0b5ed8d, "-1.25e-07"),
        (0x44dfde9f10a8d361, "6.02e+23"),
    ];
    for (bits, expected) in cases {
        assert_eq!(python_float_repr(f64::from_bits(bits)), expected, "bits {bits:#x}");
    }
}

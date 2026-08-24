//! Reading the single-step verification corpus.
//!
//! The MOO container is a flat sequence of tagged chunks, little-endian
//! throughout: a `MOO ` header, then one `TEST` chunk per test holding `NAME`,
//! `BYTS`, `INIT`, `FINA`, `CYCL` and `PORT`. Layout taken from the corpus's
//! own generator (`testgen/moo_writer.cpp` and `testgen/main.cpp`), which is
//! the authority wherever its README leaves the framing implicit.
//!
//! The corpus is unlicensed for redistribution, so it is read from wherever it
//! was cloned and never copied into this repository (ARCHITECTURE.md §5).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::Registers;

/// Where the vectors live. Overridable so the harness can be pointed
/// elsewhere without the path being baked in.
pub fn corpus_dir() -> PathBuf {
    std::env::var_os("M6809_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".cache/trexy/m6809-tests/v1")
        })
}

/// One recorded machine cycle.
///
/// `status` is the generator's four ASCII characters: `r-m-` a read, `-wm-` a
/// write, `----` an internal cycle (which only opcode 38 produces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cycle {
    pub pins: u8,
    pub address: u16,
    pub data: u8,
    pub status: [u8; 4],
}

impl Cycle {
    pub fn is_write(&self) -> bool {
        &self.status == b"-wm-"
    }
    pub fn is_read(&self) -> bool {
        &self.status == b"r-m-"
    }
    pub fn is_internal(&self) -> bool {
        &self.status == b"----"
    }
}

/// Registers plus the memory the test touches.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineState {
    pub registers: Registers,
    /// Address/value pairs. Initial and final list the same addresses.
    pub ram: Vec<(u16, u8)>,
}

#[derive(Clone, Debug)]
pub struct Test {
    pub name: String,
    pub bytes: Vec<u8>,
    pub initial: MachineState,
    pub final_state: MachineState,
    pub cycles: Vec<Cycle>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let v = *self.data.get(self.at).ok_or("ran off the end")?;
        self.at += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from(self.u8()?) | u16::from(self.u8()?) << 8)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let mut v = 0u32;
        for shift in [0, 8, 16, 24] {
            v |= u32::from(self.u8()?) << shift;
        }
        Ok(v)
    }
    fn tag(&mut self) -> Result<[u8; 4], String> {
        let end = self.at + 4;
        let slice = self.data.get(self.at..end).ok_or("ran off the end")?;
        self.at = end;
        Ok([slice[0], slice[1], slice[2], slice[3]])
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at + n;
        let slice = self.data.get(self.at..end).ok_or("ran off the end")?;
        self.at = end;
        Ok(slice)
    }
    /// A tagged chunk: four-character tag, u32 length, payload.
    fn chunk(&mut self, expect: &[u8; 4]) -> Result<Reader<'a>, String> {
        let tag = self.tag()?;
        if &tag != expect {
            return Err(format!(
                "expected chunk {} but found {}",
                String::from_utf8_lossy(expect),
                String::from_utf8_lossy(&tag)
            ));
        }
        let len = self.u32()? as usize;
        Ok(Reader { data: self.bytes(len)?, at: 0 })
    }
    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
}

/// REGS: a u16 presence mask then nine u16s, the 8-bit registers zero-extended.
fn registers(r: &mut Reader) -> Result<Registers, String> {
    let mask = r.u16()?;
    if mask != 0x01FF {
        return Err(format!("unexpected register mask {mask:#06x}"));
    }
    Ok(Registers {
        pc: r.u16()?,
        s: r.u16()?,
        u: r.u16()?,
        x: r.u16()?,
        y: r.u16()?,
        dp: r.u16()? as u8,
        a: r.u16()? as u8,
        b: r.u16()? as u8,
        cc: r.u16()? as u8,
    })
}

fn ram(r: &mut Reader) -> Result<Vec<(u16, u8)>, String> {
    let count = r.u32()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let address = r.u32()? as u16;
        out.push((address, r.u8()?));
    }
    Ok(out)
}

fn state(r: &mut Reader) -> Result<MachineState, String> {
    let mut regs = r.chunk(b"REGS")?;
    let registers = registers(&mut regs)?;
    let mut mem = r.chunk(b"RAM ")?;
    Ok(MachineState { registers, ram: ram(&mut mem)? })
}

/// Decode one `<stem>.moo.gz`.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<Test>, String> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|e| {
        format!(
            "{}: {e}\nThe m6809 corpus is not vendored — clone it and point \
             M6809_CORPUS at its v1 directory, or place it at {}",
            path.display(),
            corpus_dir().display()
        )
    })?;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(file)
        .read_to_end(&mut raw)
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let mut r = Reader { data: &raw, at: 0 };
    if &r.tag()? != b"MOO " {
        return Err(format!("{}: not a MOO file", path.display()));
    }
    let header = r.u32()? as usize;
    let mut head = Reader { data: r.bytes(header)?, at: 0 };
    let (_major, _minor) = (head.u8()?, head.u8()?);
    let (_a, _b) = (head.u8()?, head.u8()?);
    let count = head.u32()? as usize;
    let cpu = head.tag()?;
    if &cpu != b"6809" {
        return Err(format!("{}: corpus is for {}", path.display(), String::from_utf8_lossy(&cpu)));
    }

    let mut tests = Vec::with_capacity(count);
    while !r.done() {
        let mut t = r.chunk(b"TEST")?;
        let _index = t.u32()?;
        let mut name = t.chunk(b"NAME")?;
        let n = name.u32()? as usize;
        let name = String::from_utf8_lossy(name.bytes(n)?).into_owned();
        let mut byts = t.chunk(b"BYTS")?;
        let n = byts.u32()? as usize;
        let bytes = byts.bytes(n)?.to_vec();
        let mut init = t.chunk(b"INIT")?;
        let initial = state(&mut init)?;
        let mut fina = t.chunk(b"FINA")?;
        let final_state = state(&mut fina)?;
        let mut cycl = t.chunk(b"CYCL")?;
        let n = cycl.u32()? as usize;
        let mut cycles = Vec::with_capacity(n);
        for _ in 0..n {
            let pins = cycl.u8()?;
            let address = cycl.u16()?;
            let data = cycl.u8()?;
            let s = cycl.bytes(4)?;
            cycles.push(Cycle { pins, address, data, status: [s[0], s[1], s[2], s[3]] });
        }
        let _port = t.chunk(b"PORT")?;
        tests.push(Test { name, bytes, initial, final_state, cycles });
    }
    if tests.len() != count {
        return Err(format!("{}: header says {count} tests, found {}", path.display(), tests.len()));
    }
    Ok(tests)
}

/// Every `<stem>.moo.gz` in the corpus, sorted.
pub fn stems() -> Result<Vec<(String, PathBuf)>, String> {
    let dir = corpus_dir();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".moo.gz") {
            out.push((stem.to_owned(), path));
        }
    }
    out.sort();
    Ok(out)
}

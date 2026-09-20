//! A minimal ELF32 reader: just enough to load a RISC-V test binary and find
//! the `tohost` symbol it reports results through.
//!
//! Hand-rolled rather than pulled from crates.io because the loader is part of
//! what is being verified -- a bug here looks exactly like a bug in the hart,
//! and the subset of ELF that matters is small enough to read in one sitting.

use std::collections::HashMap;

pub struct Elf {
    /// Segments to place in memory, as (physical address, bytes, zero-fill length).
    pub segments: Vec<Segment>,
    pub entry: u32,
    pub symbols: HashMap<String, u32>,
}

pub struct Segment {
    pub addr: u32,
    pub data: Vec<u8>,
    /// Bytes beyond `data` that must read as zero -- the .bss part of a segment.
    pub zero_len: u32,
}

#[derive(Debug)]
pub enum ElfError {
    TooShort,
    NotElf,
    Not32Bit,
    NotLittleEndian,
    NotRiscv,
    NotExecutable,
}

impl std::fmt::Display for ElfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            ElfError::TooShort => "file is too short to be an ELF",
            ElfError::NotElf => "missing the \\x7fELF magic",
            ElfError::Not32Bit => "not ELF32 (RV64 support is a later milestone)",
            ElfError::NotLittleEndian => "not little-endian",
            ElfError::NotRiscv => "not a RISC-V object (e_machine != 243)",
            ElfError::NotExecutable => "not an executable (e_type != ET_EXEC)",
        };
        f.write_str(msg)
    }
}

const PT_LOAD: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const EM_RISCV: u16 = 243;
const ET_EXEC: u16 = 2;

/// Little-endian scalar reads. Out-of-range offsets yield 0 rather than
/// panicking, so a truncated file is rejected by the header checks instead of
/// bringing the process down.
fn u16le(b: &[u8], off: usize) -> u16 {
    b.get(off..off + 2)
        .map_or(0, |s| u16::from_le_bytes(s.try_into().unwrap()))
}
fn u32le(b: &[u8], off: usize) -> u32 {
    b.get(off..off + 4)
        .map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

impl Elf {
    pub fn parse(bytes: &[u8]) -> Result<Elf, ElfError> {
        if bytes.len() < 52 {
            return Err(ElfError::TooShort);
        }
        if &bytes[0..4] != b"\x7fELF" {
            return Err(ElfError::NotElf);
        }
        if bytes[4] != 1 {
            return Err(ElfError::Not32Bit);
        }
        if bytes[5] != 1 {
            return Err(ElfError::NotLittleEndian);
        }
        if u16le(bytes, 18) != EM_RISCV {
            return Err(ElfError::NotRiscv);
        }
        if u16le(bytes, 16) != ET_EXEC {
            return Err(ElfError::NotExecutable);
        }

        let entry = u32le(bytes, 24);
        let phoff = u32le(bytes, 28) as usize;
        let shoff = u32le(bytes, 32) as usize;
        let phentsize = u16le(bytes, 42) as usize;
        let phnum = u16le(bytes, 44) as usize;
        let shentsize = u16le(bytes, 46) as usize;
        let shnum = u16le(bytes, 48) as usize;

        // Program headers say what to load. p_paddr is used rather than
        // p_vaddr: there is no MMU yet, and riscv-tests link them equal anyway.
        let mut segments = Vec::new();
        for i in 0..phnum {
            let ph = phoff + i * phentsize;
            if u32le(bytes, ph) != PT_LOAD {
                continue;
            }
            let offset = u32le(bytes, ph + 4) as usize;
            let paddr = u32le(bytes, ph + 12);
            let filesz = u32le(bytes, ph + 16) as usize;
            let memsz = u32le(bytes, ph + 20);
            let data = bytes
                .get(offset..offset + filesz)
                .ok_or(ElfError::TooShort)?
                .to_vec();
            segments.push(Segment {
                addr: paddr,
                data,
                zero_len: memsz.saturating_sub(filesz as u32),
            });
        }

        // Section headers carry the symbol table, which is how `tohost` is found.
        let mut symbols = HashMap::new();
        for i in 0..shnum {
            let sh = shoff + i * shentsize;
            if u32le(bytes, sh + 4) != SHT_SYMTAB {
                continue;
            }
            let symoff = u32le(bytes, sh + 16) as usize;
            let symsize = u32le(bytes, sh + 20) as usize;
            let entsize = u32le(bytes, sh + 36) as usize;
            // sh_link (at +24 in Elf32_Shdr) names the string table holding
            // this symtab's names.
            let strtab_idx = u32le(bytes, sh + 24) as usize;
            let strtab = u32le(bytes, shoff + strtab_idx * shentsize + 16) as usize;
            if entsize == 0 {
                continue;
            }
            for s in (0..symsize).step_by(entsize) {
                let sym = symoff + s;
                let name_off = strtab + u32le(bytes, sym) as usize;
                let value = u32le(bytes, sym + 4);
                let Some(rest) = bytes.get(name_off..) else {
                    continue;
                };
                let len = rest.iter().position(|&c| c == 0).unwrap_or(0);
                if len > 0 {
                    symbols.insert(String::from_utf8_lossy(&rest[..len]).into_owned(), value);
                }
            }
        }

        Ok(Elf {
            segments,
            entry,
            symbols,
        })
    }
}

//! A minimal ELF reader, ELF32 and ELF64: just enough to load a RISC-V test
//! binary and find the `tohost` symbol it reports results through.
//!
//! Hand-rolled rather than pulled from crates.io because the loader is part of
//! what is being verified -- a bug here looks exactly like a bug in the hart,
//! and the subset of ELF that matters is small enough to read in one sitting.

use std::collections::HashMap;

pub struct Elf {
    /// Segments to place in memory.
    pub segments: Vec<Segment>,
    pub entry: u64,
    pub symbols: HashMap<String, u64>,
    /// ELF64, which is what selects RV64 when the image is loaded.
    pub is_64: bool,
}

pub struct Segment {
    pub addr: u64,
    pub data: Vec<u8>,
    /// Bytes beyond `data` that must read as zero -- the .bss part of a segment.
    pub zero_len: u64,
}

#[derive(Debug)]
pub enum ElfError {
    TooShort,
    NotElf,
    BadClass,
    NotLittleEndian,
    NotRiscv,
    NotExecutable,
}

impl std::fmt::Display for ElfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            ElfError::TooShort => "file is too short to be an ELF",
            ElfError::NotElf => "missing the \\x7fELF magic",
            ElfError::BadClass => "not ELF32 or ELF64",
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
fn u64le(b: &[u8], off: usize) -> u64 {
    b.get(off..off + 8)
        .map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()))
}

/// The two classes differ only in field widths and offsets, so each accessor
/// takes the class once and the parser below reads the same way for both.
struct Layout {
    is_64: bool,
}

impl Layout {
    /// A word that is 4 bytes in ELF32 and 8 in ELF64 (addresses, offsets).
    fn word(&self, b: &[u8], off32: usize, off64: usize, base: usize) -> u64 {
        if self.is_64 {
            u64le(b, base + off64)
        } else {
            u32le(b, base + off32) as u64
        }
    }
}

impl Elf {
    pub fn parse(bytes: &[u8]) -> Result<Elf, ElfError> {
        if bytes.len() < 52 {
            return Err(ElfError::TooShort);
        }
        if &bytes[0..4] != b"\x7fELF" {
            return Err(ElfError::NotElf);
        }
        let is_64 = match bytes[4] {
            1 => false,
            2 => true,
            _ => return Err(ElfError::BadClass),
        };
        if bytes[5] != 1 {
            return Err(ElfError::NotLittleEndian);
        }
        if u16le(bytes, 18) != EM_RISCV {
            return Err(ElfError::NotRiscv);
        }
        if u16le(bytes, 16) != ET_EXEC {
            return Err(ElfError::NotExecutable);
        }
        let l = Layout { is_64 };

        // Elf32_Ehdr / Elf64_Ehdr: e_entry, e_phoff, e_shoff then the halfwords.
        let entry = l.word(bytes, 24, 24, 0);
        let phoff = l.word(bytes, 28, 32, 0) as usize;
        let shoff = l.word(bytes, 32, 40, 0) as usize;
        let (phentsize_off, phnum_off, shentsize_off, shnum_off) = if is_64 {
            (54, 56, 58, 60)
        } else {
            (42, 44, 46, 48)
        };
        let phentsize = u16le(bytes, phentsize_off) as usize;
        let phnum = u16le(bytes, phnum_off) as usize;
        let shentsize = u16le(bytes, shentsize_off) as usize;
        let shnum = u16le(bytes, shnum_off) as usize;

        // Program headers say what to load. p_paddr is used rather than
        // p_vaddr: there is no MMU yet, and riscv-tests link them equal anyway.
        // ELF64 reorders the fields, putting p_flags straight after p_type.
        let mut segments = Vec::new();
        for i in 0..phnum {
            let ph = phoff + i * phentsize;
            if u32le(bytes, ph) != PT_LOAD {
                continue;
            }
            let (offset, paddr, filesz, memsz) = if is_64 {
                (
                    u64le(bytes, ph + 8) as usize,
                    u64le(bytes, ph + 24),
                    u64le(bytes, ph + 32) as usize,
                    u64le(bytes, ph + 40),
                )
            } else {
                (
                    u32le(bytes, ph + 4) as usize,
                    u32le(bytes, ph + 12) as u64,
                    u32le(bytes, ph + 16) as usize,
                    u32le(bytes, ph + 20) as u64,
                )
            };
            let data = bytes
                .get(offset..offset + filesz)
                .ok_or(ElfError::TooShort)?
                .to_vec();
            segments.push(Segment {
                addr: paddr,
                data,
                zero_len: memsz.saturating_sub(filesz as u64),
            });
        }

        // Section headers carry the symbol table, which is how `tohost` is
        // found. sh_link (the string table index) is at +24 in Elf32_Shdr and
        // +40 in Elf64_Shdr; getting that wrong yields a symbol table of
        // plausible addresses under garbage names.
        let (sh_off_o, sh_size_o, sh_link_o, sh_entsize_o) = if is_64 {
            (24, 32, 40, 56)
        } else {
            (16, 20, 24, 36)
        };
        let mut symbols = HashMap::new();
        for i in 0..shnum {
            let sh = shoff + i * shentsize;
            if u32le(bytes, sh + 4) != SHT_SYMTAB {
                continue;
            }
            let symoff = l.word(bytes, sh_off_o, sh_off_o, sh) as usize;
            let symsize = l.word(bytes, sh_size_o, sh_size_o, sh) as usize;
            let entsize = l.word(bytes, sh_entsize_o, sh_entsize_o, sh) as usize;
            let strtab_idx = u32le(bytes, sh + sh_link_o) as usize;
            let strtab = l.word(bytes, sh_off_o, sh_off_o, shoff + strtab_idx * shentsize) as usize;
            if entsize == 0 {
                continue;
            }
            for s in (0..symsize).step_by(entsize) {
                let sym = symoff + s;
                // Elf32_Sym is {name, value, ...}; Elf64_Sym moves value to +8.
                let name_off = strtab + u32le(bytes, sym) as usize;
                let value = if is_64 {
                    u64le(bytes, sym + 8)
                } else {
                    u32le(bytes, sym + 4) as u64
                };
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
            is_64,
        })
    }
}

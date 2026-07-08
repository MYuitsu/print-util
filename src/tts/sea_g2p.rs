use memmap2::Mmap;
use once_cell::sync::Lazy;
use regex::Regex;
use std::fs::File;
use std::io;
use std::path::Path;

/// Minimal mmap-backed dictionary reader adapted from sea-g2p Rust core.
/// The binary layout is compatible with `sea_g2p.bin`.
#[derive(Debug)]
pub struct PhonemeDict {
    mmap: Mmap,
    string_count: u32,
    merged_count: u32,
    common_count: u32,
    string_offsets_pos: usize,
    merged_pos: usize,
    common_pos: usize,
}

impl PhonemeDict {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: mapping a read-only file; lifetime is owned by this struct.
        let mmap = unsafe { Mmap::map(&file)? };
        if mmap.len() < 32 || &mmap[0..4] != b"SEAP" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid sea_g2p.bin header",
            ));
        }

        Ok(Self {
            string_count: u32::from_le_bytes(mmap[8..12].try_into().unwrap_or([0; 4])),
            merged_count: u32::from_le_bytes(mmap[12..16].try_into().unwrap_or([0; 4])),
            common_count: u32::from_le_bytes(mmap[16..20].try_into().unwrap_or([0; 4])),
            string_offsets_pos: u32::from_le_bytes(mmap[20..24].try_into().unwrap_or([0; 4]))
                as usize,
            merged_pos: u32::from_le_bytes(mmap[24..28].try_into().unwrap_or([0; 4])) as usize,
            common_pos: u32::from_le_bytes(mmap[28..32].try_into().unwrap_or([0; 4])) as usize,
            mmap,
        })
    }

    fn get_string(&self, id: u32) -> &str {
        if id >= self.string_count {
            return "";
        }
        let off_ptr = self.string_offsets_pos + (id as usize * 4);
        if off_ptr + 4 > self.mmap.len() {
            return "";
        }

        let offset =
            u32::from_le_bytes(self.mmap[off_ptr..off_ptr + 4].try_into().unwrap_or([0; 4]))
                as usize;
        let start = 32 + offset;
        if start >= self.mmap.len() {
            return "";
        }
        let mut end = start;
        while end < self.mmap.len() && self.mmap[end] != 0 {
            end += 1;
        }
        std::str::from_utf8(&self.mmap[start..end]).unwrap_or("")
    }

    pub fn lookup_merged(&self, word: &str) -> Option<&str> {
        let mut low: i32 = 0;
        let mut high: i32 = self.merged_count as i32 - 1;
        while low <= high {
            let mid = (low + high) / 2;
            let ptr = self.merged_pos + (mid as usize * 8);
            if ptr + 8 > self.mmap.len() {
                return None;
            }
            let w_id = u32::from_le_bytes(self.mmap[ptr..ptr + 4].try_into().ok()?);
            let current_word = self.get_string(w_id);
            if current_word == word {
                let p_id = u32::from_le_bytes(self.mmap[ptr + 4..ptr + 8].try_into().ok()?);
                return Some(self.get_string(p_id));
            }
            if current_word < word {
                low = mid + 1;
            } else {
                high = mid - 1;
            }
        }
        None
    }

    pub fn lookup_common(&self, word: &str) -> Option<(&str, &str)> {
        let mut low: i32 = 0;
        let mut high: i32 = self.common_count as i32 - 1;
        while low <= high {
            let mid = (low + high) / 2;
            let ptr = self.common_pos + (mid as usize * 12);
            if ptr + 12 > self.mmap.len() {
                return None;
            }
            let w_id = u32::from_le_bytes(self.mmap[ptr..ptr + 4].try_into().ok()?);
            let current_word = self.get_string(w_id);
            if current_word == word {
                let vi_id = u32::from_le_bytes(self.mmap[ptr + 4..ptr + 8].try_into().ok()?);
                let en_id = u32::from_le_bytes(self.mmap[ptr + 8..ptr + 12].try_into().ok()?);
                return Some((self.get_string(vi_id), self.get_string(en_id)));
            }
            if current_word < word {
                low = mid + 1;
            } else {
                high = mid - 1;
            }
        }
        None
    }
}

static TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(<en>.*?</en>)|(\w+(?:['’]\w+)*)|([^\w\s])").expect("valid token regex")
});

#[derive(Debug)]
pub struct SeaG2pCore {
    dict: PhonemeDict,
}

impl SeaG2pCore {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            dict: PhonemeDict::open(path)?,
        })
    }

    /// Simplified phonemization path:
    /// - Lookup merged dictionary first.
    /// - Fallback to common dictionary (prefer VI pronunciation).
    /// - Keep original token if not found.
    pub fn phonemize(&self, text: &str) -> String {
        let mut out = Vec::new();
        for caps in TOKEN_RE.captures_iter(text) {
            if let Some(tagged) = caps.get(1) {
                out.push(tagged.as_str().to_string());
                continue;
            }
            if let Some(word) = caps.get(2) {
                let lw = word.as_str().to_lowercase();
                if let Some(merged) = self.dict.lookup_merged(&lw) {
                    out.push(merged.replace("<en>", ""));
                } else if let Some((vi, en)) = self.dict.lookup_common(&lw) {
                    let chosen = if !vi.trim().is_empty() { vi } else { en };
                    out.push(chosen.replace("<en>", ""));
                } else {
                    out.push(lw);
                }
                continue;
            }
            if let Some(punct) = caps.get(3) {
                out.push(punct.as_str().to_string());
            }
        }
        out.join(" ")
    }
}

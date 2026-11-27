//! Reads packed files from an Electron ASAR archive, such as the Codex and
//! Grok Bot app bundles.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};

const INDEX_LIMIT: u64 = 16 * 1024 * 1024;

pub struct Archive {
    file: File,
    index: Value,
    data_start: u64,
    length: u64,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let mut prefix = [0; 16];
        file.read_exact(&mut prefix)?;
        // The JSON index is a string in a Chromium Pickle whose size is in a
        // second Pickle. The packed files follow it.
        let field = |offset: usize| {
            u32::from_le_bytes([
                prefix[offset],
                prefix[offset + 1],
                prefix[offset + 2],
                prefix[offset + 3],
            ])
        };
        let data_start = 8 + u64::from(field(4));
        let index_size = u64::from(field(12));
        ensure!(
            field(0) == 4
                && index_size <= INDEX_LIMIT
                && 16 + index_size <= data_start
                && data_start <= length,
            "Invalid ASAR header"
        );
        let mut index = vec![0; index_size as usize];
        file.read_exact(&mut index)?;
        Ok(Self {
            file,
            index: serde_json::from_slice(&index).context("Invalid ASAR index")?,
            data_start,
            length,
        })
    }

    fn folder(&self, folder: &str) -> Option<&Map<String, Value>> {
        folder
            .split('/')
            .try_fold(&self.index, |entry, name| entry.get("files")?.get(name))?
            .get("files")?
            .as_object()
    }

    /// Names the entries in a folder such as `dist/renderer/assets`.
    pub fn files(&self, folder: &str) -> Option<impl Iterator<Item = &str>> {
        Some(self.folder(folder)?.keys().map(String::as_str))
    }

    pub fn size(&self, folder: &str, name: &str) -> Option<u64> {
        self.folder(folder)?.get(name)?.get("size")?.as_u64()
    }

    /// Reads a packed file of at most `limit` bytes.
    pub fn read(&mut self, folder: &str, name: &str, limit: u64) -> Result<Vec<u8>> {
        let entry = self
            .folder(folder)
            .and_then(|files| files.get(name))
            .with_context(|| format!("{name} is missing from the app bundle"))?;
        let size = entry["size"].as_u64().context("Invalid ASAR entry size")?;
        let offset = entry["offset"]
            .as_str()
            .context("Missing packed ASAR offset")?
            .parse::<u64>()
            .context("Invalid ASAR offset")?;
        let start = self
            .data_start
            .checked_add(offset)
            .context("Invalid ASAR offset")?;
        ensure!(
            size <= limit && start <= self.length && size <= self.length - start,
            "Invalid bounds for {name} in the app bundle"
        );
        self.file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; size as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn archive(index: &Value, data: &[u8]) -> Result<tempfile::NamedTempFile> {
        let json = serde_json::to_vec(index)?;
        let padded = json.len().next_multiple_of(4);
        let mut bytes = Vec::new();
        for field in [4, 8 + padded, 4 + padded, json.len()] {
            bytes.extend_from_slice(&u32::try_from(field)?.to_le_bytes());
        }
        bytes.extend_from_slice(&json);
        bytes.resize(16 + padded, 0);
        bytes.extend_from_slice(data);
        let mut file = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(&mut file, &bytes)?;
        Ok(file)
    }

    #[test]
    fn reads_packed_files_and_rejects_bad_entries() -> Result<()> {
        let index = json!({"files": {"dist": {"files": {"assets": {"files": {
            "main.js": {"size": 5, "offset": "0"},
            "font.woff2": {"size": 3, "offset": "5"},
            "past-end.js": {"size": 4, "offset": "6"},
            "unpacked.node": {"size": 2, "unpacked": true},
        }}}}}});
        let file = archive(&index, b"hellowoff")?;
        let mut archive = Archive::open(file.path())?;
        let mut names: Vec<_> = archive.files("dist/assets").unwrap().collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["font.woff2", "main.js", "past-end.js", "unpacked.node"]
        );
        assert!(archive.files("dist/missing").is_none());
        assert_eq!(archive.read("dist/assets", "main.js", 5)?, b"hello");
        assert_eq!(archive.read("dist/assets", "font.woff2", 1024)?, b"wof");
        assert!(archive.read("dist/assets", "main.js", 4).is_err());
        assert!(archive.read("dist/assets", "past-end.js", 1024).is_err());
        assert!(archive.read("dist/assets", "unpacked.node", 1024).is_err());
        assert!(archive.read("dist/assets", "missing.js", 1024).is_err());
        Ok(())
    }

    #[test]
    fn rejects_a_corrupt_header() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        std::io::Write::write_all(
            &mut file,
            &[4, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0, 2, 0, 0, 0],
        )?;
        assert!(Archive::open(file.path()).is_err());
        Ok(())
    }
}

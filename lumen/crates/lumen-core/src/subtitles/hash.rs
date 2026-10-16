//! The OpenSubtitles file hash: the file size plus the sum of its first and last 64 KiB read as
//! little-endian 64-bit words. Two copies of the same release have the same hash whatever they
//! are called, which is how a service finds the exact subtitle for a file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::Result;

const CHUNK: u64 = 64 * 1024;

/// `(hash, size)`, or `None` for a file too small to hash (the service ignores those).
pub fn opensubtitles_hash(path: &Path) -> Result<Option<(String, u64)>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    if size < CHUNK * 2 {
        return Ok(None);
    }
    let mut hash = size;
    let mut buffer = vec![0u8; CHUNK as usize];
    for offset in [0, size - CHUNK] {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buffer)?;
        for word in buffer.chunks_exact(8) {
            let value = u64::from_le_bytes(word.try_into().expect("eight bytes"));
            hash = hash.wrapping_add(value);
        }
    }
    Ok(Some((format!("{hash:016x}"), size)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file_of(size: usize, patch: impl FnOnce(&mut Vec<u8>)) -> tempfile_path::TempFile {
        let mut bytes = vec![0u8; size];
        patch(&mut bytes);
        let temp = tempfile_path::TempFile::new();
        File::create(&temp.0).unwrap().write_all(&bytes).unwrap();
        temp
    }

    /// A file in the system temp directory that is removed afterwards.
    mod tempfile_path {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicU32, Ordering};

        static NEXT: AtomicU32 = AtomicU32::new(0);

        pub struct TempFile(pub PathBuf);

        impl TempFile {
            pub fn new() -> Self {
                let id = NEXT.fetch_add(1, Ordering::SeqCst);
                Self(std::env::temp_dir().join(format!("lumen-hash-{}-{id}", std::process::id())))
            }
        }

        impl Drop for TempFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }

    #[test]
    fn hash_is_the_size_plus_both_ends() {
        let size = 3 * CHUNK as usize;
        let zeros = file_of(size, |_| {});
        assert_eq!(
            opensubtitles_hash(&zeros.0).unwrap(),
            Some((format!("{:016x}", size), size as u64))
        );

        // One word in the first block and one in the last block each add to the sum; a word in
        // the middle of the file does not.
        let patched = file_of(size, |bytes| {
            bytes[0..8].copy_from_slice(&1u64.to_le_bytes());
            let last = bytes.len() - 8;
            bytes[last..].copy_from_slice(&2u64.to_le_bytes());
            let middle = CHUNK as usize + 16;
            bytes[middle..middle + 8].copy_from_slice(&99u64.to_le_bytes());
        });
        assert_eq!(
            opensubtitles_hash(&patched.0).unwrap().unwrap().0,
            format!("{:016x}", size + 3)
        );
    }

    #[test]
    fn hash_wraps_instead_of_overflowing() {
        let size = 2 * CHUNK as usize;
        let file = file_of(size, |bytes| {
            bytes[0..8].copy_from_slice(&u64::MAX.to_le_bytes());
        });
        assert_eq!(
            opensubtitles_hash(&file.0).unwrap().unwrap().0,
            format!("{:016x}", (size as u64).wrapping_add(u64::MAX))
        );
    }

    #[test]
    fn small_and_missing_files_have_no_hash() {
        let small = file_of(1000, |_| {});
        assert_eq!(opensubtitles_hash(&small.0).unwrap(), None);
        assert!(opensubtitles_hash(Path::new("/definitely/not/here.mkv")).is_err());
    }
}

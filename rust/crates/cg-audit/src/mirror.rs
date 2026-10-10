//! A second copy of an audit log on a different volume, so rewriting the log leaves a witness the writer cannot also rewrite.
//!
//! The hash chain and the signed anchor make tampering detectable; an external timestamp proves a past state existed. A mirror adds
//! something simpler and stronger against an attacker who gets at the log file: the mirror only ever grows, and every sync first proves
//! that what it already holds is still, byte for byte, the beginning of the source. If someone edits, truncates or replaces the source (or
//! the mirror), the next sync refuses and says so, and nothing is overwritten. Mount the mirror directory on storage the application
//! account can append to but not rewrite (an append-only or object-lock volume) and the guarantee becomes immutability of the copy.
//!
//! Only whole lines are copied: a row being written at this very moment is picked up by the next sync.

use crate::AuditError;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

fn io(e: std::io::Error) -> AuditError {
    AuditError::Invalid(e.to_string())
}

/// SHA-256 of the first `n` bytes of a file, or None if the file is shorter.
fn prefix_hash(path: &Path, n: u64) -> Result<Option<String>, AuditError> {
    let mut f = File::open(path).map_err(io)?;
    let mut h = Sha256::new();
    let mut left = n;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let got = f.read(&mut buf[..want]).map_err(io)?;
        if got == 0 {
            return Ok(None);
        }
        h.update(&buf[..got]);
        left -= got as u64;
    }
    Ok(Some(hex::encode(h.finalize())))
}

/// Brings `mirror` up to date with `source`. Returns the number of bytes appended. Errors (and changes nothing) when the mirror is not a
/// prefix of the source, or the source is shorter than the mirror.
pub fn sync_mirror(source: &Path, mirror: &Path) -> Result<u64, AuditError> {
    if !source.exists() {
        return Ok(0); // nothing has been written yet
    }
    if let Some(dir) = mirror.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let have = if mirror.exists() { std::fs::metadata(mirror).map_err(io)?.len() } else { 0 };
    if have > 0 {
        match prefix_hash(source, have)? {
            None => return Err(AuditError::Invalid(format!("the source is shorter than its mirror ({have} bytes): it was truncated or replaced"))),
            Some(src) => {
                if prefix_hash(mirror, have)?.as_deref() != Some(src.as_str()) {
                    return Err(AuditError::Invalid(format!("the first {have} bytes of the source differ from the mirror: the log was rewritten, or the mirror was")));
                }
            }
        }
    }
    let mut f = File::open(source).map_err(io)?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(have)).map_err(io)?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).map_err(io)?;
    let whole = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if whole == 0 {
        return Ok(0);
    }
    let mut out = OpenOptions::new().create(true).append(true).open(mirror).map_err(io)?;
    out.write_all(&tail[..whole]).map_err(io)?;
    out.sync_all().map_err(io)?;
    Ok(whole as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let (s, m) = (d.path().join("log.jsonl"), d.path().join("mirror").join("log.jsonl"));
        (d, s, m)
    }

    #[test]
    fn a_mirror_follows_a_growing_log_one_whole_line_at_a_time() {
        let (_d, s, m) = setup();
        assert_eq!(sync_mirror(&s, &m).unwrap(), 0, "no log yet");
        std::fs::write(&s, "one\ntwo\n").unwrap();
        assert_eq!(sync_mirror(&s, &m).unwrap(), 8);
        assert_eq!(std::fs::read_to_string(&m).unwrap(), "one\ntwo\n");
        std::fs::write(&s, "one\ntwo\nthree\npart").unwrap();
        assert_eq!(sync_mirror(&s, &m).unwrap(), 6, "the half-written last row waits for the next sync");
        assert_eq!(std::fs::read_to_string(&m).unwrap(), "one\ntwo\nthree\n");
        assert_eq!(sync_mirror(&s, &m).unwrap(), 0, "nothing new");
    }

    #[test]
    fn rewriting_truncating_or_replacing_the_log_is_refused_and_overwrites_nothing() {
        let (_d, s, m) = setup();
        std::fs::write(&s, "alpha\nbeta\ngamma\n").unwrap();
        sync_mirror(&s, &m).unwrap();
        std::fs::write(&s, "alpha\nBETA\ngamma\ndelta\n").unwrap(); // one row edited, one appended
        assert!(sync_mirror(&s, &m).unwrap_err().to_string().contains("differ"));
        std::fs::write(&s, "alpha\n").unwrap(); // truncated
        assert!(sync_mirror(&s, &m).unwrap_err().to_string().contains("shorter"));
        std::fs::write(&s, "something else entirely, as long as the old one!\n").unwrap(); // replaced
        assert!(sync_mirror(&s, &m).is_err());
        assert_eq!(std::fs::read_to_string(&m).unwrap(), "alpha\nbeta\ngamma\n", "the mirror still holds the original");
    }

    #[test]
    fn a_tampered_mirror_is_noticed_too() {
        let (_d, s, m) = setup();
        std::fs::write(&s, "alpha\nbeta\n").unwrap();
        sync_mirror(&s, &m).unwrap();
        std::fs::write(&m, "alpha\nXXXX\n").unwrap();
        std::fs::write(&s, "alpha\nbeta\ngamma\n").unwrap();
        assert!(sync_mirror(&s, &m).is_err());
    }
}

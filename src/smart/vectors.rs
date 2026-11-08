//! What each file was embedded to, kept on disk as an append-only log and in memory for scoring.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub const DIMENSIONS: usize = 256;
const RECORD_BYTES: usize = 8 + DIMENSIONS * 4;

/// A file that was looked at and scored against a search.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub key: u64,
    pub score: f32,
}

#[derive(Default)]
pub struct Vectors {
    keys: Vec<u64>,
    values: Vec<f32>,
    slots: HashMap<u64, usize>,
    log: Option<File>,
}

impl Vectors {
    /// What `path` holds. A record cut short by a crash is dropped.
    pub fn open(path: &Path) -> Vectors {
        let mut vectors = Vectors::default();
        if let Some(dir) = path.parent() { let _ = fs::create_dir_all(dir); }
        let mut bytes = Vec::new();
        if let Ok(mut file) = File::open(path) { let _ = file.read_to_end(&mut bytes); }
        let whole = bytes.len() - bytes.len() % RECORD_BYTES;
        for record in bytes[..whole].as_chunks::<RECORD_BYTES>().0 {
            let (key, rest) = record.split_at(8);
            let values: Vec<f32> = rest.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            vectors.remember(u64::from_le_bytes(key.try_into().unwrap_or_default()), &values);
        }
        if whole != bytes.len() && let Ok(file) = OpenOptions::new().write(true).open(path) { let _ = file.set_len(whole as u64); }
        vectors.log = OpenOptions::new().create(true).append(true).open(path).map_err(|e| eprintln!("search index unavailable: {e}")).ok();
        vectors
    }

    fn remember(&mut self, key: u64, values: &[f32]) {
        match self.slots.get(&key) {
            Some(slot) => self.values[slot * DIMENSIONS..(slot + 1) * DIMENSIONS].copy_from_slice(values),
            None => {
                self.slots.insert(key, self.keys.len());
                self.keys.push(key);
                self.values.extend_from_slice(values);
            }
        }
    }

    pub fn contains(&self, key: u64) -> bool {
        self.slots.contains_key(&key)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Records what `key` is. A file that couldn't be read goes in as all zeros: it scores nothing
    /// and isn't tried again.
    pub fn insert(&mut self, key: u64, values: &[f32]) {
        debug_assert_eq!(values.len(), DIMENSIONS);
        self.remember(key, values);
        let Some(log) = &mut self.log else { return };
        let mut record = Vec::with_capacity(RECORD_BYTES);
        record.extend_from_slice(&key.to_le_bytes());
        for v in values { record.extend_from_slice(&v.to_le_bytes()); }
        let _ = log.write_all(&record);
    }

    /// The best of everything for a unit-length `query`, at least `floor`, best first.
    pub fn nearest(&self, query: &[f32], floor: f32, most: usize) -> Vec<Hit> {
        let mut hits: Vec<Hit> = self.values.as_chunks::<DIMENSIONS>().0.iter().zip(&self.keys)
            .map(|(v, key)| Hit { key: *key, score: v.iter().zip(query).map(|(a, b)| a * b).sum() })
            .filter(|h| h.score >= floor)
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(most);
        hits
    }

    pub fn clear(&mut self, path: &Path) {
        self.keys.clear();
        self.values.clear();
        self.slots.clear();
        self.log = None;
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(axis: usize) -> Vec<f32> {
        let mut v = vec![0.0; DIMENSIONS];
        v[axis] = 1.0;
        v
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ab-vectors-{name}-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn what_was_stored_comes_back_after_a_restart() {
        let path = temp("restart");
        {
            let mut v = Vectors::open(&path);
            v.insert(7, &unit(0));
            v.insert(8, &unit(1));
        }
        let v = Vectors::open(&path);
        assert_eq!(v.len(), 2);
        assert!(v.contains(7));
        assert_eq!(v.nearest(&unit(1), 0.5, 10), vec![Hit { key: 8, score: 1.0 }]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_record_cut_short_by_a_crash_is_dropped_and_writing_carries_on_cleanly() {
        let path = temp("torn");
        {
            let mut v = Vectors::open(&path);
            v.insert(1, &unit(0));
        }
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[9; 100]).unwrap();
        drop(file);
        {
            let mut v = Vectors::open(&path);
            assert_eq!(v.len(), 1);
            v.insert(2, &unit(1));
        }
        let v = Vectors::open(&path);
        assert_eq!(v.len(), 2);
        assert!(v.contains(1) && v.contains(2));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn storing_a_key_again_replaces_it() {
        let path = temp("again");
        let mut v = Vectors::open(&path);
        v.insert(1, &unit(0));
        v.insert(1, &unit(1));
        assert_eq!(v.len(), 1);
        assert_eq!(v.nearest(&unit(1), 0.5, 10).len(), 1);
        assert!(v.nearest(&unit(0), 0.5, 10).is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_file_that_could_not_be_read_never_turns_up() {
        let path = temp("zeros");
        let mut v = Vectors::open(&path);
        v.insert(1, &vec![0.0; DIMENSIONS]);
        assert!(v.contains(1), "it isn't tried again");
        assert!(v.nearest(&unit(0), 0.1, 10).is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn hits_come_best_first_and_stop_at_the_limit() {
        let path = temp("order");
        let mut v = Vectors::open(&path);
        let mut near = unit(0);
        near[0] = 0.8;
        near[1] = 0.6;
        v.insert(1, &near);
        v.insert(2, &unit(0));
        v.insert(3, &unit(5));
        let hits = v.nearest(&unit(0), 0.5, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, 2);
        let _ = fs::remove_file(path);
    }
}

use crate::core::db::tree_db::TreeObjectChild;
use crate::error::OxenError;
use crate::model::{ContentHashable, NewCommit};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::Path;
use xxhash_rust::xxh3::{xxh3_128, Xxh3};

pub fn hash_buffer(buffer: &[u8]) -> String {
    let val = xxh3_128(buffer);
    format!("{val:x}")
}

pub fn hash_str<S: AsRef<str>>(buffer: S) -> String {
    let buffer = buffer.as_ref().as_bytes();
    hash_buffer(buffer)
}

pub fn hash_str_sha256<S: AsRef<str>>(str: S) -> String {
    let mut hasher = Sha256::new();
    hasher.update(str.as_ref().as_bytes());
    let result = hasher.finalize();
    format!("{result:x}")
}

pub fn hash_buffer_128bit(buffer: &[u8]) -> u128 {
    xxh3_128(buffer)
}

pub fn compute_commit_hash<E>(commit_data: &NewCommit, entries: &[E]) -> String
where
    E: ContentHashable + std::fmt::Debug,
{
    let mut commit_hasher = xxhash_rust::xxh3::Xxh3::new();
    log::debug!("Hashing {} entries", entries.len());
    for entry in entries.iter() {
        let hash = entry.content_hash();
        // log::debug!("Entry [{}] hash {}", i, hash);

        let input = hash.as_bytes();
        commit_hasher.update(input);
    }

    log::debug!("Hashing commit data {:?}", commit_data);
    let commit_str = format!("{commit_data:?}");
    commit_hasher.update(commit_str.as_bytes());

    let val = commit_hasher.digest();
    format!("{val:x}")
}

// Need to hash on both path and hash - otherwise, vnode with same content under two different path hashes
// (and many other examples) would overwrite node in objects dir since is hash-indexed
pub fn compute_children_hash(children: &Vec<TreeObjectChild>) -> String {
    let mut buffer = Vec::new();
    for child in children {
        let hash = child.hash();
        let path = child.path().to_str().unwrap();
        let hash_input = hash.as_bytes();
        let path_input = path.as_bytes();
        buffer.extend_from_slice(hash_input);
        buffer.extend_from_slice(path_input);
    }
    let val = hash_buffer_128bit(&buffer);
    format!("{val:x}")
}

pub fn hash_file_contents_with_retry(path: &Path) -> Result<String, OxenError> {
    // Not sure why some tests were failing....the file didn't get written fast enough
    // So added this method to retry a few times
    let mut timeout = 1;
    let mut retries = 0;
    let total_retries = 5;
    loop {
        match hash_file_contents(path) {
            Ok(hash) => return Ok(hash),
            Err(err) => {
                // sleep and try again
                retries += 1;
                // exponential backoff
                timeout *= 2;
                log::warn!("Error: sleeping {timeout}s failed to hash file {path:?}");
                std::thread::sleep(std::time::Duration::from_secs(timeout));
                if retries > total_retries {
                    return Err(err);
                }
            }
        }
    }
}

pub fn hash_file_contents(path: &Path) -> Result<String, OxenError> {
    // If file is < 1GB, one-shot hash for speed
    // If file is > 1GB, stream hash to avoid memory overage issues
    let file_size = std::fs::metadata(path)?.len();

    if file_size < 1_000_000_000 {
        hash_small_file_contents(path)
    } else {
        hash_large_file_contents(path)
    }
}

fn hash_small_file_contents(path: &Path) -> Result<String, OxenError> {
    match File::open(path) {
        Ok(file) => {
            let mut reader = BufReader::new(file);
            let mut buffer = Vec::new();
            match reader.read_to_end(&mut buffer) {
                Ok(_) => {
                    let result = hash_buffer(&buffer);
                    Ok(result)
                }
                Err(_) => {
                    eprintln!("Could not read file for hashing {path:?}");
                    Err(OxenError::basic_str("Could not read file for hashing"))
                }
            }
        }
        Err(err) => {
            let err =
                format!("util::hasher::hash_file_contents Could not open file {path:?} {err:?}");
            Err(OxenError::basic_str(err))
        }
    }
}

fn hash_large_file_contents(path: &Path) -> Result<String, OxenError> {
    let file = File::open(path).map_err(|err| {
        eprintln!("Could not open file {:?} due to {:?}", path, err);
        OxenError::basic_str(format!("Could not open file {:?} due to {:?}", path, err))
    })?;

    let mut reader = BufReader::new(file);
    let mut hasher = Xxh3::new();
    let mut buffer = [0; 4096];

    loop {
        let count = reader.read(&mut buffer).map_err(|_| {
            eprintln!("Could not read file for hashing {:?}", path);
            OxenError::basic_str("Could not read file for hashing")
        })?;

        if count == 0 {
            break;
        }

        hasher.update(&buffer[..count]);
    }

    let result = hasher.digest128();
    Ok(format!("{result:x}"))
}

pub fn hash_path<P: AsRef<Path>>(path: P) -> String {
    hash_str(path.as_ref().to_str().unwrap())
}

pub fn hash_pathbuf(path: &Path) -> String {
    hash_str(path.to_str().unwrap())
}

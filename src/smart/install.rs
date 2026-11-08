//! Fetches what search by description runs on, once and only when asked: the model's weights and
//! ONNX Runtime. Each file is pinned to a revision and checked against its hash, and an
//! interrupted download picks up where it stopped.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

const MODEL_URL: &str = "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/daa72c51243991dfcaf9f9137d2c573d8f7790c0";
/// Microsoft's build, the one that has WebGPU inside it. npm is where it's published whole.
const RUNTIME_URL: &str = "https://registry.npmjs.org/onnxruntime-node/-/onnxruntime-node-1.30.0.tgz";
const RUNTIME_ARCHIVE: &str = "onnxruntime-node-1.30.0.tgz";
const RUNTIME_FOLDER_IN_ARCHIVE: &str = "package/bin/napi-v6/win32/x64/";
const RUNTIME_FILES: [&str; 3] = ["onnxruntime.dll", "dxcompiler.dll", "dxil.dll"];

const CHUNK: usize = 1 << 20;
/// A long stall ends the attempt rather than the download.
const BODY_TIMEOUT: Duration = Duration::from_secs(600);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Attempts in a row that may fail without a byte arriving.
const FRUITLESS_ATTEMPTS: u32 = 4;

/// Written last, so its presence means every other file is whole.
const DONE_MARKER: &str = "installed";
/// A DLL in use can't be deleted, so removal finishes at the next launch.
const REMOVE_MARKER: &str = "remove";

struct Part {
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

const fn model(name: &'static str, url: &'static str, size: u64, sha256: &'static str) -> Part {
    Part { name, url, size, sha256 }
}

const PARTS: [Part; 6] = [
    model("model_quantized.onnx", "onnx/model_quantized.onnx", 495_165, "d06edd601f851c633a2519304cbeb8dc6170d7ceb61b436625c17fb9b6e74953"),
    model("model_quantized.onnx_data", "onnx/model_quantized.onnx_data", 313_724_928, "278a7ff1248c3618e4bd11a607fc54f7bdc7778854230f3956d3f86bd9db4f3b"),
    model("vision_encoder_quantized.onnx", "onnx/vision_encoder_quantized.onnx", 162_495, "bb0de2df53a2448a32dc7908a187c168c8afd514d4d6f674f7f46024875fa4e3"),
    model("vision_encoder_quantized.onnx_data", "onnx/vision_encoder_quantized.onnx_data", 195_228_672, "3dabd69c0a36e9a8771ad82030dde74daa5a0e02b7047a5d3f3382b1137bab89"),
    model("tokenizer.json", "tokenizer.json", 32_170_510, "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4"),
    Part { name: RUNTIME_ARCHIVE, url: RUNTIME_URL, size: 113_507_888, sha256: "6e3390d6b783e7be946fad629292799da28d0b42f84856e50d2c1b0383291e75" },
];

/// Everything a fresh install transfers.
pub const DOWNLOAD_BYTES: u64 = {
    let (mut total, mut i) = (0, 0);
    while i < PARTS.len() {
        total += PARTS[i].size;
        i += 1;
    }
    total
};

pub fn is_installed(dir: &Path) -> bool {
    dir.join(DONE_MARKER).is_file()
}

pub fn runtime_library(dir: &Path) -> PathBuf {
    dir.join("runtime").join("onnxruntime.dll")
}

/// Finishes a removal the last run couldn't complete.
pub fn finish_pending_removal(dir: &Path) {
    if dir.join(REMOVE_MARKER).is_file() { let _ = fs::remove_dir_all(dir); }
}

/// Takes the install away. What's still in use goes at the next launch.
pub fn remove(dir: &Path) {
    let _ = fs::write(dir.join(REMOVE_MARKER), b"");
    let _ = fs::remove_file(dir.join(DONE_MARKER));
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten().filter(|e| e.file_name() != REMOVE_MARKER) {
            let path = entry.path();
            let _ = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
        }
    }
    finish_pending_removal(dir);
}

fn describe_io(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::StorageFull => "the disk is full".to_owned(),
        std::io::ErrorKind::PermissionDenied => "access was denied".to_owned(),
        _ => e.to_string(),
    }
}

/// Fetches whatever is missing, counting bytes in `done`. `Ok(false)` means `stop` ended it.
pub fn run(dir: &Path, done: &AtomicU64, stop: &AtomicBool) -> Result<bool, String> {
    fs::create_dir_all(dir).map_err(|e| format!("Couldn't create the model folder: {}", describe_io(&e)))?;
    let _ = fs::remove_file(dir.join(REMOVE_MARKER));
    done.store(0, Ordering::Relaxed);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(CONNECT_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .user_agent("asset-browser")
        .build()
        .into();
    let runtime_ready = RUNTIME_FILES.iter().all(|f| dir.join("runtime").join(f).is_file());
    for part in &PARTS {
        if part.name == RUNTIME_ARCHIVE && runtime_ready {
            done.fetch_add(part.size, Ordering::Relaxed);
            continue;
        }
        if !fetch(&agent, dir, part, done, stop)? { return Ok(false) }
    }
    if !runtime_ready { unpack_runtime(dir)?; }
    fs::write(dir.join(DONE_MARKER), b"").map_err(|e| format!("Couldn't finish installing: {}", describe_io(&e)))?;
    Ok(true)
}

fn fetch(agent: &ureq::Agent, dir: &Path, part: &Part, done: &AtomicU64, stop: &AtomicBool) -> Result<bool, String> {
    let target = dir.join(part.name);
    if target.is_file() {
        done.fetch_add(part.size, Ordering::Relaxed);
        return Ok(true);
    }
    let partial = dir.join(format!("{}.part", part.name));
    let before = done.load(Ordering::Relaxed);
    let mut fruitless = 0;
    loop {
        let had = fs::metadata(&partial).map_or(0, |m| m.len());
        done.store(before + had, Ordering::Relaxed);
        match attempt(agent, part, &partial, had, before, done, stop) {
            Ok(true) => break,
            Ok(false) => return Ok(false),
            Err(e) => {
                let progressed = fs::metadata(&partial).map_or(0, |m| m.len()) > had;
                fruitless = if progressed { 0 } else { fruitless + 1 };
                if fruitless >= FRUITLESS_ATTEMPTS { return Err(format!("Couldn't download {}: {e}", part.name)) }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    if hash_of(&partial).map_err(|e| format!("Couldn't check {}: {}", part.name, describe_io(&e)))? != part.sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!("{} arrived damaged. Try again.", part.name));
    }
    fs::rename(&partial, &target).map_err(|e| format!("Couldn't save {}: {}", part.name, describe_io(&e)))?;
    Ok(true)
}

/// One go at the rest of a file. `Ok(false)` means `stop` ended it.
fn attempt(agent: &ureq::Agent, part: &Part, partial: &Path, had: u64, before: u64, done: &AtomicU64, stop: &AtomicBool) -> Result<bool, String> {
    if had == part.size { return Ok(true) }
    let url = if part.url.starts_with("https://") { part.url.to_owned() } else { format!("{MODEL_URL}/{}", part.url) };
    let mut request = agent.get(&url);
    if had > 0 { request = request.header("Range", format!("bytes={had}-")); }
    let response = request.call().map_err(|e| e.to_string())?;
    let resumed = response.status() == 206;
    let mut file = OpenOptions::new().create(true).write(true).truncate(false).open(partial).map_err(|e| describe_io(&e))?;
    let from = if resumed { had } else { 0 };
    file.set_len(from).map_err(|e| describe_io(&e))?;
    file.seek(SeekFrom::Start(from)).map_err(|e| describe_io(&e))?;
    let mut written = from;
    done.store(before + written, Ordering::Relaxed);
    let mut body = response.into_body().into_with_config().limit(u64::MAX).reader();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if stop.load(Ordering::Relaxed) { return Ok(false) }
        let n = body.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 { break }
        file.write_all(&buffer[..n]).map_err(|e| describe_io(&e))?;
        written += n as u64;
        done.store(before + written, Ordering::Relaxed);
    }
    if written == part.size { Ok(true) } else { Err("the connection ended early".to_owned()) }
}

fn hash_of(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Takes the Windows runtime out of the archive and drops the rest.
fn unpack_runtime(dir: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("Couldn't unpack the runtime: {}", describe_io(&e));
    let archive = dir.join(RUNTIME_ARCHIVE);
    let target = dir.join("runtime");
    fs::create_dir_all(&target).map_err(fail)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(File::open(&archive).map_err(fail)?));
    let mut found = 0;
    for entry in tar.entries().map_err(fail)? {
        let mut entry = entry.map_err(fail)?;
        let path = entry.path().map_err(fail)?.to_string_lossy().replace('\\', "/");
        let Some(name) = path.strip_prefix(RUNTIME_FOLDER_IN_ARCHIVE) else { continue };
        let Some(file) = RUNTIME_FILES.iter().find(|f| **f == name) else { continue };
        let partial = target.join(format!("{file}.part"));
        let mut out = File::create(&partial).map_err(fail)?;
        std::io::copy(&mut entry, &mut out).map_err(fail)?;
        drop(out);
        fs::rename(&partial, target.join(file)).map_err(fail)?;
        found += 1;
    }
    if found != RUNTIME_FILES.len() { return Err("The runtime package was missing files.".to_owned()) }
    let _ = fs::remove_file(archive);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removal_finished_late_takes_the_whole_folder() {
        let dir = std::env::temp_dir().join(format!("ab-smart-remove-{}", std::process::id()));
        fs::create_dir_all(dir.join("runtime")).unwrap();
        fs::write(dir.join("runtime").join("onnxruntime.dll"), b"x").unwrap();
        fs::write(dir.join(DONE_MARKER), b"").unwrap();
        assert!(is_installed(&dir));
        remove(&dir);
        assert!(!is_installed(&dir));
        assert!(!dir.exists(), "nothing was in use, so nothing waits for the next launch");
    }

    #[test]
    fn hashes_are_written_the_way_the_pins_are() {
        let path = std::env::temp_dir().join(format!("ab-smart-hash-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        assert_eq!(hash_of(&path).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn the_advertised_download_covers_every_part() {
        assert_eq!(DOWNLOAD_BYTES, 655_289_658);
    }
}

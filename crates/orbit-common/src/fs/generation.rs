//! Cross-process admission for one executable generation per authority.
//!
//! OS locks, not process discovery or expiring leases, define liveness. Never
//! unlink these files: replacing a locked inode would create a second authority.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use fs2::FileExt;
use sha2::{Digest, Sha256};

use crate::OrbitError;

/// Executable admission protocol required of replacement candidates.
pub const GENERATION_CONTRACT: &str = "executable-generation-v1";

/// A shared generation pin. Retain until all operations and replies finish.
pub struct GenerationGuard {
    _file: File,
}

impl Drop for GenerationGuard {
    fn drop(&mut self) {
        // flock belongs to the open description. Explicit release prevents a
        // concurrently forked, pre-exec child from extending this pin after
        // the owning process has finished its operations. Independent pins
        // use independent opens and remain held.
        let _ = FileExt::unlock(&self._file);
    }
}

/// Exclusive admission, before installing a candidate or mutating resources.
pub struct GenerationUpdate {
    admission: File,
    generation: File,
}

fn refusal(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!(
        "upgrade admission refused: {detail}; leave the installation and stores unchanged. \
         Quiesce the existing Orbit processes through their owning clients, then retry. \
         Do not delete admission files or replay a mutation whose reply was lost"
    ))
}

fn open(root: &Path, name: &str) -> Result<File, OrbitError> {
    std::fs::create_dir_all(root).map_err(refusal)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(name))
        // An existing same-generation reader can participate on a read-only
        // mount. flock needs a descriptor, not permission to rewrite bytes.
        .or_else(|error| File::open(root.join(name)).map_err(|_| error))
        .map_err(refusal)
}

fn admission(root: &Path) -> Result<File, OrbitError> {
    let file = open(root, ".generation-admission.lock")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(refusal(e)),
        }
    }
}

/// SHA-256 of the executable bytes, including checkout builds with equal versions.
pub fn executable_generation(path: &Path) -> Result<String, OrbitError> {
    hash_executable(File::open(path).map_err(refusal)?)
}

fn hash_executable(mut file: File) -> Result<String, OrbitError> {
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(refusal)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Digest the running inode, even after its installed path has been replaced.
pub fn process_generation() -> Result<&'static str, OrbitError> {
    static DIGEST: OnceLock<Result<String, String>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let path = Ok::<_, std::io::Error>(PathBuf::from("/proc/self/exe"));
            #[cfg(not(target_os = "linux"))]
            let path = std::env::current_exe();
            let result = (|| {
                let mut file = File::open(path.map_err(refusal)?).map_err(refusal)?;
                verify_running_image(&mut file)?;
                hash_executable(file)
            })();
            result.map_err(|e: OrbitError| e.to_string())
        })
        .as_deref()
        .map_err(refusal)
}

fn read_generation(file: &mut File) -> Result<String, OrbitError> {
    file.seek(SeekFrom::Start(0)).map_err(refusal)?;
    let mut record = String::new();
    file.take(128)
        .read_to_string(&mut record)
        .map_err(refusal)?;
    if record.is_empty() {
        return Ok(record);
    }
    let digest = record.strip_prefix("1:").and_then(|s| s.strip_suffix('\n'));
    match digest {
        Some(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(s.into()),
        _ => Err(refusal("invalid generation record")),
    }
}

impl GenerationGuard {
    /// Pin this process before runtime bootstrap, for the whole process lifetime.
    pub fn for_process(root: &Path) -> Result<Self, OrbitError> {
        Self::acquire(root, process_generation()?)
    }

    /// Pin an exact digest. The admission mutex makes lock conversion atomic
    /// with respect to other participants (flock conversion alone is not atomic).
    pub fn acquire(root: &Path, digest: &str) -> Result<Self, OrbitError> {
        let admission = admission(root)?;
        let mut generation = open(root, ".generation.lock")?;
        FileExt::try_lock_shared(&generation).map_err(refusal)?;
        if read_generation(&mut generation)? == digest {
            return Ok(Self { _file: generation });
        }
        FileExt::unlock(&generation).map_err(refusal)?;
        FileExt::try_lock_exclusive(&generation)
            .map_err(|_| refusal("another executable generation is still running"))?;
        GenerationUpdate {
            admission,
            generation,
        }
        .pin(digest)
    }
}

impl GenerationUpdate {
    /// Refuse before installation/resource/store writes if any process is live.
    pub fn acquire(root: &Path) -> Result<Self, OrbitError> {
        let admission = admission(root)?;
        let mut generation = open(root, ".generation.lock")?;
        FileExt::try_lock_exclusive(&generation)
            .map_err(|_| refusal("Orbit clients or commands are still running"))?;
        read_generation(&mut generation)?;
        Ok(Self {
            admission,
            generation,
        })
    }

    /// After replacement, pin the candidate through convergence. Old pinned
    /// executables cannot enter between the updater and its candidate children.
    pub fn pin(mut self, digest: &str) -> Result<GenerationGuard, OrbitError> {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(refusal("invalid executable digest"));
        }
        self.generation.seek(SeekFrom::Start(0)).map_err(refusal)?;
        self.generation
            .write_all(format!("1:{digest}\n").as_bytes())
            .map_err(refusal)?;
        self.generation.set_len(67).map_err(refusal)?;
        self.generation.sync_all().map_err(refusal)?;
        FileExt::lock_shared(&self.generation).map_err(refusal)?;
        drop(self.admission);
        Ok(GenerationGuard {
            _file: self.generation,
        })
    }
}

// Linux opens the running inode directly. Other supported systems must verify
// that current_exe still names the loaded image, before hashing that descriptor.
#[cfg(not(target_os = "macos"))]
fn verify_running_image(_file: &mut File) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_running_image(file: &mut File) -> Result<(), OrbitError> {
    unsafe extern "C" {
        fn _dyld_get_image_header(index: u32) -> *const u8;
    }
    // SAFETY: dyld's main-image header and load commands remain mapped for
    // this process lifetime. Lengths are bounded before constructing slices.
    let loaded = unsafe { _dyld_get_image_header(0) };
    if loaded.is_null() {
        return Err(refusal("cannot identify the running Mach-O image"));
    }
    let header = unsafe { std::slice::from_raw_parts(loaded, 32) };
    let length = macho_commands_length(header)?;
    let commands = unsafe { std::slice::from_raw_parts(loaded.add(32), length) };
    let running_uuid = macho_uuid(commands)?;
    let mut disk_header = [0u8; 32];
    file.read_exact(&mut disk_header).map_err(refusal)?;
    let mut disk_commands = vec![0; macho_commands_length(&disk_header)?];
    file.read_exact(&mut disk_commands).map_err(refusal)?;
    if running_uuid != macho_uuid(&disk_commands)? {
        return Err(refusal(
            "the installed executable no longer names this running image",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(refusal)?;
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn macho_commands_length(header: &[u8]) -> Result<usize, OrbitError> {
    if header.len() != 32 || header[..4] != [0xcf, 0xfa, 0xed, 0xfe] {
        return Err(refusal("expected a native 64-bit Mach-O executable"));
    }
    let size = u32::from_le_bytes([header[20], header[21], header[22], header[23]]) as usize;
    if size > 1024 * 1024 {
        return Err(refusal("invalid Mach-O load commands"));
    }
    Ok(size)
}

#[cfg(any(target_os = "macos", test))]
fn macho_uuid(mut commands: &[u8]) -> Result<[u8; 16], OrbitError> {
    while commands.len() >= 8 {
        let kind = u32::from_le_bytes([commands[0], commands[1], commands[2], commands[3]]);
        let size =
            u32::from_le_bytes([commands[4], commands[5], commands[6], commands[7]]) as usize;
        if size < 8 || size > commands.len() {
            break;
        }
        if kind == 0x1b && size == 24 {
            let mut uuid = [0; 16];
            uuid.copy_from_slice(&commands[8..24]);
            return Ok(uuid);
        }
        commands = &commands[size..];
    }
    Err(refusal("missing or invalid Mach-O image UUID"))
}

#[cfg(test)]
#[path = "tests/generation_image.rs"]
mod image_tests;

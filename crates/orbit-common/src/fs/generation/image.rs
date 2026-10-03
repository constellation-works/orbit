//! Digesting the running executable image, and verifying it is the installed one.

use std::fs::File;
use std::io::Read;
#[cfg(target_os = "macos")]
use std::io::{Seek, SeekFrom};
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::image_digest;
use super::paths::{IMAGE_DIGEST_CACHE, validated_generation_record_path};
use super::refusal::refusal;
use crate::OrbitError;

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
    process_digest(None, false)
}

/// [`process_generation`], answered from the digest cache under `cache_root`
/// for as long as the running image's file identity is unchanged (see
/// [`image_digest`]). A fresh digest is written back only when `record` is
/// set; a read-only participant must leave the root byte-identical. Whichever
/// call runs first fixes the process's digest; later calls return it.
pub(super) fn process_digest(
    cache_root: Option<&Path>,
    record: bool,
) -> Result<&'static str, OrbitError> {
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
                let cache = cache_root.and_then(|root| {
                    validated_generation_record_path(root, IMAGE_DIGEST_CACHE).ok()
                });
                image_digest::digest_with_cache(
                    cache.as_deref(),
                    record,
                    SystemTime::now(),
                    file,
                    hash_executable,
                )
            })();
            result.map_err(|e: OrbitError| e.to_string())
        })
        .as_deref()
        .map_err(refusal)
}

// Linux opens the running inode directly. Other supported systems must verify
// that current_exe still names the loaded image, before hashing that descriptor.
#[cfg(not(target_os = "macos"))]
pub(super) fn verify_running_image(_file: &mut File) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn verify_running_image(file: &mut File) -> Result<(), OrbitError> {
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
pub(super) fn macho_commands_length(header: &[u8]) -> Result<usize, OrbitError> {
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
pub(super) fn macho_uuid(mut commands: &[u8]) -> Result<[u8; 16], OrbitError> {
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

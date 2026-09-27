use super::*;
use std::ffi::CString;
use std::io::{BufWriter, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use unrar_sys as rar;

pub(super) fn library_lock(checkpoint: &dyn Fn() -> Result<(), String>) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    // The bundled static library shares error state between archive handles.
    // A callback abort must not race a different handle's native operation.
    static LIBRARY: Mutex<()> = Mutex::new(());
    loop {
        checkpoint()?;
        match LIBRARY.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

struct CallbackState<'a> {
    checkpoint: &'a dyn Fn() -> Result<(), String>,
    password: &'a [u8],
    processed: &'a std::sync::atomic::AtomicU64,
    output: Option<BufWriter<std::fs::File>>,
    last_check: Instant,
    error: Option<String>,
}

extern "C" fn callback(message: rar::UINT, user: rar::LPARAM, data: rar::LPARAM, length: rar::LPARAM) -> i32 {
    if user == 0 { return -1; }
    let state = unsafe { &mut *(user as *mut CallbackState<'_>) };
    // PROCESSDATA can arrive for every small decompression buffer. Keep pause and
    // cancellation responsive without locking the job store for every buffer.
    if message != rar::UCM_PROCESSDATA || state.last_check.elapsed() >= Duration::from_millis(50) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (state.checkpoint)()));
        match result {
            Ok(Ok(())) => {},
            Ok(Err(error)) => { state.error = Some(error); return -1; },
            Err(_) => { state.error = Some("Archive control failed".into()); return -1; },
        }
        state.last_check = Instant::now();
    }
    if message == rar::UCM_PROCESSDATA && length > 0 {
        if data == 0 { state.error = Some("RAR returned an invalid data buffer".into()); return -1; }
        if let Some(output) = state.output.as_mut() {
            let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length as usize) };
            if let Err(error) = output.write_all(bytes) {
                state.error = Some(format!("RAR extraction: could not write extracted data: {}", redact(error)));
                return -1;
            }
        }
        state.processed.fetch_add(length as u64, Ordering::Relaxed);
    }
    if message == rar::UCM_NEEDPASSWORD || message == rar::UCM_NEEDPASSWORDW {
        if data == 0 || length <= 0 { return -1; }
        if message == rar::UCM_NEEDPASSWORD {
            if state.password.len() + 1 > length as usize { return -1; }
            unsafe { std::ptr::copy_nonoverlapping(state.password.as_ptr(), data as *mut u8, state.password.len()); *((data as *mut u8).add(state.password.len())) = 0; }
        } else {
            let wide: Vec<u16> = String::from_utf8_lossy(state.password).encode_utf16().chain(Some(0)).collect();
            if wide.len() > length as usize { return -1; }
            unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), data as *mut u16, wide.len()); }
        }
    }
    if (message == rar::UCM_CHANGEVOLUME || message == rar::UCM_CHANGEVOLUMEW) && length == rar::RAR_VOL_ASK {
        state.error = Some("RAR volume is missing or cannot be read. Keep every archive part together and retry.".into());
        return -1;
    }
    1
}

pub(super) fn extract(path: &Path, dest: &Path, password: &[u8], checkpoint: &dyn Fn() -> Result<(), String>) -> Result<u64, String> {
    extract_measured(path, dest, password, checkpoint, &std::sync::atomic::AtomicU64::new(0))
}

fn rar_error(code: i32, phase: &str) -> String {
    let detail = match code {
        rar::ERAR_MISSING_PASSWORD | rar::ERAR_BAD_PASSWORD => return format!("RAR password error {code}"),
        rar::ERAR_BAD_DATA => "checksum failed: archive data is damaged or the password is incorrect",
        rar::ERAR_EREAD => "could not read an archive volume; check the drive and all parts",
        rar::ERAR_EWRITE => "could not write extracted data; check free space and the destination drive",
        rar::ERAR_ECREATE => "could not create an output file; check the destination drive and permissions",
        rar::ERAR_NO_MEMORY => "not enough memory to decompress this archive",
        rar::ERAR_EOPEN => "could not open an archive volume",
        rar::ERAR_BAD_ARCHIVE | rar::ERAR_UNKNOWN_FORMAT => "invalid or unsupported archive",
        _ => "archive operation failed",
    };
    format!("RAR {phase}: {detail} (code {code})")
}

pub(super) fn extract_measured(path: &Path, dest: &Path, password: &[u8], checkpoint: &dyn Fn() -> Result<(), String>, processed: &std::sync::atomic::AtomicU64) -> Result<u64, String> {
    let _library = library_lock(checkpoint)?;
    checkpoint()?;
    std::fs::create_dir_all(dest).map_err(redact)?;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut callback_state = CallbackState { checkpoint, password, processed, output: None, last_check: Instant::now(), error: None };
    // The C ABI defines zero/null as the default for every unused field.
    let mut open: rar::OpenArchiveDataEx = unsafe { std::mem::zeroed() };
    open.archive_name_w = name.as_ptr(); open.open_mode = rar::RAR_OM_EXTRACT;
    open.callback = Some(callback); open.user_data = &mut callback_state as *mut _ as rar::LPARAM;
    let handle = unsafe { rar::RAROpenArchiveEx(&mut open) };
    if handle.is_null() { return Err(callback_state.error.unwrap_or_else(|| rar_error(open.open_result as i32, "open"))); }
    struct Open(*const rar::Handle);
    impl Drop for Open { fn drop(&mut self) { unsafe { rar::RARCloseArchive(self.0); } } }
    let handle = Open(handle);
    let password_c = CString::new(password).map_err(redact)?;
    unsafe { rar::RARSetPassword(handle.0, password_c.as_ptr()); }
    let mut files = 0;
    loop {
        checkpoint()?;
        let mut header = rar::HeaderDataEx::default();
        let result = unsafe { rar::RARReadHeaderEx(handle.0, &mut header) };
        if let Some(error) = callback_state.error.take() { return Err(error); }
        if result == rar::ERAR_END_ARCHIVE { break; }
        if result != 0 { return Err(rar_error(result, "header")); }
        let end = header.filename_w.iter().position(|c| *c == 0).unwrap_or(header.filename_w.len());
        let relative = PathBuf::from(std::ffi::OsString::from_wide(&header.filename_w[..end]));
        if !safe_extraction_path(&relative) || header.redir_type != 0 { return Err("RAR contains an unsafe or linked path".into()); }
        let output = dest.join(relative);
        let directory = header.flags & rar::RHDF_DIRECTORY != 0;
        if directory { std::fs::create_dir_all(&output).map_err(redact)?; }
        else {
            storage::guard_bytes(dest, ((header.unp_size_high as u64) << 32) | header.unp_size as u64, "extraction")?;
            std::fs::create_dir_all(output.parent().ok_or("Archive output has no parent")?).map_err(redact)?;
            callback_state.output = Some(BufWriter::with_capacity(1024 * 1024,
                std::fs::File::create(&output).map_err(|error| format!("RAR extraction: could not create output file: {}", redact(error)))?));
        }
        // TEST still decompresses and verifies every checksum, delivering data
        // through PROCESSDATA. Rust owns the writer so cancellation closes it;
        // UnRAR's native output handle can otherwise survive a callback abort.
        let result = unsafe { rar::RARProcessFileW(handle.0, if directory { rar::RAR_SKIP } else { rar::RAR_TEST }, std::ptr::null(), std::ptr::null()) };
        if let Some(mut output) = callback_state.output.take() {
            if callback_state.error.is_none() && result == 0 {
                output.flush().map_err(|error| format!("RAR extraction: could not write extracted data: {}", redact(error)))?;
            }
        }
        if let Some(error) = callback_state.error.take() { return Err(error); }
        if result != 0 { return Err(rar_error(result, "extraction")); }
        if !directory { files += 1; }
    }
    if files == 0 { return Err("Archive produced no files; check that the first volume is present".into()); }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn io_and_crc_errors_are_not_password_retries() {
        for code in [rar::ERAR_EWRITE, rar::ERAR_EREAD, rar::ERAR_BAD_DATA, rar::ERAR_ECREATE] {
            assert!(!rar_error(code, "extraction").starts_with("RAR password error"));
        }
        assert!(rar_error(rar::ERAR_BAD_PASSWORD, "extraction").starts_with("RAR password error"));
    }
    #[test]
    #[ignore = "Requires the generated 256 MiB RAR fixture under Build-Output"]
    fn real_rar_counts_decompressed_bytes_and_cancels_mid_file() {
        use std::sync::atomic::AtomicU64;
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/extraction-0.2.11");
        let source = fixture.join("progress.rar");
        let output = fixture.join(format!("test_{}", Uuid::new_v4()));
        let measured = AtomicU64::new(0);
        let observations = Mutex::new(Vec::new());
        let control = || { observations.lock().unwrap().push((measured.load(Ordering::Relaxed), std::fs::metadata(output.join("payload.bin")).map(|m| m.len()).unwrap_or(0))); Ok(()) };
        assert_eq!(extract_measured(&source, &output, b"", &control, &measured).unwrap(), 1);
        assert_eq!(measured.load(Ordering::Relaxed), 256 * 1024 * 1024);
        let digest = |path: &Path| {
            use std::io::Read;
            use sha2::Digest;
            let mut file = std::fs::File::open(path).unwrap(); let mut hash = sha2::Sha256::new(); let mut buffer = vec![0u8; 1024 * 1024];
            loop { let n = file.read(&mut buffer).unwrap(); if n == 0 { break; } hash.update(&buffer[..n]); }
            hash.finalize()
        };
        assert_eq!(digest(&output.join("payload.bin")), digest(&fixture.join("payload.bin")));
        eprintln!("Real callback/file-size samples: {:?}", observations.lock().unwrap());
        assert!(observations.lock().unwrap().iter().all(|(processed, file_size)| file_size <= processed));
        measured.store(0, Ordering::Relaxed);
        let cancel_output = fixture.join(format!("test_cancel_{}", Uuid::new_v4()));
        let control = || if measured.load(Ordering::Relaxed) > 0 { Err("cancelled".into()) } else { Ok(()) };
        assert_eq!(extract_measured(&source, &cancel_output, b"", &control, &measured).unwrap_err(), "cancelled");
        assert!(measured.load(Ordering::Relaxed) > 0 && measured.load(Ordering::Relaxed) < 256 * 1024 * 1024);
        assert!(source.is_file());
        std::fs::remove_dir_all(output).unwrap(); std::fs::remove_dir_all(cancel_output).unwrap();
    }
    #[test]
    #[ignore = "Requires the generated encrypted multipart RAR fixture under Build-Output"]
    fn multipart_rar_uses_password_fallback_and_rejects_missing_volume() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/extraction-0.2.11");
        let source = fixture.join("encrypted.part0001.rar");
        let output = fixture.join(format!("test_multi_{}", Uuid::new_v4()));
        let progress = Arc::new(|_: u64, _: u64, _: f64| {});
        extract_rar_builtin(&source, &output, None, progress.clone(), Arc::new(|| Ok(()))).unwrap();
        assert_eq!(std::fs::metadata(output.join("payload.bin")).unwrap().len(), 256 * 1024 * 1024);
        assert!(source.exists()); std::fs::remove_dir_all(output).unwrap();
        let broken = fixture.join(format!("test_missing_{}", Uuid::new_v4())); std::fs::create_dir_all(&broken).unwrap();
        std::fs::copy(&source, broken.join("encrypted.part0001.rar")).unwrap();
        let error = extract_rar_builtin(&broken.join("encrypted.part0001.rar"), &broken.join("out"), Some("[DLPSGAME.COM]"), progress, Arc::new(|| Ok(()))).unwrap_err();
        assert!(error.contains("volume is missing"), "{error}");
        std::fs::remove_dir_all(broken).unwrap();
    }
}

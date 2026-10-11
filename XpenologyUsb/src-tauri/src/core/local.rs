use super::{
    failure::Failure,
    model::DiskInfo,
    pipeline::{Cancel, ProgressEvent, ProgressReporter},
    progress::Stage,
    runner::{self, Io, RunConfig, RunSummary},
    safety,
};
use crate::device::RawWriter;
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_IMAGE: u64 = 16 * 1024 * 1024 * 1024;

/// Decompress and validate the complete image before any destructive device access.
pub fn prepare(path: &Path, io: &dyn Io, cancel: &dyn Cancel) -> Result<File, Failure> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // Other readers are allowed; writers and deletion are blocked.
    }
    let mut file = options
        .open(path)
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    let size = file
        .metadata()
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?
        .len();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut source: Box<dyn Read> = if name.ends_with(".img") {
        if size == 0 || size > MAX_IMAGE {
            return Err(Failure::new(
                "image_invalid",
                "Image must be between 1 byte and 16 GiB",
            ));
        }
        Box::new(file)
    } else if name.ends_with(".img.gz") || name.ends_with(".img.zip") || name.ends_with(".zip") {
        if size == 0 || size > 2 * 1024 * 1024 * 1024 {
            return Err(Failure::new(
                "image_invalid",
                "Compressed image exceeds 2 GiB",
            ));
        }
        let mut bytes = Vec::new();
        let mut block = vec![0; 256 * 1024];
        loop {
            if cancel.is_canceled() {
                return Err(Failure::new("canceled", ""));
            }
            let n = file
                .read(&mut block)
                .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
            if n == 0 {
                break;
            }
            if bytes.len() as u64 + n as u64 > 2 * 1024 * 1024 * 1024 {
                return Err(Failure::new(
                    "image_invalid",
                    "Compressed image exceeds 2 GiB",
                ));
            }
            bytes.extend_from_slice(&block[..n]);
        }
        io.open_decompressed(bytes, &name)
            .map_err(|e| Failure::new("image_invalid", e))?
            .0
    } else {
        return Err(Failure::new(
            "image_invalid",
            "Select .img, .img.gz or .zip",
        ));
    };

    let mut snapshot =
        tempfile::tempfile().map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    let mut buffer = vec![0; 256 * 1024];
    let mut total = 0;
    loop {
        if cancel.is_canceled() {
            return Err(Failure::new("canceled", ""));
        }
        let n = source
            .read(&mut buffer)
            .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_IMAGE {
            return Err(Failure::new("image_invalid", "Image exceeds 16 GiB"));
        }
        snapshot
            .write_all(&buffer[..n])
            .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    }
    validate(&mut snapshot)?;
    Ok(snapshot)
}

fn validate(file: &mut File) -> Result<(), Failure> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    let mut header = [0; 512];
    file.read_exact(&mut header)
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    if header[510..512] != [0x55, 0xaa] {
        return Err(Failure::new("image_invalid", "Missing disk boot signature"));
    }
    file.rewind()
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run<F: FnMut(ProgressEvent)>(
    mut snapshot: File,
    name: &str,
    verify: bool,
    disk: &DiskInfo,
    protected: &HashSet<u32>,
    writer: &dyn RawWriter,
    cancel: &dyn Cancel,
    emit: F,
) -> Result<RunSummary, Failure> {
    let size = snapshot
        .metadata()
        .map_err(|e| Failure::new("image_invalid", e.to_string()))?
        .len();
    safety::can_write(disk, protected, size, None)
        .map_err(|r| Failure::new("rejected", format!("{r:?}")))?;
    if cancel.is_canceled() {
        return Err(Failure::new("canceled", ""));
    }
    let mut rep = ProgressReporter::new(emit);
    rep.begin(Stage::Preparing, None);
    let session = writer.open(disk).map_err(Failure::from)?;
    let cfg = RunConfig {
        loader: super::loader::Loader::MShell,
        verify,
    };
    let bytes = runner::write_to_target(
        &cfg,
        disk,
        session,
        &mut snapshot,
        Some(size),
        cancel,
        &mut rep,
    )
    .map_err(|e| {
        let mut f = Failure::from(e);
        f.target_erased = true;
        f
    })?;
    rep.finish();
    Ok(RunSummary {
        loader: "Local".into(),
        tag: String::new(),
        asset_name: name.into(),
        bytes_written: bytes,
        verified: verify,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::pipeline::NeverCancel,
        device::{
            fake::{FakeEnumerator, FakeWriter},
            UsbEnumerator,
        },
    };
    #[test]
    fn invalid_image_fails_before_device_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.img");
        std::fs::write(&path, [0; 1024]).unwrap();
        let error =
            prepare(&path, &crate::io_real::RealIo::new().unwrap(), &NeverCancel).unwrap_err();
        assert_eq!(error.detail, "Missing disk boot signature");
        assert!(!error.target_erased);
    }
    #[test]
    fn valid_local_snapshot_uses_shared_verified_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("valid.img");
        let mut bytes = vec![0x5a; 4096];
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        std::fs::write(&path, &bytes).unwrap();
        let file = prepare(&path, &crate::io_real::RealIo::new().unwrap(), &NeverCancel).unwrap();
        let disk = FakeEnumerator::sample()
            .list_disks()
            .unwrap()
            .into_iter()
            .find(|d| d.number == 2)
            .unwrap();
        let writer = FakeWriter::new(16 * 1024 * 1024, 512);
        let result = run(
            file,
            "local.img",
            true,
            &disk,
            &HashSet::new(),
            &writer,
            &NeverCancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(result.bytes_written, 4096);
        assert!(result.verified);
    }

    #[test]
    fn gzip_is_fully_checked_before_a_snapshot_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.img.gz");
        let mut bytes = vec![0x5a; 4096];
        bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        std::fs::write(&path, &compressed).unwrap();
        let io = crate::io_real::RealIo::new().unwrap();
        let mut snapshot = prepare(&path, &io, &NeverCancel).unwrap();
        let mut actual = Vec::new();
        snapshot.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        std::fs::write(&path, &compressed[..compressed.len() - 4]).unwrap();
        let error = prepare(&path, &io, &NeverCancel).unwrap_err();
        assert_eq!(error.code, "image_invalid");
        assert!(!error.target_erased);
    }
}

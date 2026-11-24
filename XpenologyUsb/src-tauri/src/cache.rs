use crate::core::{loader::Release, runner::Io};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ENTRY: u64 = 2 * 1024 * 1024 * 1024;

pub struct CachedIo<T> {
    pub inner: T,
    pub directory: Option<PathBuf>,
}

fn digest(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn clear(directory: &Path) -> std::io::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for item in fs::read_dir(directory)? {
        let item = item?;
        if item.file_type()?.is_file()
            && matches!(
                item.path().extension().and_then(|s| s.to_str()),
                Some("bin" | "sha256")
            )
        {
            fs::remove_file(item.path())?;
        }
    }
    Ok(())
}

/// 새 항목 `incoming` 바이트가 들어갈 자리를 만든다. 오래된 것부터 지운다.
fn evict(dir: &Path, incoming: u64, budget: u64) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|s| s == "bin"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .map(|m| (m.modified().ok(), e.path(), m.len()))
        })
        .collect();
    entries.sort_by_key(|(time, _, _)| *time);
    let mut total: u64 = entries.iter().map(|(_, _, len)| len).sum();
    for (_, path, len) in entries {
        if total + incoming <= budget {
            break;
        }
        fs::remove_file(&path)?;
        let _ = fs::remove_file(path.with_extension("sha256"));
        total = total.saturating_sub(len);
    }
    Ok(())
}

impl<T: Io> Io for CachedIo<T> {
    fn fetch_releases(&self, url: &str) -> Result<Vec<Release>, String> {
        self.inner.fetch_releases(url)
    }
    fn fetch_text(&self, url: &str) -> Result<String, String> {
        self.inner.fetch_text(url)
    }
    fn open_decompressed(
        &self,
        data: Vec<u8>,
        name: &str,
    ) -> Result<(Box<dyn Read + Send>, Option<u64>), String> {
        self.inner.open_decompressed(data, name)
    }
    fn download(
        &self,
        url: &str,
        progress: &mut dyn FnMut(u64, Option<u64>),
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, String> {
        let key = digest(url.as_bytes());
        if let Some(dir) = &self.directory {
            let path = dir.join(format!("{key}.bin"));
            if fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_ENTRY) {
                let cached = (|| -> Option<Vec<u8>> {
                    let mut bytes = Vec::new();
                    let mut file = fs::File::open(&path).ok()?;
                    let mut block = vec![0; 256 * 1024];
                    loop {
                        if stop() {
                            return None;
                        }
                        let n = file.read(&mut block).ok()?;
                        if n == 0 {
                            break;
                        }
                        if bytes.len() as u64 + n as u64 > MAX_ENTRY {
                            return None;
                        }
                        bytes.extend_from_slice(&block[..n]);
                    }
                    let expected = fs::read_to_string(dir.join(format!("{key}.sha256"))).ok()?;
                    (digest(&bytes) == expected.trim()).then_some(bytes)
                })();
                if stop() {
                    return Err("Canceled".into());
                }
                if let Some(bytes) = cached {
                    // 정리 순서가 수정 시각을 보므로, 다시 쓴 항목은 시각을 갱신해
                    // 가장 최근에 쓴 것으로 만든다. 실패해도 캐시를 쓰는 데는 지장이 없다.
                    let _ = fs::File::options()
                        .write(true)
                        .open(&path)
                        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
                    progress(bytes.len() as u64, Some(bytes.len() as u64));
                    return Ok(bytes);
                }
                let _ = fs::remove_file(&path);
                let _ = fs::remove_file(dir.join(format!("{key}.sha256")));
            }
        }
        let bytes = self.inner.download(url, progress, stop)?;
        if stop() {
            return Err("Canceled".into());
        }
        if let Some(dir) = &self.directory {
            // Cache failures never prevent writing; publication is atomic per file.
            let _ = (|| -> std::io::Result<()> {
                fs::create_dir_all(dir)?;
                evict(dir, bytes.len() as u64, BUDGET)?;
                if bytes.len() as u64 > MAX_ENTRY {
                    return Ok(());
                }
                let mut data = tempfile::NamedTempFile::new_in(dir)?;
                data.write_all(&bytes)?;
                data.as_file().sync_all()?;
                let mut hash = tempfile::NamedTempFile::new_in(dir)?;
                hash.write_all(digest(&bytes).as_bytes())?;
                data.persist(dir.join(format!("{key}.bin")))
                    .map_err(|e| e.error)?;
                hash.persist(dir.join(format!("{key}.sha256")))
                    .map_err(|e| e.error)?;
                Ok(())
            })();
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake(AtomicUsize);
    impl Io for Fake {
        fn fetch_releases(&self, _: &str) -> Result<Vec<Release>, String> {
            Ok(vec![])
        }
        fn fetch_text(&self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn open_decompressed(
            &self,
            _: Vec<u8>,
            _: &str,
        ) -> Result<(Box<dyn Read + Send>, Option<u64>), String> {
            Err("unused".into())
        }
        fn download(
            &self,
            _: &str,
            _: &mut dyn FnMut(u64, Option<u64>),
            _: &dyn Fn() -> bool,
        ) -> Result<Vec<u8>, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(vec![1, 2, 3])
        }
    }
    #[test]
    fn corrupt_cache_is_downloaded_again() {
        let dir = tempfile::tempdir().unwrap();
        let io = CachedIo {
            inner: Fake(AtomicUsize::new(0)),
            directory: Some(dir.path().into()),
        };
        for _ in 0..2 {
            assert_eq!(
                io.download("url", &mut |_, _| {}, &|| false).unwrap(),
                vec![1, 2, 3]
            );
        }
        assert_eq!(io.inner.0.load(Ordering::Relaxed), 1);
        fs::write(dir.path().join(format!("{}.bin", digest(b"url"))), b"bad").unwrap();
        io.download("url", &mut |_, _| {}, &|| false).unwrap();
        assert_eq!(io.inner.0.load(Ordering::Relaxed), 2);
        clear(dir.path()).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    fn age(path: &Path, secs: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// 자리가 모자라면 **가장 오래 쓰지 않은** 이미지를 지운다.
    ///
    /// 정리 순서는 파일 수정 시각을 보는데, 캐시에서 다시 꺼내 써도 그 시각이
    /// 바뀌지 않았다. 그래서 자주 쓰는 로더가 먼저 받았다는 이유만으로 지워졌다.
    #[test]
    fn eviction_keeps_the_image_that_was_just_reused() {
        let dir = tempfile::tempdir().unwrap();
        let io = CachedIo {
            inner: Fake(AtomicUsize::new(0)),
            directory: Some(dir.path().into()),
        };
        let entry = |url: &str| dir.path().join(format!("{}.bin", digest(url.as_bytes())));
        io.download("old-but-reused", &mut |_, _| {}, &|| false)
            .unwrap();
        io.download("newer-unused", &mut |_, _| {}, &|| false)
            .unwrap();
        age(&entry("old-but-reused"), 200);
        age(&entry("newer-unused"), 100);

        io.download("old-but-reused", &mut |_, _| {}, &|| false)
            .unwrap();
        assert_eq!(io.inner.0.load(Ordering::Relaxed), 2, "캐시를 쓰지 않았다");

        // 두 항목(3 바이트씩)에 새 3 바이트가 들어오려면 하나를 비워야 한다.
        evict(dir.path(), 3, 6).unwrap();
        assert!(entry("old-but-reused").exists(), "방금 쓴 이미지를 지웠다");
        assert!(!entry("newer-unused").exists());
    }
}

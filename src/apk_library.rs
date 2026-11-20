use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ring::digest::{Context, SHA256};

use crate::util;

/// Converted APKs and imported APKs both survive application restarts.
pub fn scan() -> Result<Vec<PathBuf>, String> {
    scan_directories(&[util::apk_library_dir()])
}

fn scan_directories(directories: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for directory in directories {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "无法读取 APK 列表目录 {}：{error}",
                    directory.display()
                ));
            }
        };
        for entry in entries {
            let path = entry
                .map_err(|error| format!("无法读取 APK 列表：{error}"))?
                .path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("apk"))
            {
                paths.push(path);
            }
        }
    }
    paths.sort_by_cached_key(|path| util::file_name(path).to_lowercase());
    paths.dedup();
    Ok(paths)
}

/// Copy rather than remember the source path, so moving the source does not break the library.
pub fn import(source: &Path) -> Result<PathBuf, String> {
    import_into(source, &util::apk_library_dir())
}

fn fingerprint(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hash = Context::new(&SHA256);
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finish().as_ref().to_vec())
}

fn import_into(source: &Path, directory: &Path) -> Result<PathBuf, String> {
    if !source.is_file()
        || !source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("apk"))
    {
        return Err(format!("请选择有效的 APK 文件：{}", source.display()));
    }
    fs::create_dir_all(directory).map_err(|error| format!("无法创建 APK 列表目录：{error}"))?;
    let name = source.file_name().ok_or("APK 文件名无效")?;
    let stem = source
        .file_stem()
        .ok_or("APK 文件名无效")?
        .to_string_lossy();
    let mut index = 0u32;
    let mut source_hash = None;
    loop {
        let target = if index == 0 {
            directory.join(name)
        } else {
            directory.join(format!("{stem}-{index}.apk"))
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let open = options.open(&target);
        match open {
            Ok(mut output) => {
                let copied = (|| {
                    let mut input = File::open(source)?;
                    let mut buffer = [0; 64 * 1024];
                    let mut hash = Context::new(&SHA256);
                    loop {
                        let count = input.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        output.write_all(&buffer[..count])?;
                        hash.update(&buffer[..count]);
                    }
                    output.flush().map(|()| hash.finish().as_ref().to_vec())
                })();
                drop(output);
                match copied {
                    Ok(hash)
                        if source_hash
                            .as_ref()
                            .is_some_and(|source_hash| source_hash != &hash) =>
                    {
                        let _ = fs::remove_file(&target);
                        return Err("导入 APK 时源文件发生变化，请重试".into());
                    }
                    Ok(_) => return Ok(target),
                    Err(error) => {
                        let _ = fs::remove_file(&target);
                        return Err(format!("导入 APK 失败：{error}"));
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if source.canonicalize().ok() == target.canonicalize().ok() {
                    return Ok(target);
                }
                if fs::metadata(source).map(|meta| meta.len()).ok()
                    == fs::metadata(&target).map(|meta| meta.len()).ok()
                {
                    if source_hash.is_none() {
                        source_hash = Some(fingerprint(source)?);
                    }
                    let target_hash = fingerprint(&target)?;
                    if source_hash.as_ref() == Some(&target_hash) {
                        return Ok(target);
                    }
                }
                index = index.checked_add(1).ok_or("同名 APK 数量过多")?;
            }
            Err(error) => return Err(format!("无法保存 APK：{error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_preserve_sources_deduplicate_and_never_overwrite_collisions() {
        let root =
            std::env::temp_dir().join(format!("androidtool-apk-library-{}", std::process::id()));
        let source = root.join("source/demo.apk");
        let library = root.join("library");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"first apk").unwrap();
        let first = import_into(&source, &library).unwrap();
        assert_eq!(import_into(&source, &library).unwrap(), first);
        assert_eq!(import_into(&first, &library).unwrap(), first);
        fs::write(&source, b"other apk").unwrap();
        let second = import_into(&source, &library).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"first apk");
        assert_eq!(fs::read(&source).unwrap(), b"other apk");
        assert_eq!(fs::read(&second).unwrap(), b"other apk");
        assert_eq!(scan_directories(&[library]).unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}

// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub(super) const MAX_ENTRIES: usize = 50_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Entry {
    pub path: String,
    pub size: Option<u64>,
    pub modified_ns: u64,
    pub executable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub entries: Vec<Entry>,
}

pub(super) struct Source {
    pub manifest: Manifest,
    pub paths: Vec<PathBuf>,
}

pub(super) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}

pub(super) fn validate_path(path: &str) -> io::Result<()> {
    if path.len() > 2048 || !path.split('/').all(component) {
        return Err(invalid(format!("不支持的文件路径：{path}")));
    }
    Ok(())
}

impl Manifest {
    pub fn validate(&self, max_bytes: u64) -> io::Result<u64> {
        if self.entries.is_empty() || self.entries.len() > MAX_ENTRIES {
            return Err(invalid("文件数量必须为 1–50000"));
        }
        let mut paths = HashSet::new();
        let mut directories = HashSet::new();
        let mut bytes = 0u64;
        for entry in &self.entries {
            validate_path(&entry.path)?;
            let folded = entry.path.to_lowercase();
            if !paths.insert(folded.clone()) {
                return Err(invalid("重名或仅大小写不同的文件不支持跨平台传输"));
            }
            if let Some((parent, _)) = folded.rsplit_once('/') {
                if !directories.contains(parent) {
                    return Err(invalid("文件清单缺少父目录或父路径不是目录"));
                }
            }
            if let Some(size) = entry.size {
                bytes = bytes
                    .checked_add(size)
                    .ok_or_else(|| invalid("文件总大小溢出"))?;
            } else {
                directories.insert(folded);
            }
        }
        if bytes > max_bytes {
            return Err(invalid("传输超过接收端允许的大小"));
        }
        Ok(bytes)
    }

    pub fn id(&self, peer: &str) -> io::Result<String> {
        let mut hash = Sha256::new();
        hash.update(peer.as_bytes());
        hash.update(serde_json::to_vec(self).map_err(io::Error::other)?);
        Ok(format!("{:x}", hash.finalize()))
    }
}

pub(super) fn modified(metadata: &fs::Metadata) -> io::Result<u64> {
    let nanos = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    u64::try_from(nanos).map_err(|_| invalid("文件修改时间超出范围"))
}

pub(super) fn collect(paths: Vec<PathBuf>) -> io::Result<Source> {
    let mut source = Source {
        manifest: Manifest {
            entries: Vec::new(),
        },
        paths: Vec::new(),
    };
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("请选择文件或目录，文件名必须是 UTF-8"))?
            .to_owned();
        visit(&mut source, &path, name, 0)?;
    }
    source.manifest.validate(u64::MAX)?;
    Ok(source)
}

fn visit(source: &mut Source, path: &Path, relative: String, depth: usize) -> io::Result<()> {
    if depth > 64 || source.manifest.entries.len() >= MAX_ENTRIES {
        return Err(invalid("目录过深或文件超过 50000 个"));
    }
    validate_path(&relative)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(invalid(format!(
            "不传输符号链接或特殊文件：{}",
            path.display()
        )));
    }
    source.manifest.entries.push(Entry {
        path: relative.clone(),
        size: metadata.is_file().then_some(metadata.len()),
        modified_ns: modified(&metadata)?,
        executable: executable(&metadata),
    });
    source.paths.push(path.to_owned());
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| invalid("文件名必须是 UTF-8"))?;
            visit(
                source,
                &child.path(),
                format!("{relative}/{name}"),
                depth + 1,
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}

// A receiver owns its staging directory. Never follow links supplied by a
// previous transfer or local replacement when constructing destination paths.
pub(super) fn directory(base: &Path, relative: &str) -> io::Result<PathBuf> {
    let mut path = base.to_owned();
    for part in relative.split('/').filter(|p| !p.is_empty()) {
        if !component(part) {
            return Err(invalid("非法目录"));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(invalid("接收目录包含符号链接或非目录")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[cfg(unix)]
                let mut builder = fs::DirBuilder::new();
                #[cfg(not(unix))]
                let builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                builder.create(&path)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_windows_aliases_and_file_parents() {
        for path in [
            "../a", "/a", "a\\b", "C:/a", "a//b", "a/..", "NUL.txt", "a.",
        ] {
            assert!(validate_path(path).is_err(), "{path}");
        }
        let file = |path: &str| Entry {
            path: path.into(),
            size: Some(1),
            modified_ns: 0,
            executable: false,
        };
        assert!(
            Manifest {
                entries: vec![file("a"), file("a/b")]
            }
            .validate(10)
            .is_err()
        );
        assert!(
            Manifest {
                entries: vec![file("A"), file("a")]
            }
            .validate(10)
            .is_err()
        );
        assert!(
            Manifest {
                entries: vec![file("a")]
            }
            .validate(0)
            .is_err()
        );
    }
    #[test]
    fn collects_nested_and_empty_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("中文");
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("data"), b"hello").unwrap();
        let source = collect(vec![root]).unwrap();
        assert_eq!(source.manifest.validate(5).unwrap(), 5);
        assert_eq!(source.manifest.entries.len(), 3);
    }
    #[cfg(unix)]
    #[test]
    fn refuses_source_and_destination_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/tmp", temp.path().join("link")).unwrap();
        assert!(collect(vec![temp.path().join("link")]).is_err());
        assert!(directory(temp.path(), "link/nested").is_err());
    }
}

// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use std::{io, path::PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Snapshot {
    pub revision: u64,
    pub paths: Vec<PathBuf>,
    pub received: bool,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use objc2::{rc::autoreleasepool, runtime::ProtocolObject};
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSString, NSURL};

    fn marker() -> objc2::rc::Retained<NSString> {
        NSString::from_str("dev.lanbridge.received-files")
    }

    fn read(board: &NSPasteboard) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        if let Some(items) = board.pasteboardItems() {
            for item in items.iter() {
                // File URLs are decoded by Foundation, including escaped names.
                if let Some(value) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) {
                    let url = NSURL::URLWithString(&value)
                        .ok_or_else(|| io::Error::other("文件 URL 无效"))?;
                    if !url.isFileURL() {
                        continue;
                    }
                    if let Some(path) = url.path() {
                        paths.push(PathBuf::from(path.to_string()));
                    }
                }
            }
        }
        Ok(paths)
    }

    fn snapshot(board: &NSPasteboard) -> io::Result<Snapshot> {
        let revision = board.changeCount() as u64;
        let paths = read(board)?;
        let received = board.stringForType(&marker()).is_some();
        if revision != board.changeCount() as u64 {
            return Err(io::Error::other("文件剪贴板正在变化，稍后重试"));
        }
        Ok(Snapshot {
            revision,
            paths,
            received,
        })
    }

    fn write(board: &NSPasteboard, paths: Vec<PathBuf>, expected: Option<u64>) -> io::Result<bool> {
        // Each item carries the marker in the same write as its URL. Other
        // processes must never observe received URLs without loop suppression.
        let items = paths
            .iter()
            .map(|path| {
                let path = std::fs::canonicalize(path)?;
                let string = path
                    .to_str()
                    .ok_or_else(|| io::Error::other("文件路径不是 UTF-8"))?;
                let url = NSURL::fileURLWithPath(&NSString::from_str(string));
                let url_string = url
                    .absoluteString()
                    .ok_or_else(|| io::Error::other("文件 URL 无效"))?;
                let item = NSPasteboardItem::new();
                if !item.setString_forType(&url_string, unsafe { NSPasteboardTypeFileURL })
                    || !item.setString_forType(&NSString::from_str("1"), &marker())
                {
                    return Err(io::Error::other("文件剪贴板条目创建失败"));
                }
                Ok(item)
            })
            .collect::<io::Result<Vec<_>>>()?;
        if expected.is_some_and(|value| value != board.changeCount() as u64) {
            return Ok(false);
        }
        let objects: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = items
            .iter()
            .map(|item| ProtocolObject::from_ref(&**item))
            .collect();
        board.clearContents();
        if !board.writeObjects(&NSArray::from_slice(&objects)) {
            return Err(io::Error::other("文件剪贴板写入失败"));
        }
        Ok(true)
    }

    pub fn read_snapshot() -> io::Result<Snapshot> {
        autoreleasepool(|_| snapshot(&NSPasteboard::generalPasteboard()))
    }
    pub fn has_files() -> io::Result<bool> {
        autoreleasepool(|_| {
            let board = NSPasteboard::generalPasteboard();
            Ok(board.pasteboardItems().is_some_and(|items| {
                items.iter().any(|item| {
                    item.types()
                        .containsObject(unsafe { NSPasteboardTypeFileURL })
                })
            }))
        })
    }
    pub fn write_if_unchanged(paths: Vec<PathBuf>, expected: u64) -> io::Result<bool> {
        autoreleasepool(|_| write(&NSPasteboard::generalPasteboard(), paths, Some(expected)))
    }

    #[cfg(test)]
    #[derive(Clone)]
    pub(in crate::files) struct TestClipboard(String);
    #[cfg(test)]
    impl TestClipboard {
        pub(in crate::files) fn new() -> Self {
            autoreleasepool(|_| Self(NSPasteboard::pasteboardWithUniqueName().name().to_string()))
        }
        pub(in crate::files) fn snapshot(&self) -> io::Result<Snapshot> {
            autoreleasepool(|_| {
                snapshot(&NSPasteboard::pasteboardWithName(&NSString::from_str(
                    &self.0,
                )))
            })
        }
        pub(in crate::files) fn copy(&self, path: &std::path::Path) {
            autoreleasepool(|_| {
                let board = NSPasteboard::pasteboardWithName(&NSString::from_str(&self.0));
                let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
                let object: &ProtocolObject<dyn NSPasteboardWriting> =
                    ProtocolObject::from_ref(&*url);
                board.clearContents();
                assert!(board.writeObjects(&NSArray::from_slice(&[object])));
            });
        }
        pub(in crate::files) fn publish(
            &self,
            paths: Vec<PathBuf>,
            expected: u64,
        ) -> io::Result<bool> {
            autoreleasepool(|_| {
                write(
                    &NSPasteboard::pasteboardWithName(&NSString::from_str(&self.0)),
                    paths,
                    Some(expected),
                )
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn file_urls_roundtrip_on_private_pasteboard_without_touching_user_clipboard() {
            autoreleasepool(|_| {
                let temp = tempfile::tempdir().unwrap();
                let paths = vec![temp.path().join("中文 a#%.txt"), temp.path().join("second")];
                for path in &paths {
                    std::fs::write(path, b"test").unwrap();
                }
                let board = NSPasteboard::pasteboardWithUniqueName();
                write(&board, paths.clone(), None).unwrap();
                assert!(snapshot(&board).unwrap().received);
                assert_eq!(
                    read(&board).unwrap(),
                    paths
                        .iter()
                        .map(|p| std::fs::canonicalize(p).unwrap())
                        .collect::<Vec<_>>()
                );
                let previous = snapshot(&board).unwrap().revision;
                board.clearContents();
                assert!(!write(&board, paths, Some(previous)).unwrap());
                assert!(snapshot(&board).unwrap().paths.is_empty());
            });
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        mem,
        os::windows::ffi::{OsStrExt, OsStringExt},
        ptr,
    };
    use windows::{
        Win32::{
            Foundation::{GlobalFree, HANDLE, HWND, POINT},
            System::{
                DataExchange::{
                    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
                    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
                    SetClipboardData,
                },
                Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            },
            UI::{
                Shell::{DROPFILES, DragQueryFileW, HDROP},
                WindowsAndMessaging::{
                    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
                },
            },
        },
        core::w,
    };

    const CF_HDROP: u32 = 15;
    struct Clipboard(Option<HWND>);
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
                if let Some(window) = self.0 {
                    let _ = DestroyWindow(window);
                }
            }
        }
    }
    fn open(write: bool) -> io::Result<Clipboard> {
        unsafe {
            let window = if write {
                Some(
                    CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("STATIC"),
                        w!("DeskUnify Files"),
                        WINDOW_STYLE(0),
                        0,
                        0,
                        0,
                        0,
                        Some(HWND_MESSAGE),
                        None,
                        None,
                        None,
                    )
                    .map_err(io::Error::other)?,
                )
            } else {
                None
            };
            if let Err(error) = OpenClipboard(window) {
                if let Some(window) = window {
                    let _ = DestroyWindow(window);
                }
                return Err(io::Error::other(error));
            }
            Ok(Clipboard(window))
        }
    }
    fn marker() -> io::Result<u32> {
        let value = unsafe { RegisterClipboardFormatW(w!("dev.lanbridge.received-files")) };
        if value == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(value)
    }
    pub fn has_files() -> io::Result<bool> {
        Ok(unsafe { IsClipboardFormatAvailable(CF_HDROP).is_ok() })
    }
    pub fn read_snapshot() -> io::Result<Snapshot> {
        let _clipboard = open(false)?;
        unsafe {
            let revision = GetClipboardSequenceNumber() as u64;
            let received = IsClipboardFormatAvailable(marker()?).is_ok();
            if IsClipboardFormatAvailable(CF_HDROP).is_err() {
                return Ok(Snapshot {
                    revision,
                    paths: vec![],
                    received,
                });
            }
            let data = GetClipboardData(CF_HDROP)
                .map_err(|_| io::Error::other("请先在资源管理器复制文件或文件夹"))?;
            let drop = HDROP(data.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            if count == 0 || count > 50_000 {
                return Err(io::Error::other("文件剪贴板数量无效"));
            }
            let mut paths = Vec::new();
            for index in 0..count {
                let length = DragQueryFileW(drop, index, None) as usize;
                if length > 32768 {
                    return Err(io::Error::other("文件路径过长"));
                }
                let mut name = vec![0u16; length + 1];
                let actual = DragQueryFileW(drop, index, Some(&mut name)) as usize;
                if actual != length {
                    return Err(io::Error::other("文件剪贴板读取失败"));
                }
                paths.push(PathBuf::from(std::ffi::OsString::from_wide(
                    &name[..length],
                )));
            }
            Ok(Snapshot {
                revision,
                paths,
                received,
            })
        }
    }
    pub fn write_if_unchanged(paths: Vec<PathBuf>, expected: u64) -> io::Result<bool> {
        let mut names = Vec::new();
        for path in paths {
            names.extend(std::fs::canonicalize(path)?.as_os_str().encode_wide());
            names.push(0);
        }
        names.push(0);
        let marker = marker()?;
        let _clipboard = open(true)?;
        unsafe {
            if GetClipboardSequenceNumber() as u64 != expected {
                return Ok(false);
            }
            let size = mem::size_of::<DROPFILES>() + names.len() * 2;
            let memory = GlobalAlloc(GMEM_MOVEABLE, size).map_err(io::Error::other)?;
            let data = GlobalLock(memory);
            if data.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err(io::Error::last_os_error());
            }
            let header = DROPFILES {
                pFiles: mem::size_of::<DROPFILES>() as u32,
                pt: POINT::default(),
                fNC: false.into(),
                fWide: true.into(),
            };
            ptr::write(data.cast::<DROPFILES>(), header);
            ptr::copy_nonoverlapping(
                names.as_ptr(),
                data.cast::<u8>()
                    .add(mem::size_of::<DROPFILES>())
                    .cast::<u16>(),
                names.len(),
            );
            let _ = GlobalUnlock(memory);
            let result =
                EmptyClipboard().and_then(|_| SetClipboardData(CF_HDROP, Some(HANDLE(memory.0))));
            if let Err(error) = result {
                let _ = GlobalFree(Some(memory));
                return Err(io::Error::other(error));
            }
            // Successful SetClipboardData transfers ownership to Windows.
            let tag = match GlobalAlloc(GMEM_MOVEABLE, 1) {
                Ok(tag) => tag,
                Err(error) => {
                    let _ = EmptyClipboard();
                    return Err(io::Error::other(error));
                }
            };
            let pointer = GlobalLock(tag);
            if pointer.is_null() {
                let _ = GlobalFree(Some(tag));
                let _ = EmptyClipboard();
                return Err(io::Error::last_os_error());
            }
            *pointer.cast::<u8>() = 1;
            let _ = GlobalUnlock(tag);
            if let Err(error) = SetClipboardData(marker, Some(HANDLE(tag.0))) {
                let _ = GlobalFree(Some(tag));
                let _ = EmptyClipboard();
                return Err(io::Error::other(error));
            }
            Ok(true)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(super) use platform::TestClipboard;
pub(super) use platform::{has_files, read_snapshot, write_if_unchanged};

pub(super) fn read_paths() -> io::Result<Vec<PathBuf>> {
    let snapshot = read_snapshot()?;
    if snapshot.paths.is_empty() {
        return Err(io::Error::other(
            "请先在 Finder / 资源管理器复制文件或文件夹",
        ));
    }
    Ok(snapshot.paths)
}

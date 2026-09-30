// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use std::{io, sync::mpsc, thread};
use tokio::sync::oneshot;

enum Request {
    Read(oneshot::Sender<io::Result<Option<String>>>),
    Write(String, oneshot::Sender<io::Result<()>>),
}

/// All OS clipboard access runs on one dedicated thread, including creation
/// and destruction. It cannot block the input runtime or overlap on Windows.
pub(super) struct NativeClipboard {
    requests: Option<mpsc::Sender<Request>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl NativeClipboard {
    pub fn new() -> io::Result<Self> {
        let (requests, incoming) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut clipboard = None;
                while let Ok(request) = incoming.recv() {
                    let backend = match clipboard.as_mut() {
                        Some(backend) => Ok(backend),
                        None => arboard::Clipboard::new().map(|backend| clipboard.insert(backend)),
                    };
                    match request {
                        Request::Read(reply) => {
                            // Finder may also offer a text representation of file
                            // URLs. Never sync that text over the file clipboard.
                            if crate::files::has_file_clipboard().unwrap_or(false) {
                                let _ = reply.send(Ok(None));
                                continue;
                            }
                            let result = backend
                                .and_then(|backend| match backend.get_text() {
                                    Ok(text) => Ok(Some(text)),
                                    Err(arboard::Error::ContentNotAvailable) => Ok(None),
                                    Err(error) => Err(error),
                                })
                                .map_err(io::Error::other);
                            let _ = reply.send(result);
                        }
                        Request::Write(text, reply) => {
                            let result = backend
                                .and_then(|backend| backend.set_text(text))
                                .map_err(io::Error::other);
                            let _ = reply.send(result);
                        }
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            worker: Some(worker),
        })
    }

    pub async fn read(&self) -> io::Result<Option<String>> {
        let (reply, response) = oneshot::channel();
        self.requests
            .as_ref()
            .ok_or_else(closed)?
            .send(Request::Read(reply))
            .map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }

    pub async fn write(&self, text: String) -> io::Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .as_ref()
            .ok_or_else(closed)?
            .send(Request::Write(text, reply))
            .map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }

    pub async fn terminate(mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "clipboard worker stopped")
}

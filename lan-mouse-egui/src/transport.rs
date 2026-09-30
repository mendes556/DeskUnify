// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use futures::{Stream, StreamExt};
use lan_mouse_ipc::{
    FrontendEvent, FrontendRequest, IPC_VERSION, IpcError, UiAction, UiSnapshot, connect_async,
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);

pub async fn execute(action: UiAction) -> Result<UiSnapshot, String> {
    // Check the schema before allowing an old daemon to apply a mutation.
    if !matches!(action, UiAction::Snapshot) {
        request(UiAction::Snapshot).await?;
    }
    let scan = matches!(action, UiAction::Scan);
    let retry = matches!(action, UiAction::RetryBackends);
    let mut snapshot = request(action).await?;
    if scan || retry {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            // Re-enable is asynchronous: always read at least one fresh snapshot.
            tokio::time::sleep(Duration::from_millis(250)).await;
            snapshot = request(UiAction::Snapshot).await?;
            if scan {
                if let Some(error) = &snapshot.discovery_error {
                    return Err(format!("局域网发现失败：{error}"));
                }
            }
            if (retry && snapshot.native_ready()) || tokio::time::Instant::now() >= deadline {
                break;
            }
        }
    }
    snapshot
        .discovered
        .sort_by(|a, b| a.fingerprint.cmp(&b.fingerprint));
    Ok(snapshot)
}

async fn request(action: UiAction) -> Result<UiSnapshot, String> {
    let id = format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut events, mut requests) = connect_async(Some(Duration::from_secs(2)))
            .await
            .map_err(|error| format!("无法连接本机后台：{error}"))?;
        requests
            .request(FrontendRequest::Ui {
                id: id.clone(),
                action,
            })
            .await
            .map_err(|error| error.to_string())?;
        read_result(&mut events, &id).await
    })
    .await
    .map_err(|_| "后台未确认操作；请检查后台是否为当前版本".to_owned())?
}

async fn read_result(
    events: &mut (impl Stream<Item = Result<FrontendEvent, IpcError>> + Unpin),
    id: &str,
) -> Result<UiSnapshot, String> {
    while let Some(event) = events.next().await {
        if let FrontendEvent::UiResult {
            id: response_id,
            result,
        } = event.map_err(|error| error.to_string())?
        {
            if response_id == id {
                let snapshot = result?;
                if snapshot.protocol_version != IPC_VERSION {
                    return Err("界面与后台版本不一致，请退出旧后台后重新启动".into());
                }
                return Ok(snapshot);
            }
        }
    }
    Err("本机后台连接已断开".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::snapshot;
    use futures::stream;

    #[tokio::test]
    async fn unrelated_replies_do_not_confirm_an_operation() {
        let mut events = stream::iter([
            Ok(FrontendEvent::Enumerate(vec![])),
            Ok(FrontendEvent::UiResult {
                id: "other".into(),
                result: Err("unrelated".into()),
            }),
            Ok(FrontendEvent::UiResult {
                id: "mine".into(),
                result: Ok(snapshot()),
            }),
        ]);
        assert_eq!(read_result(&mut events, "mine").await.unwrap().port, 4242);
    }

    #[tokio::test]
    async fn rejection_disconnect_and_old_schema_are_failures() {
        assert!(read_result(&mut stream::empty(), "mine").await.is_err());
        let mut events = stream::iter([Ok(FrontendEvent::UiResult {
            id: "mine".into(),
            result: Err("配置保存失败".into()),
        })]);
        assert_eq!(
            read_result(&mut events, "mine").await.unwrap_err(),
            "配置保存失败"
        );
        let mut old = snapshot();
        old.protocol_version -= 1;
        let mut events = stream::iter([Ok(FrontendEvent::UiResult {
            id: "mine".into(),
            result: Ok(old),
        })]);
        assert!(
            read_result(&mut events, "mine")
                .await
                .unwrap_err()
                .contains("版本不一致")
        );
    }
}

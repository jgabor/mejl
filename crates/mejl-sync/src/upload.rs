//! Attachment upload: read a local file off the async threads, then upload
//! it as a blob.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;

use crate::error::SyncError;
use crate::event::SyncEvent;
use crate::shared::Shared;

/// The media type for a file name, falling back to plain bytes.
pub(crate) fn guess_type(path: &Path) -> String {
    mime_guess::from_path(path)
        .first()
        .map(|m| m.essence_str().to_owned())
        .unwrap_or_else(|| "application/octet-stream".to_owned())
}

pub(crate) async fn upload(
    sh: Arc<Shared>,
    token: u64,
    path: PathBuf,
    content_type: Option<String>,
) {
    sh.status.update(|s| s.activity.uploads += 1);
    let result = upload_inner(&sh, &path, content_type).await;
    sh.status
        .update(|s| s.activity.uploads = s.activity.uploads.saturating_sub(1));
    match result {
        Ok(event) => sh.events.emit(event(token)),
        Err(e) => {
            sh.note(&e);
            sh.events.emit(SyncEvent::UploadFailed {
                token,
                error: e.to_string(),
            });
        }
    }
}

async fn upload_inner(
    sh: &Shared,
    path: &Path,
    content_type: Option<String>,
) -> Result<impl FnOnce(u64) -> SyncEvent, SyncError> {
    let session = sh.session().await?;
    let account = sh.account()?;
    let limit = session.core_limits().max_size_upload;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| SyncError::Invalid(format!("{} is not a file", path.display())))?;
    let content_type = content_type.unwrap_or_else(|| guess_type(path));
    let owned = path.to_owned();
    // File reads block, so they run on the blocking pool. The size is
    // checked first so a huge file is never read into memory.
    let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, SyncError> {
        let meta = std::fs::metadata(&owned)
            .map_err(|e| SyncError::Invalid(format!("cannot read {}: {e}", owned.display())))?;
        if !meta.is_file() {
            return Err(SyncError::Invalid(format!(
                "{} is not a file",
                owned.display()
            )));
        }
        if meta.len() > limit {
            return Err(SyncError::Invalid(format!(
                "{} is {} bytes; the server takes at most {limit}",
                owned.display(),
                meta.len()
            )));
        }
        std::fs::read(&owned)
            .map_err(|e| SyncError::Invalid(format!("cannot read {}: {e}", owned.display())))
    })
    .await
    .map_err(|e| SyncError::Invalid(format!("the file read stopped: {e}")))??;
    let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
    let result = sh
        .client
        .upload(&account, &content_type, Bytes::from(data))
        .await;
    if result.is_ok() {
        sh.set_online(true);
    }
    let uploaded = result?;
    Ok(move |token| SyncEvent::Uploaded {
        token,
        blob_id: uploaded.blob_id,
        name,
        content_type,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_types() {
        assert_eq!(guess_type(Path::new("rota.pdf")), "application/pdf");
        assert_eq!(guess_type(Path::new("notes.txt")), "text/plain");
        assert_eq!(
            guess_type(Path::new("mystery.zzzz")),
            "application/octet-stream"
        );
    }
}

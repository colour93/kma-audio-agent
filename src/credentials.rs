use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub agent_id: Uuid,
    pub room_id: String,
    pub token: String,
}

impl Credentials {
    pub async fn load(path: &Path) -> Result<Option<Self>> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("parse credentials")?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("read credentials"),
        }
    }

    pub async fn store(&self, path: &Path) -> Result<()> {
        store_private(path, &serde_json::to_vec_pretty(self)?).await
    }
}

pub(crate) async fn store_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let temporary = path.with_extension("json.part");
    tokio::fs::write(&temporary, bytes).await?;
    set_private_permissions(&temporary).await?;
    tokio::fs::rename(&temporary, path).await?;
    set_private_permissions(path).await?;
    Ok(())
}

#[cfg(unix)]
async fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

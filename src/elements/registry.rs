use std::fs;
use std::path;

use serde_json::Value as JsonValue;

use crate::errors::*;

// length of asset id prefix to use for sub-directory partitioning
// (in number of hex characters, not bytes)

const DIR_PARTITION_LEN: usize = 2;
pub struct AssetRegistry {
    directory: path::PathBuf,
}

impl AssetRegistry {
    pub fn new(directory: path::PathBuf) -> Self {
        Self { directory }
    }

    pub fn load<A: ToString>(&self, asset_id: A) -> Result<Option<AssetMeta>> {
        let name = format!("{}.json", asset_id.to_string());
        let subdir = self.directory.join(&name[0..DIR_PARTITION_LEN]);
        let path = subdir.join(name);
        Ok(if path.exists() {
            let contents = fs::read_to_string(path).chain_err(|| "failed reading file")?;
            Some(serde_json::from_str(&contents).chain_err(|| "failed parsing file")?)
        } else {
            None
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AssetMeta {
    pub contract: JsonValue,
    pub entity: JsonValue,
    pub precision: u8,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticker: Option<String>,
}

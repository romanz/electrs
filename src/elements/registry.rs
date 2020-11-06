use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use std::{cmp, fs, path, thread};

use serde_json::Value as JsonValue;

use elements::bitcoin_hashes::hex::FromHex;
use elements::AssetId;

use crate::errors::*;

// length of asset id prefix to use for sub-directory partitioning
// (in number of hex characters, not bytes)

const DIR_PARTITION_LEN: usize = 2;
pub struct AssetRegistry {
    directory: path::PathBuf,
    assets_cache: HashMap<AssetId, (SystemTime, AssetMeta)>,
}

pub type AssetEntry<'a> = (&'a AssetId, &'a AssetMeta);

impl AssetRegistry {
    pub fn new(directory: path::PathBuf) -> Self {
        Self {
            directory,
            assets_cache: Default::default(),
        }
    }

    pub fn get(&self, asset_id: &AssetId) -> Option<&AssetMeta> {
        self.assets_cache
            .get(asset_id)
            .map(|(_, metadata)| metadata)
    }

    pub fn list(&self, start_index: usize, limit: usize, sorting: AssetSorting) -> Vec<AssetEntry> {
        let mut assets: Vec<AssetEntry> = self
            .assets_cache
            .iter()
            .map(|(asset_id, (_, metadata))| (asset_id, metadata))
            .collect();
        assets.sort_by(sorting.as_comparator());
        assets.into_iter().skip(start_index).take(limit).collect()
    }

    pub fn fs_sync(&mut self) -> Result<()> {
        for entry in fs::read_dir(&self.directory).chain_err(|| "failed reading asset dir")? {
            let entry = entry.chain_err(|| "invalid fh")?;
            let filetype = entry.file_type().chain_err(|| "failed getting file type")?;
            if !filetype.is_dir() || entry.file_name().len() != DIR_PARTITION_LEN {
                continue;
            }

            for file_entry in
                fs::read_dir(entry.path()).chain_err(|| "failed reading asset subdir")?
            {
                let file_entry = file_entry.chain_err(|| "invalid fh")?;
                let path = file_entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                let asset_id = AssetId::from_hex(
                    path.file_stem()
                        .unwrap() // cannot fail if extension() succeeded
                        .to_str()
                        .chain_err(|| "invalid filename")?,
                )
                .chain_err(|| "invalid filename")?;

                let modified = file_entry
                    .metadata()
                    .chain_err(|| "failed reading metadata")?
                    .modified()
                    .chain_err(|| "metadata modified failed")?;

                if let Some((last_update, _)) = self.assets_cache.get(&asset_id) {
                    if *last_update == modified {
                        continue;
                    }
                }

                let metadata: AssetMeta = serde_json::from_str(
                    &fs::read_to_string(path).chain_err(|| "failed reading file")?,
                )
                .chain_err(|| "failed parsing file")?;

                self.assets_cache.insert(asset_id, (modified, metadata));
            }
        }
        Ok(())
    }

    pub fn spawn_sync(asset_db: Arc<RwLock<AssetRegistry>>) -> thread::JoinHandle<()> {
        thread::spawn(move || loop {
            if let Err(e) = asset_db.write().unwrap().fs_sync() {
                error!("registry fs_sync failed: {:?}", e);
            }

            thread::sleep(Duration::from_secs(15));
            // TODO handle shutdowm
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AssetMeta {
    #[serde(skip_serializing_if = "JsonValue::is_null")]
    pub contract: JsonValue,
    #[serde(skip_serializing_if = "JsonValue::is_null")]
    pub entity: JsonValue,
    pub precision: u8,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticker: Option<String>,
}

impl AssetMeta {
    fn domain(&self) -> Option<&str> {
        self.entity["domain"].as_str()
    }
}

pub struct AssetSorting(AssetSortField, AssetSortDir);

pub enum AssetSortField {
    Name,
    Domain,
    Ticker,
}
pub enum AssetSortDir {
    Descending,
    Ascending,
}

impl AssetSorting {
    fn as_comparator(self) -> Box<dyn Fn(&AssetEntry, &AssetEntry) -> cmp::Ordering> {
        let sort_fn: Box<dyn Fn(&AssetEntry, &AssetEntry) -> cmp::Ordering> = match self.0 {
            AssetSortField::Name => {
                // Order by name first, use asset id as a tie breaker. the other sorting fields
                // don't require this because they're guaranteed to be unique.
                Box::new(|a, b| lc_cmp(&a.1.name, &b.1.name).then_with(|| a.0.cmp(b.0)))
            }
            AssetSortField::Domain => Box::new(|a, b| a.1.domain().cmp(&b.1.domain())),
            AssetSortField::Ticker => Box::new(|a, b| lc_cmp(&a.1.ticker, &b.1.ticker)),
        };

        match self.1 {
            AssetSortDir::Ascending => sort_fn,
            AssetSortDir::Descending => Box::new(move |a, b| sort_fn(a, b).reverse()),
        }
    }

    pub fn from_query_params(query: &HashMap<String, String>) -> Result<Self> {
        let field = match query.get("sort_field").map(String::as_str) {
            None => AssetSortField::Ticker,
            Some("name") => AssetSortField::Name,
            Some("domain") => AssetSortField::Domain,
            Some("ticker") => AssetSortField::Ticker,
            _ => bail!("invalid sort field"),
        };

        let dir = match query.get("sort_dir").map(String::as_str) {
            None => AssetSortDir::Ascending,
            Some("asc") => AssetSortDir::Ascending,
            Some("desc") => AssetSortDir::Descending,
            _ => bail!("invalid sort direction"),
        };

        Ok(Self(field, dir))
    }
}

fn lc_cmp(a: &str, b: &str) -> cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase())
}

use bitcoin_hashes::sha256d::Hash as Sha256dHash;
use bitcoin_hashes::hex::FromHex;

use rusoto_core::Region;
use rusoto_dynamodb::{DynamoDb, DynamoDbClient, ScanInput};

use std::default::Default;
use std::env;
use std::format;

use crate::errors::*;
use std::collections::HashMap;
use serde_json::Value;

pub struct SubscriptionsManager {}

impl SubscriptionsManager {

    pub fn get_script_hashes() -> Result<HashMap<Sha256dHash, Value>> {
        let client = DynamoDbClient::new(Region::UsWest2);

        let mut script_hashes = HashMap::new();
        let mut last_evaluated_key = None;
        // we need an alias for "status" because that happens to be a saved word
        let mut expression_attribute_names = HashMap::new();
        expression_attribute_names.insert(String::from("#statusHash"), String::from("status"));

        loop {
            // loop until no more pages (1MB limit)
            let scan_input = ScanInput {
                table_name: format!("{}_AddressInfo", env::var("ENV").unwrap_or(String::from("dev"))),
                projection_expression: Some(String::from("electrumHash, #statusHash")),
                expression_attribute_names: Some(expression_attribute_names.clone()),
                exclusive_start_key: last_evaluated_key.clone(),
                ..Default::default()
            };

            match client.scan(scan_input).sync() {
                Ok(output) => {
                    match output.items {
                        Some(items) => {
                            for item in items {
                                let script_hash_attribute_value = item.get("electrumHash").unwrap();
                                let script_hash_str = script_hash_attribute_value.s.as_ref().unwrap();
                                let script_hash_res = Sha256dHash::from_hex(&script_hash_str);
                                if script_hash_res.is_ok() {
                                    let script_hash = script_hash_res.unwrap();

                                    let status_hash_attribute_value_option = item.get("status");
                                    let status_hash_str_option =
                                        status_hash_attribute_value_option.map(|attr_value| attr_value.s.as_ref().unwrap());
                                    let status_hash = match status_hash_str_option {
                                        Some(s) => json!(s),
                                        None => Value::Null,
                                    };

                                    debug!("subscribing script_hash = {:?}, status_hash = {:?}", script_hash, status_hash);
                                    script_hashes.insert(script_hash, status_hash);
                                }
                            }
                        },
                        None => {
                            bail!(ErrorKind::DynamoDB("Failed fetching script hashes from DB".to_string()))
                        }
                    };
                    last_evaluated_key = output.last_evaluated_key;
                    if last_evaluated_key.is_none() {
                        break;
                    }
                },
                Err(error) => {
                    bail!(ErrorKind::DynamoDB(error.to_string()))
                }
            }
        }

        Ok(script_hashes)
    }
}
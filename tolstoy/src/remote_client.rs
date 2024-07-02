// Copyright 2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![allow(dead_code)]

use std::fmt::Display;
use reqwest::Url;
use hyper::{StatusCode};
use serde::{Deserialize, Serialize};
use serde_json;
// TODO: https://github.com/mozilla/mentat/issues/570
// use serde_cbor;
use futures::executor::block_on;
use uuid::Uuid;
use public_traits::errors::MentatError::TolstoyError;

use crate::logger::d;
use public_traits::errors::Result;

use crate::types::{GlobalTransactionLog, Tx, TxPart};

#[derive(Serialize, Deserialize)]
struct SerializedHead {
    head: Uuid,
}

#[derive(Serialize)]
struct SerializedTransaction<'a> {
    parent: &'a Uuid,
    chunks: &'a [Uuid],
}

#[derive(Deserialize)]
struct DeserializableTransaction {
    parent: Uuid,
    chunks: Vec<Uuid>,
    id: Uuid,
    seq: i64,
}

#[derive(Deserialize)]
struct SerializedTransactions {
    limit: i64,
    from: Uuid,
    transactions: Vec<Uuid>,
}

pub struct RemoteClient {
    base_uri: String,
    user_uuid: Uuid,
}

impl RemoteClient {
    pub fn new(base_uri: String, user_uuid: Uuid) -> Self {
        RemoteClient {
            base_uri,
            user_uuid,
        }
    }

    fn bound_base_uri(&self) -> String {
        format!("{}/{}", self.base_uri, self.user_uuid) // TODO escaping
    }

    fn get_uuid(&self, uri: String) -> Result<Uuid> {
        Ok(block_on(async {
            let result: SerializedHead = https_get(uri).await?;
            result.head
        }))
    }

    fn put<T>(&self, uri: String, payload: T, expected: StatusCode) -> Result<()>
    where
        T: Display
    {
        block_on(async {
            https_put(uri, payload, expected).await.unwrap(); //TODO ?
        });
        Ok(())
    }

    fn get_transactions(&self, parent_uuid: &Uuid) -> Result<Vec<Uuid>> {
        let uri = format!(
            "{}/transactions?from={}",
            self.bound_base_uri(),
            parent_uuid
        );
        Ok(block_on(async {
            let result: SerializedTransactions = https_get(uri).await?;
            d(&format!("got transactions: {:?}", &result.transactions));
            result.transactions
        }))
    }

    fn get_chunks(&self, transaction_uuid: &Uuid) -> Result<Vec<Uuid>> {
        let uri = format!(
            "{}/transactions/{}",
            self.bound_base_uri(),
            transaction_uuid
        );
        Ok(block_on(async {
            let result: DeserializableTransaction = https_get(uri);
            d(&format!("got transaction chunks: {:?}", &result.chunks));
            result.chunks
        }))
    }

    fn get_chunk(&self, chunk_uuid: &Uuid) -> Result<TxPart> {
        let uri = format!("{}/chunks/{}", self.bound_base_uri(), chunk_uuid);
        Ok(block_on(async {
            let result: TxPart = https_get(uri);
            d(&format!("got transaction chunk: {:?}", &result));
            result
        }))
    }
}

async fn https_put<T>(uri: String, payload: T, expected: StatusCode) -> Result<()>
where
    T: ToString
{
    d(&format!("PUT {:?}", uri));
    let url = Url::parse(&*uri)?;

    // Create a reqwest client
    let client = reqwest::Client::new();

    // Make the HTTP PUT request with the Accept: application/json header
    let response = client
        .put(url)
        .header(reqwest::header::ACCEPT, mime::APPLICATION_JSON.to_string())
        .body(payload.to_string())
        .send()
        .await?;

    // Check if the request was successful (status code 2xx)
    dbg!("HTTP/PUT response.status: {}", response.status());
    if !response.status().is_success() {
        Err(format!("HTTP/PUT request failed with status code: {}", response.status()).into())
    } else if response.status() != expected {
        Err(format!("HTTP/PUT request status code: {} does not match expected code: {}", response.status(), expected).into())
    } else {
        Ok(())
    }
}

async fn https_get<'de, T>(uri: String) -> Result<T>
    where
        T: Deserialize<'de> + Serialize,
{
    d(&format!("GET {:?}", uri));
    let url = Url::parse(&*uri)?;

    // Create a reqwest client
    let client = reqwest::Client::builder()
        .tcp_nodelay(true)
        .build()?;

    // Make the HTTP GET request
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, mime::APPLICATION_JSON.to_string())
        .send()
        .await?;

    // Check if the request was successful (status code 2xx)
    dbg!("HTTP/GET response.status: {}", response.status());

    if !response.status().is_success() {
        Err(TolstoyError::BadRemoteResponse(format!("Failed to get JSON from URL: {}", response.status())))
    } else {
        // Deserialize the JSON response in the body
        let body = response.text().await?;
        let result = serde_json::from_str::<T>(&body)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(result)
    }
}

impl GlobalTransactionLog for RemoteClient {
    fn head(&self) -> Result<Uuid> {
        let uri = format!("{}/head", self.bound_base_uri());
        self.get_uuid(uri)
    }

    fn set_head(&mut self, uuid: &Uuid) -> Result<()> {
        // {"head": uuid}
        let head = SerializedHead { head: *uuid };

        let uri = format!("{}/head", self.bound_base_uri());
        let json = serde_json::to_string(&head)?;
        d(&format!("serialized head: {:?}", json));
        self.put(uri, json, StatusCode::NO_CONTENT)
    }

    /// Slurp transactions and datoms after `tx`, returning them as owned data.
    /// TODO: This is inefficient but convenient for development.
    fn transactions_after(&self, tx: &Uuid) -> Result<Vec<Tx>> {
        let new_txs = self.get_transactions(tx)?;
        let mut tx_list = Vec::new();

        for tx in new_txs {
            let mut tx_parts = Vec::new();
            let chunks = self.get_chunks(&tx)?;

            // We pass along all of the downloaded parts, including transaction's
            // metadata datom. Transactor is expected to do the right thing, and
            // use txInstant from one of our datoms.
            for chunk in chunks {
                let part = self.get_chunk(&chunk)?;
                tx_parts.push(part);
            }

            tx_list.push(Tx {
                tx,
                parts: tx_parts,
            });
        }

        d(&format!("got tx list: {:?}", &tx_list));

        Ok(tx_list)
    }

    fn put_transaction(
        &mut self,
        transaction_uuid: &Uuid,
        parent_uuid: &Uuid,
        chunks: &[Uuid],
    ) -> Result<()> {
        // {"parent": uuid, "chunks": [chunk1, chunk2...]}
        let transaction = SerializedTransaction {
            parent: parent_uuid,
            chunks,
        };

        let uri = format!(
            "{}/transactions/{}",
            self.bound_base_uri(),
            transaction_uuid
        );
        let json = serde_json::to_string(&transaction)?;
        d(&format!("serialized transaction: {:?}", json));
        self.put(uri, json, StatusCode::CREATED)
    }

    fn put_chunk(&mut self, chunk_uuid: &Uuid, payload: &TxPart) -> Result<()> {
        let payload: String = serde_json::to_string(payload)?;
        let uri = format!("{}/chunks/{}", self.bound_base_uri(), chunk_uuid);
        d(&format!("serialized chunk: {:?}", payload));
        // TODO don't want to clone every datom!
        self.put(uri, payload, StatusCode::CREATED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_remote_client_bound_uri() {
        let user_uuid = Uuid::from_str(&"316ea470-ce35-4adf-9c61-e0de6e289c59").expect("uuid");
        let server_uri = String::from("https://example.com/api/0.1");
        let remote_client = RemoteClient::new(server_uri, user_uuid);
        assert_eq!(
            "https://example.com/api/0.1/316ea470-ce35-4adf-9c61-e0de6e289c59",
            remote_client.bound_base_uri()
        );
    }
}

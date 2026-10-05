// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![allow(dead_code)]

use std; // To refer to std::result::Result.

use std::collections::BTreeSet;
use std::error::Error;

use thiserror::Error;

use rusqlite;
use uuid;

use edn;

use core_traits::{Attribute, ValueType};

use db_traits::errors::DbError;
use query_algebrizer_traits::errors::AlgebrizerError;
use query_projector_traits::errors::ProjectorError;
use query_pull_traits::errors::PullError;
use sql_traits::errors::SQLError;

pub type Result<T> = std::result::Result<T, MentatError>;

#[derive(Debug, Error)]
pub enum MentatError {
    #[error("bad uuid {0}")]
    BadUuid(String),

    #[error("path {0} already exists")]
    PathAlreadyExists(String),

    #[error("variables {0:?} unbound at query execution time")]
    UnboundVariables(BTreeSet<String>),

    #[error("invalid argument name: '{0}'")]
    InvalidArgumentName(String),

    /// Bad JSON query options (`mentat::options_from_json`).
    #[error("query options: {0}")]
    BadQueryOptions(String),

    #[error("unknown attribute: '{0}'")]
    UnknownAttribute(String),

    #[error("invalid vocabulary version")]
    InvalidVocabularyVersion,

    #[error(
        "vocabulary {0}/version {1} already has attribute {2}, and the requested definition differs"
    )]
    ConflictingAttributeDefinitions(String, u32, String, Attribute, Attribute),

    #[error("existing vocabulary {0} too new: wanted version {1}, got version {2}")]
    ExistingVocabularyTooNew(String, u32, u32),

    #[error("core schema: wanted version {0}, got version {1:?}")]
    UnexpectedCoreSchema(u32, Option<u32>),

    #[error("Lost the transact() race!")]
    UnexpectedLostTransactRace,

    #[error("missing core attribute {0}")]
    MissingCoreVocabulary(edn::query::Keyword),

    #[error("schema changed since query was prepared")]
    PreparedQuerySchemaMismatch,

    #[error("provided value of type {0} doesn't match attribute value type {1}")]
    ValueTypeMismatch(ValueType, ValueType),

    #[error("{0}")]
    IoError(#[source] std::io::Error),

    /// We're just not done yet.  Message that the feature is recognized but not yet
    /// implemented.
    #[error("not yet implemented: {0}")]
    NotYetImplemented(String),

    // It would be better to capture the underlying `rusqlite::Error`, but that type doesn't
    // implement many useful traits, including `Clone`, `Eq`, and `PartialEq`.
    #[error("SQL error: {0}, cause: {1}")]
    RusqliteError(String, String),

    #[error("{0}")]
    EdnParseError(#[source] edn::ParseError),

    #[error("{0}")]
    DbError(#[source] DbError),

    #[error("{0}")]
    AlgebrizerError(#[source] AlgebrizerError),

    #[error("{0}")]
    ProjectorError(#[source] ProjectorError),

    #[error("{0}")]
    PullError(#[source] PullError),

    #[error("{0}")]
    SQLError(#[source] SQLError),

    #[error("{0}")]
    UuidError(#[source] uuid::Error),
}

impl From<std::io::Error> for MentatError {
    fn from(error: std::io::Error) -> Self {
        MentatError::IoError(error)
    }
}

impl From<sql_traits::conn::SqlError> for MentatError {
    fn from(error: sql_traits::conn::SqlError) -> Self {
        MentatError::RusqliteError(error.message, String::new())
    }
}

impl From<rusqlite::Error> for MentatError {
    fn from(error: rusqlite::Error) -> Self {
        let cause = match error.source() {
            Some(e) => e.to_string(),
            None => "".to_string(),
        };
        MentatError::RusqliteError(error.to_string(), cause)
    }
}

impl From<uuid::Error> for MentatError {
    fn from(error: uuid::Error) -> Self {
        MentatError::UuidError(error)
    }
}

impl From<edn::ParseError> for MentatError {
    fn from(error: edn::ParseError) -> Self {
        MentatError::EdnParseError(error)
    }
}

impl From<DbError> for MentatError {
    fn from(error: DbError) -> Self {
        MentatError::DbError(error)
    }
}

impl From<AlgebrizerError> for MentatError {
    fn from(error: AlgebrizerError) -> Self {
        MentatError::AlgebrizerError(error)
    }
}

impl From<ProjectorError> for MentatError {
    fn from(error: ProjectorError) -> Self {
        MentatError::ProjectorError(error)
    }
}

impl From<PullError> for MentatError {
    fn from(error: PullError) -> Self {
        MentatError::PullError(error)
    }
}

impl From<SQLError> for MentatError {
    fn from(error: SQLError) -> Self {
        MentatError::SQLError(error)
    }
}

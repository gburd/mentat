// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use std;

use core_traits::{ValueType, ValueTypeSet};

use edn::{query::PlainSymbol, ParseError};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, AlgebrizerError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingError {
    NoBoundVariable,
    UnexpectedBinding,
    RepeatedBoundVariable, // TODO: include repeated variable(s).

    /// Expected `[[?x ?y]]` but got some other type of binding.  Mentat is deliberately more strict
    /// than Datomic: we won't try to make sense of non-obvious (and potentially erroneous) bindings.
    ExpectedBindRel,

    /// Expected `[[?x ?y]]` or `[?x ...]` but got some other type of binding.  Mentat is
    /// deliberately stricter than Datomic: we won't try to make sense of non-obvious (and
    /// potentially erroneous) bindings.
    ExpectedBindRelOrBindColl,

    /// Expected `[?x1 … ?xN]` or `[[?x1 … ?xN]]` but got some other number of bindings.  Mentat is
    /// deliberately stricter than Datomic: we prefer placeholders to omission.
    InvalidNumberOfBindings {
        number: usize,
        expected: usize,
    },
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AlgebrizerError {
    #[error("{0} var {1} is duplicated")]
    DuplicateVariableError(PlainSymbol, &'static str),

    #[error("unexpected FnArg")]
    UnsupportedArgument,

    #[error("value of type {0} provided for var {1}, expected {2}")]
    InputTypeDisagreement(PlainSymbol, ValueType, ValueType),

    #[error("invalid number of arguments to {0}: expected {1}, got {2}.")]
    InvalidNumberOfArguments(PlainSymbol, usize, usize),

    #[error("invalid argument to {0}: expected {1} in position {2}.")]
    InvalidArgument(PlainSymbol, &'static str, usize),

    #[error("invalid argument to {0}: expected one of {1:?} in position {2}.")]
    InvalidArgumentType(PlainSymbol, ValueTypeSet, usize),

    // TODO: flesh this out.
    #[error("invalid expression in ground constant")]
    InvalidGroundConstant,

    #[error("invalid limit {0} of type {1}: expected natural number.")]
    InvalidLimit(String, ValueType),

    #[error("mismatched bindings in ground")]
    GroundBindingsMismatch,

    #[error("no entid found for ident: {0}")]
    UnrecognizedIdent(String),

    #[error("no function named {0}")]
    UnknownFunction(PlainSymbol),

    #[error(":limit var {0} not present in :in")]
    UnknownLimitVar(PlainSymbol),

    #[error("unbound variable {0} in order clause or function call")]
    UnboundVariable(PlainSymbol),

    // TODO: flesh out.
    #[error("non-matching variables in 'or' clause")]
    NonMatchingVariablesInOrClause,

    #[error("non-matching variables in 'not' clause")]
    NonMatchingVariablesInNotClause,

    #[error("binding error in {0}: {1:?}")]
    InvalidBinding(PlainSymbol, BindingError),

    #[error("{0}")]
    EdnParseError(ParseError), // TODO [#from] ?
}

impl core::convert::From<ParseError> for AlgebrizerError {
    fn from(error: ParseError) -> AlgebrizerError {
        AlgebrizerError::EdnParseError(error)
    }
}

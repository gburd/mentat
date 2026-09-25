// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

// Regression tests for query shapes that parse successfully but hit engine
// limitations. These used to panic (`unimplemented!()`); now they must return a
// typed `AlgebrizerError` instead of aborting the process.

extern crate core_traits;
extern crate edn;
extern crate mentat_core;
extern crate mentat_query_algebrizer;
extern crate query_algebrizer_traits;

mod utils;

use core_traits::{Attribute, TypedValue, ValueType};

use mentat_core::Schema;

use edn::query::{Keyword, Variable};

use query_algebrizer_traits::errors::AlgebrizerError;

use mentat_query_algebrizer::{Known, QueryInputs};

use crate::utils::{add_attribute, associate_ident, bails, bails_with_inputs};

fn prepopulated_schema() -> Schema {
    let mut schema = Schema::default();
    associate_ident(&mut schema, Keyword::namespaced("foo", "knows"), 66);
    associate_ident(&mut schema, Keyword::namespaced("foo", "age"), 68);
    associate_ident(&mut schema, Keyword::namespaced("foo", "description"), 70);
    add_attribute(
        &mut schema,
        66,
        Attribute {
            value_type: ValueType::Ref,
            multival: true,
            ..Default::default()
        },
    );
    add_attribute(
        &mut schema,
        68,
        Attribute {
            value_type: ValueType::Long,
            multival: false,
            ..Default::default()
        },
    );
    add_attribute(
        &mut schema,
        70,
        Attribute {
            value_type: ValueType::String,
            index: true,
            fulltext: true,
            multival: true,
            ..Default::default()
        },
    );
    schema
}

// A bigint constant (`123N`) in a pattern value place is not yet supported (#280).
#[test]
fn test_bigint_pattern_value_bails() {
    let schema = prepopulated_schema();
    let known = Known::for_schema(&schema);
    let q = r#"[:find ?e :where [?e :foo/age 123N]]"#;
    assert_eq!(
        bails(known, q),
        AlgebrizerError::UnsupportedBigInteger,
        "a bigint value constant should return a typed error, not panic"
    );
}

// A bigint constant in `ground` is not yet supported (#280).
#[test]
fn test_bigint_ground_bails() {
    let schema = prepopulated_schema();
    let known = Known::for_schema(&schema);
    let q = r#"[:find ?x :where [?x :foo/knows ?p] [(ground 123N) ?x]]"#;
    assert_eq!(
        bails(known, q),
        AlgebrizerError::UnsupportedBigInteger,
        "a bigint ground constant should return a typed error, not panic"
    );
}

// A bigint constant inside an `or` arm's pattern value place is not yet supported.
#[test]
fn test_bigint_or_arm_value_bails() {
    let schema = prepopulated_schema();
    let known = Known::for_schema(&schema);
    let q = r#"[:find ?e :where (or [?e :foo/age 123N] [?e :foo/age 5])]"#;
    assert_eq!(
        bails(known, q),
        AlgebrizerError::UnsupportedBigInteger,
        "a bigint value constant in an or arm should return a typed error, not panic"
    );
}

// A non-default source var (`$src`) in a pattern is not yet supported by the
// SQLite engine.
#[test]
fn test_non_default_source_pattern_bails() {
    let schema = prepopulated_schema();
    let known = Known::for_schema(&schema);
    let q = r#"[:find ?e :in $src :where [$src ?e :foo/age ?a]]"#;
    match bails(known, q) {
        AlgebrizerError::UnsupportedSource(name) => assert_eq!(name, "src"),
        e => panic!("expected UnsupportedSource, got {:?}", e),
    }
}

// Binding a fulltext value output var to an already-materialized value used to
// panic in `bind_column_to_var`; now it returns a typed binding error.
#[test]
fn test_fulltext_value_prebound_bails() {
    let schema = prepopulated_schema();
    let known = Known::for_schema(&schema);
    let q = r#"[:find ?entity
                :in ?val
                :where [(fulltext $ :foo/description "hello") [[?entity ?val ?tx ?score]]]]"#;
    let inputs = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?val"),
        TypedValue::typed_string("hello"),
    )]);
    assert!(
        matches!(
            bails_with_inputs(known, q, inputs),
            AlgebrizerError::InvalidBinding(_, _)
        ),
        "a pre-bound fulltext value var should return a typed error, not panic"
    );
}

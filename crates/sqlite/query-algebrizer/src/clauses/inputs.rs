// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use std::collections::BTreeMap;

use core_traits::{TypedValue, ValueType};

use edn::query::Variable;

use query_algebrizer_traits::errors::{AlgebrizerError, Result};

/// Define the inputs to a query. This is in two parts: a set of values known now, and a set of
/// types known now.
/// The separate map of types is to allow queries to be algebrized without full knowledge of
/// the bindings that will be used at execution time.
/// When built correctly, `types` is guaranteed to contain the types of `values` -- use
/// `QueryInputs::new` or `QueryInputs::with_values` to construct an instance.
#[derive(Default)]
pub struct QueryInputs {
    pub(crate) types: BTreeMap<Variable, ValueType>,
    pub(crate) values: BTreeMap<Variable, TypedValue>,
    /// Non-scalar `:in` inputs, keyed by the binding variable(s):
    ///   * a collection `[?x ...]` -> one entry, `[[?x], [[v0], [v1], ...]]`;
    ///   * a tuple `[?a ?b]`      -> one row of values across the vars;
    ///   * a relation `[[?a ?b]]` -> many rows.
    ///
    /// Each entry is `(vars, rows)` where every row has `vars.len()` values. The
    /// algebrizer turns these into VALUES joins for the matching binding form.
    pub(crate) collections: Vec<(Vec<Variable>, Vec<Vec<TypedValue>>)>,
}

impl QueryInputs {
    pub fn with_value_sequence(vals: Vec<(Variable, TypedValue)>) -> QueryInputs {
        let values: BTreeMap<Variable, TypedValue> = vals.into_iter().collect();
        QueryInputs::with_values(values)
    }

    pub fn with_type_sequence(types: Vec<(Variable, ValueType)>) -> QueryInputs {
        QueryInputs {
            types: types.into_iter().collect(),
            values: BTreeMap::default(),
            collections: Vec::default(),
        }
    }

    pub fn with_values(values: BTreeMap<Variable, TypedValue>) -> QueryInputs {
        QueryInputs {
            types: values
                .iter()
                .map(|(var, val)| (var.clone(), val.value_type()))
                .collect(),
            values,
            collections: Vec::default(),
        }
    }

    /// Bind a single variable to a collection of values (`:in $ [?x ...]`).
    pub fn with_collection(var: Variable, values: Vec<TypedValue>) -> QueryInputs {
        let rows = values.into_iter().map(|v| vec![v]).collect();
        QueryInputs {
            types: BTreeMap::default(),
            values: BTreeMap::default(),
            collections: vec![(vec![var], rows)],
        }
    }

    /// Bind several variables to one row of values (`:in $ [?a ?b]`).
    pub fn with_tuple(vars: Vec<Variable>, values: Vec<TypedValue>) -> QueryInputs {
        QueryInputs {
            types: BTreeMap::default(),
            values: BTreeMap::default(),
            collections: vec![(vars, vec![values])],
        }
    }

    /// Bind several variables to a table of rows (`:in $ [[?a ?b]]`).
    pub fn with_relation(vars: Vec<Variable>, rows: Vec<Vec<TypedValue>>) -> QueryInputs {
        QueryInputs {
            types: BTreeMap::default(),
            values: BTreeMap::default(),
            collections: vec![(vars, rows)],
        }
    }

    pub fn new(
        mut types: BTreeMap<Variable, ValueType>,
        values: BTreeMap<Variable, TypedValue>,
    ) -> Result<QueryInputs> {
        // Make sure that the types of the values agree with those in types, and collect.
        for (var, v) in values.iter() {
            let t = v.value_type();
            let old = types.insert(var.clone(), t);
            if let Some(old) = old {
                if old != t {
                    bail!(AlgebrizerError::InputTypeDisagreement(var.name(), old, t));
                }
            }
        }
        Ok(QueryInputs {
            types,
            values,
            collections: Vec::default(),
        })
    }
}

// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use core_traits::{TypedValue, ValueType, ValueTypeSet};

use mentat_core::{HasSchema, Schema};

use edn::query::{FnArg, NonIntegerConstant, PlainSymbol, Variable};

use crate::clauses::ConjoiningClauses;

use query_algebrizer_traits::errors::{AlgebrizerError, Result};

use crate::types::{ColumnConstraint, EmptyBecause, QueryValue};

/// Argument resolution.
impl ConjoiningClauses {
    /// A comparison predicate's variable must be one of `types`. When its type isn't known to
    /// be ([_ ?a ?v]), the SQL must also check the value's type tag, or `[(> ?v 1)]` would
    /// compare strings, keywords and refs too (#179; Mozilla #520).
    fn require_types_for_comparison(&mut self, var: &Variable, types: ValueTypeSet) {
        let check = !self.known_type_set(var).is_subset(types);
        self.narrow_types_for_var(var.clone(), types);
        if let (true, Some(qa)) = (check, self.extracted_types.get(var)) {
            let value = qa.0.clone();
            self.wheres.add_intersection(ColumnConstraint::HasTypes {
                value,
                value_types: types,
                check_value: true,
            });
        }
    }

    /// Take a function argument and turn it into a `QueryValue` suitable for use in a concrete
    /// constraint.
    /// Additionally, do two things:
    /// - Mark the pattern as known-empty if any argument is known non-numeric.
    /// - Mark any variables encountered as numeric.
    pub(crate) fn resolve_numeric_argument(
        &mut self,
        function: &PlainSymbol,
        position: usize,
        arg: FnArg,
    ) -> Result<QueryValue> {
        use self::FnArg::*;
        match arg {
            FnArg::Variable(var) => {
                // Handle incorrect types
                if let Some(v) = self.bound_value(&var) {
                    if v.value_type().is_numeric() {
                        Ok(QueryValue::TypedValue(v))
                    } else {
                        bail!(AlgebrizerError::InputTypeDisagreement(var.name(), ValueType::Long, v.value_type()))
                    }
                } else {
                    self.require_types_for_comparison(&var, ValueTypeSet::of_numeric_types());
                    self.column_bindings
                        .get(&var)
                        .and_then(|cols| cols.first().map(|col| QueryValue::Column(col.clone())))
                        .ok_or_else(|| AlgebrizerError::UnboundVariable(var.name()))
                }
            },
            // Can't be an entid.
            EntidOrInteger(i) => Ok(QueryValue::TypedValue(TypedValue::Long(i))),
            IdentOrKeyword(_) |
            SrcVar(_) |
            Constant(NonIntegerConstant::Boolean(_)) |
            Constant(NonIntegerConstant::Text(_)) |
            Constant(NonIntegerConstant::Uuid(_)) |
            Constant(NonIntegerConstant::Instant(_)) |        // Instants are covered below.
            Constant(NonIntegerConstant::BigInteger(_)) |
            Vector(_) => {
                self.mark_known_empty(EmptyBecause::NonNumericArgument);
                bail!(AlgebrizerError::InvalidArgument(function.clone(), "numeric", position))
            },
            Constant(NonIntegerConstant::Float(f)) => Ok(QueryValue::TypedValue(TypedValue::Double(f))),
        }
    }

    /// Just like `resolve_numeric_argument`, but for `ValueType::Instant`.
    pub(crate) fn resolve_instant_argument(
        &mut self,
        function: &PlainSymbol,
        position: usize,
        arg: FnArg,
    ) -> Result<QueryValue> {
        use self::FnArg::*;
        match arg {
            FnArg::Variable(var) => match self.bound_value(&var) {
                Some(TypedValue::Instant(v)) => Ok(QueryValue::TypedValue(TypedValue::Instant(v))),
                Some(v) => bail!(AlgebrizerError::InputTypeDisagreement(
                    var.name(),
                    ValueType::Instant,
                    v.value_type()
                )),
                None => {
                    self.require_types_for_comparison(
                        &var,
                        ValueTypeSet::of_one(ValueType::Instant),
                    );
                    self.column_bindings
                        .get(&var)
                        .and_then(|cols| cols.first().map(|col| QueryValue::Column(col.clone())))
                        .ok_or_else(|| AlgebrizerError::UnboundVariable(var.name()))
                }
            },
            Constant(NonIntegerConstant::Instant(v)) => {
                Ok(QueryValue::TypedValue(TypedValue::Instant(v)))
            }

            // TODO: should we allow integers if they seem to be timestamps? It's ambiguous…
            EntidOrInteger(_)
            | IdentOrKeyword(_)
            | SrcVar(_)
            | Constant(NonIntegerConstant::Boolean(_))
            | Constant(NonIntegerConstant::Float(_))
            | Constant(NonIntegerConstant::Text(_))
            | Constant(NonIntegerConstant::Uuid(_))
            | Constant(NonIntegerConstant::BigInteger(_))
            | Vector(_) => {
                self.mark_known_empty(EmptyBecause::NonInstantArgument);
                bail!(AlgebrizerError::InvalidArgumentType(
                    function.clone(),
                    ValueType::Instant.into(),
                    position
                ))
            }
        }
    }

    /// Take a function argument and turn it into a `QueryValue` suitable for use in a concrete
    /// constraint.
    pub(crate) fn resolve_ref_argument(
        &mut self,
        schema: &Schema,
        function: &PlainSymbol,
        position: usize,
        arg: FnArg,
    ) -> Result<QueryValue> {
        use self::FnArg::*;
        match arg {
            FnArg::Variable(var) => {
                self.constrain_var_to_type(var.clone(), ValueType::Ref);
                if let Some(TypedValue::Ref(e)) = self.bound_value(&var) {
                    // Incorrect types will be handled by the constraint, above.
                    Ok(QueryValue::Entid(e))
                } else {
                    self.column_bindings
                        .get(&var)
                        .and_then(|cols| cols.first().map(|col| QueryValue::Column(col.clone())))
                        .ok_or_else(|| AlgebrizerError::UnboundVariable(var.name()))
                }
            }
            EntidOrInteger(i) => Ok(QueryValue::TypedValue(TypedValue::Ref(i))),
            IdentOrKeyword(i) => schema
                .get_entid(&i)
                .map(|known_entid| QueryValue::Entid(known_entid.into()))
                .ok_or_else(|| AlgebrizerError::UnrecognizedIdent(i.to_string())),
            Constant(NonIntegerConstant::Boolean(_))
            | Constant(NonIntegerConstant::Float(_))
            | Constant(NonIntegerConstant::Text(_))
            | Constant(NonIntegerConstant::Uuid(_))
            | Constant(NonIntegerConstant::Instant(_))
            | Constant(NonIntegerConstant::BigInteger(_))
            | SrcVar(_)
            | Vector(_) => {
                self.mark_known_empty(EmptyBecause::NonEntityArgument);
                bail!(AlgebrizerError::InvalidArgumentType(
                    function.clone(),
                    ValueType::Ref.into(),
                    position
                ))
            }
        }
    }

    /// Take a transaction ID function argument and turn it into a `QueryValue` suitable for use in
    /// a concrete constraint.
    pub(crate) fn resolve_tx_argument(
        &mut self,
        schema: &Schema,
        function: &PlainSymbol,
        position: usize,
        arg: FnArg,
    ) -> Result<QueryValue> {
        // Under the hood there's nothing special about a transaction ID -- it's just another ref.
        // In the future, we might handle instants specially.
        self.resolve_ref_argument(schema, function, position, arg)
    }
}

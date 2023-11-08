use crate::expression::value_compute::{binary_op, unary_op};
use crate::expression::ScalarExpression;
use crate::types::errors::TypeError;
use crate::types::tuple::Tuple;
use crate::types::value::{DataValue, ValueRef};
use itertools::Itertools;
use lazy_static::lazy_static;
use std::sync::Arc;

lazy_static! {
    static ref NULL_VALUE: ValueRef = Arc::new(DataValue::Null);
}

impl ScalarExpression {
    pub fn eval_column(&self, tuple: &Tuple) -> Result<ValueRef, TypeError> {
        if let Some(value) = Self::eval_with_name(&tuple, self.output_columns().name()) {
            return Ok(value.clone());
        }

        match &self {
            ScalarExpression::Constant(val) => Ok(val.clone()),
            ScalarExpression::ColumnRef(col) => {
                let value = Self::eval_with_name(&tuple, col.name())
                    .unwrap_or(&NULL_VALUE)
                    .clone();

                Ok(value)
            }
            ScalarExpression::Alias { expr, alias } => {
                if let Some(value) = Self::eval_with_name(&tuple, alias) {
                    return Ok(value.clone());
                }

                expr.eval_column(tuple)
            }
            ScalarExpression::TypeCast { expr, ty, .. } => {
                let value = expr.eval_column(tuple)?;

                Ok(Arc::new(DataValue::clone(&value).cast(ty)?))
            }
            ScalarExpression::Binary {
                left_expr,
                right_expr,
                op,
                ..
            } => {
                let left = left_expr.eval_column(tuple)?;
                let right = right_expr.eval_column(tuple)?;

                Ok(Arc::new(binary_op(&left, &right, op)?))
            }
            ScalarExpression::IsNull { expr, negated } => {
                let mut value = expr.eval_column(tuple)?.is_null();

                if *negated {
                    value = !value;
                }
                Ok(Arc::new(DataValue::Boolean(Some(value))))
            }
            ScalarExpression::Unary { expr, op, .. } => {
                let value = expr.eval_column(tuple)?;

                Ok(Arc::new(unary_op(&value, op)?))
            }
            ScalarExpression::AggCall { .. } => {
                let value = Self::eval_with_name(&tuple, self.output_columns().name())
                    .unwrap_or(&NULL_VALUE)
                    .clone();

                Ok(value)
            },
        }
    }

    fn eval_with_name<'a>(tuple: &'a Tuple, name: &str) -> Option<&'a ValueRef> {
        tuple
            .columns
            .iter()
            .find_position(|tul_col| tul_col.name() == name)
            .map(|(i, _)| &tuple.values[i])
    }
}

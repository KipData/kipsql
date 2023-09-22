use std::cmp::Ordering;
use std::collections::{Bound, HashSet};
use std::mem;
use std::sync::Arc;
use ahash::RandomState;
use itertools::Itertools;
use crate::catalog::ColumnRef;
use crate::expression::{BinaryOperator, ScalarExpression, UnaryOperator};
use crate::expression::value_compute::{binary_op, unary_op};
use crate::types::{ColumnId, LogicalType};
use crate::types::errors::TypeError;
use crate::types::value::{DataValue, ValueRef};

#[derive(Debug, PartialEq, Clone)]
pub enum ConstantBinary {
    Scope {
        min: Bound<ValueRef>,
        max: Bound<ValueRef>
    },
    Eq(ValueRef),
    NotEq(ValueRef),

    // ConstantBinary in And can only be Scope\Eq\NotEq
    And(Vec<ConstantBinary>),
    // ConstantBinary in Or can only be Scope\Eq\NotEq\And
    Or(Vec<ConstantBinary>)
}

impl ConstantBinary {
    fn is_null(&self) -> Result<bool, TypeError> {
        match self {
            ConstantBinary::Scope { min, max } => {
                let op = |bound: &Bound<ValueRef>| {
                    if let Bound::Included(val) | Bound::Excluded(val) = bound {
                        val.is_null()
                    } else {
                        false
                    }
                };
                if op(min) || op(max) {
                    return Ok(true);
                }

                Ok(matches!((min, max), (Bound::Unbounded, Bound::Unbounded)))
            },
            ConstantBinary::Eq(val) | ConstantBinary::NotEq(val) => Ok(val.is_null()),
            _ => Err(TypeError::InvalidType),
        }
    }

    pub fn rearrange(self) -> Result<Vec<ConstantBinary>, TypeError> {
        match self {
            ConstantBinary::Or(mut binaries) => {
                let mut condition_binaries = Vec::new();

                for binary in binaries {
                    match binary {
                        ConstantBinary::Or(_) => return Err(TypeError::InvalidType),
                        ConstantBinary::And(mut and_binaries) => {
                            condition_binaries.append(&mut and_binaries);
                        }
                        ConstantBinary::Scope { min: Bound::Unbounded, max: Bound::Unbounded } => (),
                        source => condition_binaries.push(source),
                    }
                }
                // Sort
                condition_binaries.sort_by(|a, b| {
                    let op = |binary: &ConstantBinary| {
                        match binary {
                            ConstantBinary::Scope { min, .. } => min.clone(),
                            ConstantBinary::Eq(val) => Bound::Included(val.clone()),
                            ConstantBinary::NotEq(val) => Bound::Excluded(val.clone()),
                            _ => unreachable!()
                        }
                    };

                    Self::bound_compared(&op(a), &op(b), true)
                        .unwrap_or(Ordering::Equal)
                });

                let mut merged_binaries: Vec<ConstantBinary> = Vec::new();

                for condition in condition_binaries {
                    let op = |binary: &ConstantBinary| {
                        match binary {
                            ConstantBinary::Scope { min, max } => (min.clone(), max.clone()),
                            ConstantBinary::Eq(val) => (Bound::Unbounded, Bound::Included(val.clone())),
                            ConstantBinary::NotEq(val) => (Bound::Unbounded, Bound::Excluded(val.clone())),
                            _ => unreachable!()
                        }
                    };
                    let mut is_push = merged_binaries.is_empty();

                    for binary in merged_binaries.iter_mut().rev() {
                        if let ConstantBinary::Scope { max, .. } = binary {
                            let (condition_min, condition_max) = op(&condition);
                            let is_lt_min = Self::bound_compared(max, &condition_min, false)
                                .unwrap_or(Ordering::Equal)
                                .is_lt();
                            let is_lt_max = Self::bound_compared(max, &condition_max, false)
                                .unwrap_or(Ordering::Equal)
                                .is_lt();

                            if !is_lt_min && is_lt_max {
                                let _ = mem::replace(max, condition_max);
                            } else if !matches!(condition, ConstantBinary::Scope {..}) {
                                is_push = is_lt_max;
                            } else if is_lt_min && is_lt_max {
                                is_push = true
                            }

                            break
                        }
                    }

                    if is_push {
                        merged_binaries.push(condition);
                    }
                }

                Ok(merged_binaries)
            },
            ConstantBinary::And(binaries) => Ok(binaries),
            source => Ok(vec![source]),
        }
    }

    pub fn scope_aggregation(&mut self) -> Result<(), TypeError> {
        match self {
            ConstantBinary::And(binaries) => {
                Self::_scope_aggregation(binaries)?
            }
            ConstantBinary::Or(binaries) => {
                for binary in binaries {
                    binary.scope_aggregation()?
                }
            }
            _ => ()
        }

        Ok(())
    }

    fn bound_compared(left_bound: &Bound<ValueRef>, right_bound: &Bound<ValueRef>, is_min: bool) -> Option<Ordering> {
        let op = |is_min, order: Ordering| {
            if is_min {
                order
            } else {
                order.reverse()
            }
        };

        match (left_bound, right_bound) {
            (Bound::Unbounded, Bound::Unbounded) => Some(Ordering::Equal),
            (Bound::Unbounded, _) => Some(op(is_min, Ordering::Less)),
            (_, Bound::Unbounded) => Some(op(is_min, Ordering::Greater)),
            (Bound::Included(left), Bound::Included(right)) => left.partial_cmp(right),
            (Bound::Included(left), Bound::Excluded(right)) => {
                left.partial_cmp(right)
                    .map(|order| order.then(op(is_min, Ordering::Less)))
            },
            (Bound::Excluded(left), Bound::Excluded(right)) => left.partial_cmp(right),
            (Bound::Excluded(left), Bound::Included(right)) => {
                left.partial_cmp(right)
                    .map(|order| order.then(op(is_min, Ordering::Greater)))
            },
        }
    }

    // Tips: It only makes sense if the condition is and aggregation
    fn _scope_aggregation(binaries: &mut Vec<ConstantBinary>) -> Result<(), TypeError> {
        let mut scope_min = Bound::Unbounded;
        let mut scope_max = Bound::Unbounded;
        let mut eq_set = HashSet::with_hasher(RandomState::new());

        let sort_op = |binary: &&ConstantBinary| {
            match binary {
                ConstantBinary::Scope { .. } => 3,
                ConstantBinary::NotEq(_) => 2,
                ConstantBinary::Eq(_) => 1,
                ConstantBinary::And(_) | ConstantBinary::Or(_) => 0
            }
        };

        // Aggregate various ranges to get the minimum range
        for binary in binaries.iter().sorted_by_key(sort_op) {
            match binary {
                ConstantBinary::Scope { min, max } => {
                    // Skip if eq or noteq exists
                    if !eq_set.is_empty() { continue }

                    if let Some(order) = Self::bound_compared(&scope_min, &min, true) {
                        if order.is_lt() {
                            scope_min = min.clone();
                        }
                    }

                    if let Some(order) = Self::bound_compared(&scope_max, &max, false) {
                        if order.is_gt() {
                            scope_max = max.clone();
                        }
                    }
                }
                ConstantBinary::Eq(val) => {
                    let _ = eq_set.insert(val.clone());
                },
                ConstantBinary::NotEq(val) => {
                    let _ = eq_set.remove(val);
                },
                ConstantBinary::Or(_) | ConstantBinary::And(_) => return Err(TypeError::InvalidType)
            }
        }

        let eq_binaries = eq_set.into_iter()
            .sorted_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .map(|val| ConstantBinary::Eq(val))
            .collect_vec();

        if !eq_binaries.is_empty() {
            let _ = mem::replace(binaries, eq_binaries);
        } else if !matches!((&scope_min, &scope_max), (Bound::Unbounded, Bound::Unbounded)) {
            let scope_binary = ConstantBinary::Scope {
                min: scope_min,
                max: scope_max,
            };

            let _ = mem::replace(binaries, vec![scope_binary]);
        } else {
            let _ = mem::replace(binaries, vec![]);
        }

        Ok(())
    }
}

enum Replace {
    Binary(ReplaceBinary),
    Unary(ReplaceUnary),
}

struct ReplaceBinary {
    column_expr: ScalarExpression,
    val_expr: ScalarExpression,
    op: BinaryOperator,
    ty: LogicalType,
    is_column_left: bool
}

struct ReplaceUnary {
    child_expr: ScalarExpression,
    op: UnaryOperator,
    ty: LogicalType,
}

impl ScalarExpression {
    fn unpack_val(&mut self) -> Option<ValueRef> {
        match self {
            ScalarExpression::Constant(val) => Some(val.clone()),
            ScalarExpression::Alias { expr, .. } => expr.unpack_val(),
            ScalarExpression::TypeCast { expr, ty, .. } => {
                expr.unpack_val()
                    .and_then(|val| DataValue::clone(&val)
                        .cast(ty).ok()
                        .map(Arc::new))
            }
            ScalarExpression::IsNull { expr } => {
                let is_null = expr.unpack_val().map(|val| val.is_null());

                Some(Arc::new(DataValue::Boolean(is_null)))
            },
            ScalarExpression::Unary { expr, op, .. } => {
                let val = expr.unpack_val()?;

                unary_op(&val, op).ok()
                    .map(Arc::new)

            }
            ScalarExpression::Binary { left_expr, right_expr, op, .. } => {
                let left = left_expr.unpack_val()?;
                let right = right_expr.unpack_val()?;

                binary_op(&left, &right, op).ok()
                    .map(|val| {
                        let val_ref = Arc::new(val);
                        let _ = mem::replace(
                            self,
                            ScalarExpression::Constant(val_ref.clone()),
                        );

                        val_ref
                    })
            }
            _ => None
        }
    }

    fn unpack_col(&self, is_binary_then_return: bool) -> Option<ColumnRef> {
        match self {
            ScalarExpression::ColumnRef(col) => Some(col.clone()),
            ScalarExpression::Alias { expr, .. } => expr.unpack_col(is_binary_then_return),
            ScalarExpression::Binary { left_expr, right_expr, .. } => {
                if is_binary_then_return {
                    return None;
                }

                match (left_expr.unpack_col(is_binary_then_return),
                       right_expr.unpack_col(is_binary_then_return))
                {
                    (Some(col), None) | (None, Some(col)) => Some(col),
                    _ => None
                }
            }
            ScalarExpression::Unary { expr, .. } => expr.unpack_col(is_binary_then_return),
            _ => None
        }
    }

    pub fn simplify(&mut self) -> Result<(), TypeError> {
        self._simplify(&mut None)
    }

    // Tips: Indirect expressions like `ScalarExpression:：Alias` will be lost
    fn _simplify(&mut self, fix_option: &mut Option<Replace>) -> Result<(), TypeError> {
        match self {
            ScalarExpression::Binary { left_expr, right_expr, op, ty } => {
                Self::fix_expr(fix_option, left_expr, right_expr, op)?;

                // `(c1 - 1) and (c1 + 2)` cannot fix!
                Self::fix_expr(fix_option, right_expr, left_expr, op)?;

                if matches!(op, BinaryOperator::Plus | BinaryOperator::Divide
                    | BinaryOperator::Minus | BinaryOperator::Multiply)
                {
                    match (left_expr.unpack_col(true), right_expr.unpack_col(true)) {
                        (Some(_), Some(_)) => (),
                        (Some(col), None) => {
                            fix_option.replace(Replace::Binary(ReplaceBinary{
                                column_expr: ScalarExpression::ColumnRef(col),
                                val_expr: right_expr.as_ref().clone(),
                                op: *op,
                                ty: *ty,
                                is_column_left: true,
                            }));
                        }
                        (None, Some(col)) => {
                            fix_option.replace(Replace::Binary(ReplaceBinary{
                                column_expr: ScalarExpression::ColumnRef(col),
                                val_expr: left_expr.as_ref().clone(),
                                op: *op,
                                ty: *ty,
                                is_column_left: false,
                            }));
                        }
                        _ => ()
                    }
                }
            }
            ScalarExpression::Alias { expr, .. } => expr._simplify(fix_option)?,
            ScalarExpression::TypeCast { expr, .. } => {
                if let Some(val) = expr.unpack_val() {
                    let _ = mem::replace(self, ScalarExpression::Constant(val));
                }
            },
            ScalarExpression::IsNull { expr, .. } => {
                if let Some(val) = expr.unpack_val() {
                    let _ = mem::replace(self, ScalarExpression::Constant(
                        Arc::new(DataValue::Boolean(Some(val.is_null())))
                    ));
                }
            },
            ScalarExpression::Unary { expr, op, ty } => {
                if let Some(val) = expr.unpack_val() {
                    let new_expr = ScalarExpression::Constant(
                        Arc::new(unary_op(&val, op)?)
                    );
                    let _ = mem::replace(self, new_expr);
                } else {
                    let _ = fix_option.replace(Replace::Unary(
                        ReplaceUnary {
                            child_expr: expr.as_ref().clone(),
                            op: *op,
                            ty: *ty,
                        }
                    ));
                }
            },
            _ => ()
        }

        Ok(())
    }

    fn fix_expr(
        fix_option: &mut Option<Replace>,
        left_expr: &mut Box<ScalarExpression>,
        right_expr: &mut Box<ScalarExpression>,
        op: &mut BinaryOperator,
    ) -> Result<(), TypeError> {
        left_expr._simplify(fix_option)?;

        if let Some(replace) = fix_option.take() {
            match replace {
                Replace::Binary(binary) => Self::fix_binary(binary, left_expr, right_expr, op),
                Replace::Unary(unary) => {
                    Self::fix_unary(unary, left_expr, right_expr, op);
                    Self::fix_expr(fix_option, left_expr, right_expr, op)?;
                },
            }
        }
        Ok(())
    }

    fn fix_unary(
        replace_unary: ReplaceUnary,
        col_expr: &mut Box<ScalarExpression>,
        val_expr: &mut Box<ScalarExpression>,
        op: &mut BinaryOperator
    ) {
        let ReplaceUnary { child_expr, op: fix_op, ty: fix_ty } = replace_unary;
        let _ = mem::replace(col_expr, Box::new(child_expr));
        let _ = mem::replace(val_expr, Box::new(ScalarExpression::Unary {
            op: fix_op,
            expr: val_expr.clone(),
            ty: fix_ty,
        }));
        let _ = mem::replace(op, match fix_op {
            UnaryOperator::Plus => *op,
            UnaryOperator::Minus => {
                match *op {
                    BinaryOperator::Plus => BinaryOperator::Minus,
                    BinaryOperator::Minus => BinaryOperator::Plus,
                    BinaryOperator::Multiply => BinaryOperator::Divide,
                    BinaryOperator::Divide => BinaryOperator::Multiply,
                    BinaryOperator::Gt => BinaryOperator::Lt,
                    BinaryOperator::Lt => BinaryOperator::Gt,
                    BinaryOperator::GtEq => BinaryOperator::LtEq,
                    BinaryOperator::LtEq => BinaryOperator::GtEq,
                    source_op => source_op
                }
            }
            UnaryOperator::Not => {
                match *op {
                    BinaryOperator::Gt => BinaryOperator::Lt,
                    BinaryOperator::Lt => BinaryOperator::Gt,
                    BinaryOperator::GtEq => BinaryOperator::LtEq,
                    BinaryOperator::LtEq => BinaryOperator::GtEq,
                    source_op => source_op
                }
            }
        });
    }

    fn fix_binary(
        replace_binary: ReplaceBinary,
        left_expr: &mut Box<ScalarExpression>,
        right_expr: &mut Box<ScalarExpression>,
        op: &mut BinaryOperator
    ) {
        let ReplaceBinary { column_expr, val_expr, op: fix_op, ty: fix_ty, is_column_left } = replace_binary;
        let op_flip = |op: BinaryOperator| {
            match op {
                BinaryOperator::Plus => BinaryOperator::Minus,
                BinaryOperator::Minus => BinaryOperator::Plus,
                BinaryOperator::Multiply => BinaryOperator::Divide,
                BinaryOperator::Divide => BinaryOperator::Multiply,
                _ => unreachable!()
            }
        };
        let comparison_flip = |op: BinaryOperator| {
            match op {
                BinaryOperator::Gt => BinaryOperator::Lt,
                BinaryOperator::GtEq => BinaryOperator::LtEq,
                BinaryOperator::Lt => BinaryOperator::Gt,
                BinaryOperator::LtEq => BinaryOperator::GtEq,
                source_op => source_op
            }
        };
        let (fixed_op, fixed_left_expr, fixed_right_expr) = if is_column_left {
            (op_flip(fix_op), right_expr.clone(), Box::new(val_expr))
        } else {
            if matches!(fix_op, BinaryOperator::Minus | BinaryOperator::Multiply) {
                let _ = mem::replace(op, comparison_flip(*op));
            }
            (fix_op, Box::new(val_expr), right_expr.clone())
        };

        let _ = mem::replace(left_expr, Box::new(column_expr));
        let _ = mem::replace(right_expr, Box::new(ScalarExpression::Binary {
            op: fixed_op,
            left_expr: fixed_left_expr,
            right_expr: fixed_right_expr,
            ty: fix_ty,
        }));
    }

    /// The definition of Or is not the Or in the Where condition.
    /// The And and Or of ConstantBinary are concerned with the data range that needs to be aggregated.
    /// - `ConstantBinary::And`: Aggregate the minimum range of all conditions in and
    /// - `ConstantBinary::Or`: Rearrange and sort the range of each OR data
    pub fn convert_binary(&mut self, col_id: &ColumnId) -> Result<Option<ConstantBinary>, TypeError> {
        match self {
            ScalarExpression::Binary { left_expr, right_expr, op, .. } => {
                match (left_expr.convert_binary(col_id)?, right_expr.convert_binary(col_id)?) {
                    (Some(left_binary), Some(right_binary)) => {
                        match (left_binary, right_binary) {
                            (ConstantBinary::And(mut left), ConstantBinary::And(mut right))
                            | (ConstantBinary::Or(mut left), ConstantBinary::Or(mut right)) => {
                                left.append(&mut right);

                                Ok(Some(ConstantBinary::And(left)))
                            }
                            (ConstantBinary::And(mut left), ConstantBinary::Or(mut right)) => {
                                right.append(&mut left);

                                Ok(Some(ConstantBinary::Or(right)))
                            }
                            (ConstantBinary::Or(mut left), ConstantBinary::And(mut right)) => {
                                left.append(&mut right);

                                Ok(Some(ConstantBinary::Or(left)))
                            }
                            (ConstantBinary::And(mut binaries), binary)
                            | (binary, ConstantBinary::And(mut binaries)) => {
                                binaries.push(binary);

                                Ok(Some(ConstantBinary::And(binaries)))
                            }
                            (ConstantBinary::Or(mut binaries), binary)
                            | (binary, ConstantBinary::Or(mut binaries)) => {
                                binaries.push(binary);

                                Ok(Some(ConstantBinary::Or(binaries)))
                            }
                            (left, right) => {
                                match op {
                                    BinaryOperator::And => {
                                        Ok(Some(ConstantBinary::And(vec![left, right])))
                                    }
                                    BinaryOperator::Or => {
                                        Ok(Some(ConstantBinary::Or(vec![left, right])))
                                    }
                                    BinaryOperator::Xor => todo!(),
                                    _ => Ok(None)
                                }
                            }
                        }
                    },
                    (None, None) => {
                        if let (Some(col), Some(val)) =
                            (left_expr.unpack_col(false), right_expr.unpack_val())
                        {
                            return Ok(Self::new_binary(col_id, *op, col, val, false));
                        }
                        if let (Some(val), Some(col)) =
                            (left_expr.unpack_val(), right_expr.unpack_col(false))
                        {
                            return Ok(Self::new_binary(col_id, *op, col, val, true));
                        }

                        return Ok(None);
                    }
                    (Some(binary), None) | (None, Some(binary)) => return Ok(Some(binary)),
                }
            },
            ScalarExpression::Alias { expr, .. } => expr.convert_binary(col_id),
            ScalarExpression::TypeCast { expr, .. } => expr.convert_binary(col_id),
            ScalarExpression::IsNull { expr } => expr.convert_binary(col_id),
            ScalarExpression::Unary { expr, .. } => expr.convert_binary(col_id),
            _ => Ok(None),
        }
    }

    fn new_binary(col_id: &ColumnId, mut op: BinaryOperator, col: ColumnRef, val: ValueRef, is_flip: bool) -> Option<ConstantBinary> {
        if col.id.unwrap() != *col_id {
            return None;
        }

        if is_flip {
            op = match op {
                BinaryOperator::Gt => BinaryOperator::Lt,
                BinaryOperator::Lt => BinaryOperator::Gt,
                BinaryOperator::GtEq => BinaryOperator::LtEq,
                BinaryOperator::LtEq => BinaryOperator::GtEq,
                source_op => source_op
            };
        }

        match op {
            BinaryOperator::Gt => {
                Some(ConstantBinary::Scope {
                    min: Bound::Excluded(val.clone()),
                    max: Bound::Unbounded
                })
            }
            BinaryOperator::Lt => {
                Some(ConstantBinary::Scope {
                    min: Bound::Unbounded,
                    max: Bound::Excluded(val.clone()),
                })
            }
            BinaryOperator::GtEq => {
                Some(ConstantBinary::Scope {
                    min: Bound::Included(val.clone()),
                    max: Bound::Unbounded
                })
            }
            BinaryOperator::LtEq => {
                Some(ConstantBinary::Scope {
                    min: Bound::Unbounded,
                    max: Bound::Included(val.clone()),
                })
            }
            BinaryOperator::Eq | BinaryOperator::Spaceship => {
                Some(ConstantBinary::Eq(val.clone()))
            },
            BinaryOperator::NotEq => {
                Some(ConstantBinary::NotEq(val.clone()))
            },
            _ => None
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::Bound;
    use std::sync::Arc;
    use crate::catalog::{ColumnCatalog, ColumnDesc};
    use crate::expression::{BinaryOperator, ScalarExpression};
    use crate::expression::simplify::ConstantBinary;
    use crate::types::errors::TypeError;
    use crate::types::LogicalType;
    use crate::types::value::DataValue;

    fn build_test_expr() -> (ScalarExpression, ScalarExpression) {
        let col_1 = Arc::new(ColumnCatalog {
            id: Some(0),
            name: "c1".to_string(),
            table_name: None,
            nullable: false,
            desc: ColumnDesc {
                column_datatype: LogicalType::Integer,
                is_primary: false,
                is_unique: false,
            },
        });

        let c1_main_binary_expr = ScalarExpression::Binary {
            op: BinaryOperator::Minus,
            left_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            right_expr: Box::new(ScalarExpression::Constant(Arc::new(DataValue::Int32(Some(1))))),
            ty: LogicalType::Integer,
        };
        let val_main_binary_expr = ScalarExpression::Binary {
            op: BinaryOperator::Minus,
            left_expr: Box::new(ScalarExpression::Constant(Arc::new(DataValue::Int32(Some(1))))),
            right_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            ty: LogicalType::Integer,
        };

        let comparison_expr_op = |expr| {
            ScalarExpression::Binary {
                op: BinaryOperator::GtEq,
                left_expr: Box::new(ScalarExpression::Alias {
                    expr: Box::from(expr),
                    alias: "alias".to_string(),
                }),
                right_expr: Box::new(ScalarExpression::Constant(Arc::new(DataValue::Int32(Some(2))))),
                ty: LogicalType::Boolean,
            }
        };
        // c1 - 1 >= 2
        let c1_main_expr = comparison_expr_op(c1_main_binary_expr);
        // 1 - c1 >= 2
        let val_main_expr = comparison_expr_op(val_main_binary_expr);

        (c1_main_expr, val_main_expr)
    }

    #[test]
    fn test_convert_binary_simple() -> Result<(), TypeError> {
        let col_1 = Arc::new(ColumnCatalog {
            id: Some(0),
            name: "c1".to_string(),
            table_name: None,
            nullable: false,
            desc: ColumnDesc {
                column_datatype: LogicalType::Integer,
                is_primary: false,
                is_unique: false,
            },
        });
        let val_1 = Arc::new(DataValue::Int32(Some(1)));

        let binary_eq = ScalarExpression::Binary {
            op: BinaryOperator::Eq,
            left_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            right_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_eq, ConstantBinary::Eq(val_1.clone()));

        let binary_not_eq = ScalarExpression::Binary {
            op: BinaryOperator::NotEq,
            left_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            right_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_not_eq, ConstantBinary::NotEq(val_1.clone()));

        let binary_lt = ScalarExpression::Binary {
            op: BinaryOperator::Lt,
            left_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            right_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_lt, ConstantBinary::Scope {
            min: Bound::Unbounded,
            max: Bound::Excluded(val_1.clone())
        });

        let binary_lteq = ScalarExpression::Binary {
            op: BinaryOperator::LtEq,
            left_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            right_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_lteq, ConstantBinary::Scope {
            min: Bound::Unbounded,
            max: Bound::Included(val_1.clone())
        });

        let binary_gt = ScalarExpression::Binary {
            op: BinaryOperator::Gt,
            left_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            right_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_gt, ConstantBinary::Scope {
            min: Bound::Excluded(val_1.clone()),
            max: Bound::Unbounded
        });

        let binary_gteq = ScalarExpression::Binary {
            op: BinaryOperator::GtEq,
            left_expr: Box::new(ScalarExpression::ColumnRef(col_1.clone())),
            right_expr: Box::new(ScalarExpression::Constant(val_1.clone())),
            ty: LogicalType::Boolean,
        }.convert_binary(&0)?.unwrap();

        assert_eq!(binary_gteq, ConstantBinary::Scope {
            min: Bound::Included(val_1.clone()),
            max: Bound::Unbounded
        });

        Ok(())
    }

    #[test]
    fn test_scope_aggregation_eq_noteq() -> Result<(), TypeError> {
        let val_0 = Arc::new(DataValue::Int32(Some(0)));
        let val_1 = Arc::new(DataValue::Int32(Some(1)));
        let val_2 = Arc::new(DataValue::Int32(Some(2)));
        let val_3 = Arc::new(DataValue::Int32(Some(3)));

        let mut binary = ConstantBinary::And(vec![
            ConstantBinary::Eq(val_0.clone()),
            ConstantBinary::NotEq(val_1.clone()),
            ConstantBinary::Eq(val_2.clone()),
            ConstantBinary::NotEq(val_3.clone()),
        ]);

        binary.scope_aggregation()?;

        assert_eq!(
            binary,
            ConstantBinary::And(vec![
                ConstantBinary::Eq(val_0),
                ConstantBinary::Eq(val_2),
            ])
        );

        Ok(())
    }

    #[test]
    fn test_scope_aggregation_eq_noteq_cover() -> Result<(), TypeError> {
        let val_0 = Arc::new(DataValue::Int32(Some(0)));
        let val_1 = Arc::new(DataValue::Int32(Some(1)));
        let val_2 = Arc::new(DataValue::Int32(Some(2)));
        let val_3 = Arc::new(DataValue::Int32(Some(3)));

        let mut binary = ConstantBinary::And(vec![
            ConstantBinary::Eq(val_0.clone()),
            ConstantBinary::NotEq(val_1.clone()),
            ConstantBinary::Eq(val_2.clone()),
            ConstantBinary::NotEq(val_3.clone()),

            ConstantBinary::NotEq(val_0.clone()),
            ConstantBinary::NotEq(val_1.clone()),
            ConstantBinary::NotEq(val_2.clone()),
            ConstantBinary::NotEq(val_3.clone()),
        ]);

        binary.scope_aggregation()?;

        assert_eq!(
            binary,
            ConstantBinary::And(vec![])
        );

        Ok(())
    }

    #[test]
    fn test_scope_aggregation_scope() -> Result<(), TypeError> {
        let val_0 = Arc::new(DataValue::Int32(Some(0)));
        let val_1 = Arc::new(DataValue::Int32(Some(1)));
        let val_2 = Arc::new(DataValue::Int32(Some(2)));
        let val_3 = Arc::new(DataValue::Int32(Some(3)));

        let mut binary = ConstantBinary::And(vec![
            ConstantBinary::Scope {
                min: Bound::Excluded(val_0.clone()),
                max: Bound::Included(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_1.clone()),
                max: Bound::Excluded(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Excluded(val_1.clone()),
                max: Bound::Included(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_0.clone()),
                max: Bound::Excluded(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Unbounded,
                max: Bound::Unbounded,
            },
        ]);

        binary.scope_aggregation()?;

        assert_eq!(
            binary,
            ConstantBinary::And(vec![
                ConstantBinary::Scope {
                    min: Bound::Excluded(val_1.clone()),
                    max: Bound::Excluded(val_2.clone()),
                }
            ])
        );

        Ok(())
    }

    #[test]
    fn test_scope_aggregation_mixed() -> Result<(), TypeError> {
        let val_0 = Arc::new(DataValue::Int32(Some(0)));
        let val_1 = Arc::new(DataValue::Int32(Some(1)));
        let val_2 = Arc::new(DataValue::Int32(Some(2)));
        let val_3 = Arc::new(DataValue::Int32(Some(3)));

        let mut binary = ConstantBinary::And(vec![
            ConstantBinary::Scope {
                min: Bound::Excluded(val_0.clone()),
                max: Bound::Included(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_1.clone()),
                max: Bound::Excluded(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Excluded(val_1.clone()),
                max: Bound::Included(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_0.clone()),
                max: Bound::Excluded(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Unbounded,
                max: Bound::Unbounded,
            },
            ConstantBinary::Eq(val_1.clone()),
            ConstantBinary::Eq(val_0.clone()),
            ConstantBinary::NotEq(val_1.clone()),
        ]);

        binary.scope_aggregation()?;

        assert_eq!(
            binary,
            ConstantBinary::And(vec![
                ConstantBinary::Eq(val_0.clone()),
            ])
        );

        Ok(())
    }

    #[test]
    fn test_rearrange() -> Result<(), TypeError> {
        let val_0 = Arc::new(DataValue::Int32(Some(0)));
        let val_1 = Arc::new(DataValue::Int32(Some(1)));
        let val_2 = Arc::new(DataValue::Int32(Some(2)));
        let val_3 = Arc::new(DataValue::Int32(Some(3)));

        let val_5 = Arc::new(DataValue::Int32(Some(5)));

        let val_6 = Arc::new(DataValue::Int32(Some(6)));
        let val_7 = Arc::new(DataValue::Int32(Some(7)));
        let val_8 = Arc::new(DataValue::Int32(Some(8)));

        let val_10 = Arc::new(DataValue::Int32(Some(10)));

        let binary = ConstantBinary::Or(vec![
            ConstantBinary::Scope {
                min: Bound::Excluded(val_6.clone()),
                max: Bound::Included(val_10.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Excluded(val_0.clone()),
                max: Bound::Included(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_1.clone()),
                max: Bound::Excluded(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Excluded(val_1.clone()),
                max: Bound::Included(val_2.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_0.clone()),
                max: Bound::Excluded(val_3.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Included(val_6.clone()),
                max: Bound::Included(val_7.clone())
            },
            ConstantBinary::Scope {
                min: Bound::Unbounded,
                max: Bound::Unbounded,
            },
            ConstantBinary::NotEq(val_8.clone()),
            ConstantBinary::Eq(val_5.clone()),
            ConstantBinary::Eq(val_0.clone()),
            ConstantBinary::Eq(val_1.clone()),
        ]);

        assert_eq!(
            binary.rearrange()?,
            vec![
                ConstantBinary::Scope {
                    min: Bound::Included(val_0.clone()),
                    max: Bound::Included(val_3.clone()),
                },
                ConstantBinary::Eq(val_5.clone()),
                ConstantBinary::Scope {
                    min: Bound::Included(val_6.clone()),
                    max: Bound::Included(val_10.clone()),
                }
            ]
        );

        Ok(())
    }
}
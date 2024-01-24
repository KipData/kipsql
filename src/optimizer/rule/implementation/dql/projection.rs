use lazy_static::lazy_static;
use crate::optimizer::core::memo::{Expression, GroupExpression};
use crate::optimizer::core::rule::{ImplementationRule, MatchPattern};
use crate::planner::operator::{Operator, PhysicalOption};
use crate::optimizer::core::pattern::{Pattern, PatternChildrenPredicate};
use crate::optimizer::OptimizerError;
use crate::single_mapping;

lazy_static! {
    static ref PROJECTION_PATTERN: Pattern = {
        Pattern {
            predicate: |op| matches!(op, Operator::Project(_)),
            children: PatternChildrenPredicate::None,
        }
    };
}

#[derive(Clone)]
pub struct ProjectionImplementation;

single_mapping!(ProjectionImplementation, PROJECTION_PATTERN, PhysicalOption::Project);
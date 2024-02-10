use crate::binder::{Binder, QueryBindStep};
use crate::expression::ScalarExpression;
use crate::planner::operator::aggregate::AggregateOperator;
use crate::planner::LogicalPlan;
use crate::storage::Transaction;

impl<'a, T: Transaction> Binder<'a, T> {
    pub fn bind_distinct(
        &mut self,
        children: LogicalPlan,
        select_list: Vec<ScalarExpression>,
    ) -> LogicalPlan {
        self.context.step(QueryBindStep::Distinct);

        AggregateOperator::build(children, vec![], select_list)
    }
}

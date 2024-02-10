use crate::binder::Binder;
use crate::errors::DatabaseError;
use crate::planner::operator::Operator;
use crate::planner::LogicalPlan;
use crate::storage::Transaction;

impl<'a, T: Transaction> Binder<'a, T> {
    pub(crate) fn bind_show_tables(&mut self) -> Result<LogicalPlan, DatabaseError> {
        Ok(LogicalPlan::new(Operator::Show, vec![]))
    }
}

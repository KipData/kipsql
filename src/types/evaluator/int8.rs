use crate::numeric_binary_evaluator_definition;
use crate::types::evaluator::BinaryEvaluator;
use crate::types::evaluator::DataValue;
use paste::paste;
use serde::{Deserialize, Serialize};
use std::hint;

numeric_binary_evaluator_definition!(Int8, DataValue::Int8);

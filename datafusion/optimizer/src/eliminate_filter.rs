// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! [`EliminateFilter`] replaces `where false` or `where null` with an empty relation.

use datafusion_common::tree_node::Transformed;
use datafusion_common::{Result, ScalarValue};
use datafusion_expr::{EmptyRelation, Expr, Filter, LogicalPlan};
use std::sync::Arc;

use crate::optimizer::ApplyOrder;
use crate::{OptimizerConfig, OptimizerRule};

/// Optimization rule that eliminate the scalar value (true/false/null) filter
/// with an [LogicalPlan::EmptyRelation]
///
/// This saves time in planning and executing the query.
/// Note that this rule should be applied after simplify expressions optimizer rule.
#[derive(Default, Debug)]
pub struct EliminateFilter;

impl EliminateFilter {
    #[allow(missing_docs)]
    pub fn new() -> Self {
        Self {}
    }
}

impl OptimizerRule for EliminateFilter {
    fn name(&self) -> &str {
        "eliminate_filter"
    }

    fn apply_order(&self) -> Option<ApplyOrder> {
        Some(ApplyOrder::TopDown)
    }

    fn supports_rewrite(&self) -> bool {
        true
    }

    fn rewrite(
        &self,
        plan: LogicalPlan,
        _config: &dyn OptimizerConfig,
    ) -> Result<Transformed<LogicalPlan>> {
        match plan {
            LogicalPlan::Filter(Filter {
                predicate: Expr::Literal(ScalarValue::Boolean(v), _),
                input,
                ..
            }) => match v {
                Some(true) => Ok(Transformed::yes(Arc::unwrap_or_clone(input))),
                Some(false) | None => Ok(Transformed::yes(LogicalPlan::EmptyRelation(
                    EmptyRelation {
                        produce_one_row: false,
                        schema: Arc::clone(input.schema()),
                    },
                ))),
            },
            _ => Ok(Transformed::no(plan)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::assert_optimized_plan_eq_snapshot;
    use crate::OptimizerContext;
    use datafusion_common::{Result, ScalarValue};
    use datafusion_expr::{col, lit, logical_plan::builder::LogicalPlanBuilder, Expr};

    use crate::eliminate_filter::EliminateFilter;
    use crate::test::*;
    use datafusion_expr::test::function_stub::sum;

    macro_rules! assert_optimized_plan_equal {
        (
            $plan:expr,
            @ $expected:literal $(,)?
        ) => {{
            let optimizer_ctx = OptimizerContext::new().with_max_passes(1);
            let rules: Vec<Arc<dyn crate::OptimizerRule + Send + Sync>> = vec![Arc::new(EliminateFilter::new())];
            assert_optimized_plan_eq_snapshot!(
                optimizer_ctx,
                rules,
                $plan,
                @ $expected,
            )
        }};
    }

    #[test]
    fn filter_false() -> Result<()> {
        let filter_expr = lit(false);

        let table_scan = test_table_scan().unwrap();
        let plan = LogicalPlanBuilder::from(table_scan)
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .filter(filter_expr)?
            .build()?;

        // No aggregate / scan / limit
        assert_optimized_plan_equal!(plan, @"EmptyRelation")
    }

    #[test]
    fn filter_null() -> Result<()> {
        let filter_expr = Expr::Literal(ScalarValue::Boolean(None), None);

        let table_scan = test_table_scan().unwrap();
        let plan = LogicalPlanBuilder::from(table_scan)
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .filter(filter_expr)?
            .build()?;

        // No aggregate / scan / limit
        assert_optimized_plan_equal!(plan, @"EmptyRelation")
    }

    #[test]
    fn filter_false_nested() -> Result<()> {
        let filter_expr = lit(false);

        let table_scan = test_table_scan()?;
        let plan1 = LogicalPlanBuilder::from(table_scan.clone())
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .build()?;
        let plan = LogicalPlanBuilder::from(table_scan)
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .filter(filter_expr)?
            .union(plan1)?
            .build()?;

        // Left side is removed
        assert_optimized_plan_equal!(plan, @r"
        Union
          EmptyRelation
          Aggregate: groupBy=[[test.a]], aggr=[[sum(test.b)]]
            TableScan: test
        ")
    }

    #[test]
    fn filter_true() -> Result<()> {
        let filter_expr = lit(true);

        let table_scan = test_table_scan()?;
        let plan = LogicalPlanBuilder::from(table_scan)
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .filter(filter_expr)?
            .build()?;

        assert_optimized_plan_equal!(plan, @r"
        Aggregate: groupBy=[[test.a]], aggr=[[sum(test.b)]]
          TableScan: test
        ")
    }

    #[test]
    fn filter_true_nested() -> Result<()> {
        let filter_expr = lit(true);

        let table_scan = test_table_scan()?;
        let plan1 = LogicalPlanBuilder::from(table_scan.clone())
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .build()?;
        let plan = LogicalPlanBuilder::from(table_scan)
            .aggregate(vec![col("a")], vec![sum(col("b"))])?
            .filter(filter_expr)?
            .union(plan1)?
            .build()?;

        // Filter is removed
        assert_optimized_plan_equal!(plan, @r"
        Union
          Aggregate: groupBy=[[test.a]], aggr=[[sum(test.b)]]
            TableScan: test
          Aggregate: groupBy=[[test.a]], aggr=[[sum(test.b)]]
            TableScan: test
        ")
    }

    #[test]
    fn filter_from_subquery() -> Result<()> {
        // SELECT a FROM (SELECT a FROM test WHERE FALSE) WHERE TRUE

        let false_filter = lit(false);
        let table_scan = test_table_scan()?;
        let plan1 = LogicalPlanBuilder::from(table_scan)
            .project(vec![col("a")])?
            .filter(false_filter)?
            .build()?;

        let true_filter = lit(true);
        let plan = LogicalPlanBuilder::from(plan1)
            .project(vec![col("a")])?
            .filter(true_filter)?
            .build()?;

        // Filter is removed
        assert_optimized_plan_equal!(plan, @r"
        Projection: test.a
          EmptyRelation
        ")
    }
}

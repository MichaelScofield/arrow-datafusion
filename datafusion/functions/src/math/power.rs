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

//! Math function: `power()`.
use std::any::Any;
use std::sync::Arc;

use super::log::LogFunc;

use arrow::array::{ArrayRef, AsArray, Int64Array};
use arrow::datatypes::{ArrowNativeTypeOp, DataType, Float64Type};
use datafusion_common::{
    arrow_datafusion_err, exec_datafusion_err, exec_err, internal_datafusion_err,
    plan_datafusion_err, DataFusionError, Result, ScalarValue,
};
use datafusion_expr::expr::ScalarFunction;
use datafusion_expr::simplify::{ExprSimplifyResult, SimplifyInfo};
use datafusion_expr::{
    ColumnarValue, Documentation, Expr, ScalarFunctionArgs, ScalarUDF, TypeSignature,
};
use datafusion_expr::{ScalarUDFImpl, Signature, Volatility};
use datafusion_macros::user_doc;

#[user_doc(
    doc_section(label = "Math Functions"),
    description = "Returns a base expression raised to the power of an exponent.",
    syntax_example = "power(base, exponent)",
    standard_argument(name = "base", prefix = "Numeric"),
    standard_argument(name = "exponent", prefix = "Exponent numeric")
)]
#[derive(Debug)]
pub struct PowerFunc {
    signature: Signature,
    aliases: Vec<String>,
}

impl Default for PowerFunc {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerFunc {
    pub fn new() -> Self {
        use DataType::*;
        Self {
            signature: Signature::one_of(
                vec![
                    TypeSignature::Exact(vec![Int64, Int64]),
                    TypeSignature::Exact(vec![Float64, Float64]),
                ],
                Volatility::Immutable,
            ),
            aliases: vec![String::from("pow")],
        }
    }
}

impl ScalarUDFImpl for PowerFunc {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> &str {
        "power"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, arg_types: &[DataType]) -> Result<DataType> {
        match arg_types[0] {
            DataType::Int64 => Ok(DataType::Int64),
            _ => Ok(DataType::Float64),
        }
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let args = ColumnarValue::values_to_arrays(&args.args)?;

        let arr: ArrayRef = match args[0].data_type() {
            DataType::Float64 => {
                let bases = args[0].as_primitive::<Float64Type>();
                let exponents = args[1].as_primitive::<Float64Type>();
                let result = arrow::compute::binary::<_, _, _, Float64Type>(
                    bases,
                    exponents,
                    f64::powf,
                )?;
                Arc::new(result) as _
            }
            DataType::Int64 => {
                let bases = downcast_named_arg!(&args[0], "base", Int64Array);
                let exponents = downcast_named_arg!(&args[1], "exponent", Int64Array);
                bases
                    .iter()
                    .zip(exponents.iter())
                    .map(|(base, exp)| match (base, exp) {
                        (Some(base), Some(exp)) => Ok(Some(base.pow_checked(
                            exp.try_into().map_err(|_| {
                                exec_datafusion_err!(
                                    "Can't use negative exponents: {exp} in integer computation, please use Float."
                                )
                            })?,
                        ).map_err(|e| arrow_datafusion_err!(e))?)),
                        _ => Ok(None),
                    })
                    .collect::<Result<Int64Array>>()
                    .map(Arc::new)? as _
            }

            other => {
                return exec_err!(
                    "Unsupported data type {other:?} for function {}",
                    self.name()
                )
            }
        };

        Ok(ColumnarValue::Array(arr))
    }

    /// Simplify the `power` function by the relevant rules:
    /// 1. Power(a, 0) ===> 0
    /// 2. Power(a, 1) ===> a
    /// 3. Power(a, Log(a, b)) ===> b
    fn simplify(
        &self,
        mut args: Vec<Expr>,
        info: &dyn SimplifyInfo,
    ) -> Result<ExprSimplifyResult> {
        let exponent = args.pop().ok_or_else(|| {
            plan_datafusion_err!("Expected power to have 2 arguments, got 0")
        })?;
        let base = args.pop().ok_or_else(|| {
            plan_datafusion_err!("Expected power to have 2 arguments, got 1")
        })?;

        let exponent_type = info.get_data_type(&exponent)?;
        match exponent {
            Expr::Literal(value, _)
                if value == ScalarValue::new_zero(&exponent_type)? =>
            {
                Ok(ExprSimplifyResult::Simplified(Expr::Literal(
                    ScalarValue::new_one(&info.get_data_type(&base)?)?,
                    None,
                )))
            }
            Expr::Literal(value, _) if value == ScalarValue::new_one(&exponent_type)? => {
                Ok(ExprSimplifyResult::Simplified(base))
            }
            Expr::ScalarFunction(ScalarFunction { func, mut args })
                if is_log(&func) && args.len() == 2 && base == args[0] =>
            {
                let b = args.pop().unwrap(); // length checked above
                Ok(ExprSimplifyResult::Simplified(b))
            }
            _ => Ok(ExprSimplifyResult::Original(vec![base, exponent])),
        }
    }

    fn documentation(&self) -> Option<&Documentation> {
        self.doc()
    }
}

/// Return true if this function call is a call to `Log`
fn is_log(func: &ScalarUDF) -> bool {
    func.inner().as_any().downcast_ref::<LogFunc>().is_some()
}

#[cfg(test)]
mod tests {
    use arrow::array::Float64Array;
    use arrow::datatypes::Field;
    use datafusion_common::cast::{as_float64_array, as_int64_array};

    use super::*;

    #[test]
    fn test_power_f64() {
        let arg_fields = vec![
            Field::new("a", DataType::Float64, true).into(),
            Field::new("a", DataType::Float64, true).into(),
        ];
        let args = ScalarFunctionArgs {
            args: vec![
                ColumnarValue::Array(Arc::new(Float64Array::from(vec![
                    2.0, 2.0, 3.0, 5.0,
                ]))), // base
                ColumnarValue::Array(Arc::new(Float64Array::from(vec![
                    3.0, 2.0, 4.0, 4.0,
                ]))), // exponent
            ],
            arg_fields,
            number_rows: 4,
            return_field: Field::new("f", DataType::Float64, true).into(),
        };
        let result = PowerFunc::new()
            .invoke_with_args(args)
            .expect("failed to initialize function power");

        match result {
            ColumnarValue::Array(arr) => {
                let floats = as_float64_array(&arr)
                    .expect("failed to convert result to a Float64Array");
                assert_eq!(floats.len(), 4);
                assert_eq!(floats.value(0), 8.0);
                assert_eq!(floats.value(1), 4.0);
                assert_eq!(floats.value(2), 81.0);
                assert_eq!(floats.value(3), 625.0);
            }
            ColumnarValue::Scalar(_) => {
                panic!("Expected an array value")
            }
        }
    }

    #[test]
    fn test_power_i64() {
        let arg_fields = vec![
            Field::new("a", DataType::Int64, true).into(),
            Field::new("a", DataType::Int64, true).into(),
        ];
        let args = ScalarFunctionArgs {
            args: vec![
                ColumnarValue::Array(Arc::new(Int64Array::from(vec![2, 2, 3, 5]))), // base
                ColumnarValue::Array(Arc::new(Int64Array::from(vec![3, 2, 4, 4]))), // exponent
            ],
            arg_fields,
            number_rows: 4,
            return_field: Field::new("f", DataType::Int64, true).into(),
        };
        let result = PowerFunc::new()
            .invoke_with_args(args)
            .expect("failed to initialize function power");

        match result {
            ColumnarValue::Array(arr) => {
                let ints = as_int64_array(&arr)
                    .expect("failed to convert result to a Int64Array");

                assert_eq!(ints.len(), 4);
                assert_eq!(ints.value(0), 8);
                assert_eq!(ints.value(1), 4);
                assert_eq!(ints.value(2), 81);
                assert_eq!(ints.value(3), 625);
            }
            ColumnarValue::Scalar(_) => {
                panic!("Expected an array value")
            }
        }
    }
}

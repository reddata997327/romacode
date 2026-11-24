//! `math.*` tools — pure arithmetic, no I/O. The default policy `allow`s
//! these (read-only, side-effect-free).

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind};
use rc_core::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

macro_rules! binop {
    ($struct:ident, $name:expr, $desc:expr, $op:expr) => {
        pub struct $struct;
        #[async_trait]
        impl Tool for $struct {
            fn name(&self) -> &str { $name }
            fn description(&self) -> &str { $desc }
            fn schema(&self) -> Schema {
                Schema::new()
                    .param(Parameter::required("a", ParamType::Number))
                    .param(Parameter::required("b", ParamType::Number))
            }
            fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
            async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
                // Schema-validated input can still be unrepresentable
                // (e.g. u64 > i64::MAX), so convert via need_param — a
                // missing/wrong-shape param is an error result, not a panic.
                let a = need_param(input, "a", ParamType::Number, Value::as_f64)?;
                let b = need_param(input, "b", ParamType::Number, Value::as_f64)?;
                Ok(serde_json::json!(($op)(a, b)))
            }
        }
    };
}

binop!(Add, "math.add", "Add two numbers (a + b).", |a: f64, b: f64| a + b);
binop!(Subtract, "math.subtract", "Subtract b from a (a - b).", |a: f64, b: f64| a - b);
binop!(Multiply, "math.multiply", "Multiply two numbers (a × b).", |a: f64, b: f64| a * b);

pub struct Divide;
#[async_trait]
impl Tool for Divide {
    fn name(&self) -> &str { "math.divide" }
    fn description(&self) -> &str { "Divide a by b (a / b). Errors on zero." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("a", ParamType::Number))
            .param(Parameter::required("b", ParamType::Number))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let a = need_param(input, "a", ParamType::Number, Value::as_f64)?;
        let b = need_param(input, "b", ParamType::Number, Value::as_f64)?;
        if b == 0.0 {
            return Err(rc_core::error::ToolError::Other("division by zero".into()));
        }
        let r = a / b;
        // NaN / ±inf serialize as JSON null — surface a real error instead.
        if !r.is_finite() {
            return Err(rc_core::error::ToolError::Other(format!(
                "result of {a} / {b} is not a finite number"
            )));
        }
        Ok(serde_json::json!(r))
    }
}

pub struct Sqrt;
#[async_trait]
impl Tool for Sqrt {
    fn name(&self) -> &str { "math.sqrt" }
    fn description(&self) -> &str { "Square root of a number." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("n", ParamType::Number))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let n = need_param(input, "n", ParamType::Number, Value::as_f64)?;
        Ok(serde_json::json!(n.sqrt()))
    }
}

pub struct Factorial;
#[async_trait]
impl Tool for Factorial {
    fn name(&self) -> &str { "math.factorial" }
    fn description(&self) -> &str { "Factorial of a non-negative integer (n!)." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("n", ParamType::Integer))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        // `as_i64().unwrap()` panicked on schema-valid u64 > i64::MAX
        // (e.g. 18446744073709551615) — that is now an error result.
        let n = need_param(input, "n", ParamType::Integer, Value::as_i64)?;
        if n < 0 {
            return Err(rc_core::error::ToolError::Other("factorial of a negative number".into()));
        }
        // u128 to hold larger results
        let mut acc: u128 = 1;
        for i in 2..=n as u64 {
            acc = acc.saturating_mul(i as u128);
        }
        Ok(serde_json::json!(acc.to_string()))
    }
}

pub struct Gcd;
#[async_trait]
impl Tool for Gcd {
    fn name(&self) -> &str { "math.gcd" }
    fn description(&self) -> &str { "Greatest common divisor of two integers (Euclid)." }
    fn schema(&self) -> Schema {
        Schema::new()
            .param(Parameter::required("a", ParamType::Integer))
            .param(Parameter::required("b", ParamType::Integer))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let mut a = need_param(input, "a", ParamType::Integer, Value::as_i64)?.unsigned_abs();
        let mut b = need_param(input, "b", ParamType::Integer, Value::as_i64)?.unsigned_abs();
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        Ok(serde_json::json!(a))
    }
}

/// All math tools, ready to register.
pub fn all() -> Vec<rc_core::tool::ToolArc> {
    use rc_core::tool::arc;
    vec![
        arc(Add),
        arc(Subtract),
        arc(Multiply),
        arc(Divide),
        arc(Sqrt),
        arc(Factorial),
        arc(Gcd),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::Tool;

    #[tokio::test]
    async fn factorial_u64_overflow_is_error_not_panic() {
        // Schema-valid (is_u64) but unrepresentable as i64 — must be an
        // error result, never a panic that unwinds the agent loop.
        let out = Factorial
            .execute(&serde_json::json!({ "n": 18446744073709551615u64 }), &ToolContext::default())
            .await;
        assert!(out.is_err(), "expected error, got {out:?}");
    }

    #[tokio::test]
    async fn gcd_u64_overflow_is_error_not_panic() {
        let big = 18446744073709551615u64;
        let out = Gcd
            .execute(&serde_json::json!({ "a": big, "b": 6 }), &ToolContext::default())
            .await;
        assert!(out.is_err(), "expected error, got {out:?}");
        // Normal (and negative) inputs still work.
        let out = Gcd
            .execute(&serde_json::json!({ "a": 48, "b": -18 }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(out, 6);
    }

    #[tokio::test]
    async fn divide_by_zero_is_error_not_nan() {
        let out = Divide
            .execute(&serde_json::json!({ "a": 1.0, "b": 0.0 }), &ToolContext::default())
            .await;
        assert!(out.is_err(), "0/0 must error, not serialize NaN as null");
        let out = Divide
            .execute(&serde_json::json!({ "a": 10.0, "b": 4.0 }), &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(out, 2.5);
    }

    #[tokio::test]
    async fn missing_or_wrong_params_error_not_panic() {
        assert!(Add.execute(&serde_json::json!({}), &ToolContext::default()).await.is_err());
        assert!(Sqrt.execute(&serde_json::json!({ "n": "x" }), &ToolContext::default()).await.is_err());
        assert!(Factorial.execute(&serde_json::json!({}), &ToolContext::default()).await.is_err());
        assert!(Gcd.execute(&serde_json::json!({ "a": 4 }), &ToolContext::default()).await.is_err());
    }
}

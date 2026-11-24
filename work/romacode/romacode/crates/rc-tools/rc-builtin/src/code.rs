//! `code.*` tools — encoding/parsing helpers, no I/O. Default policy `allow`s
//! these (pure functions). Base64 is hand-implemented to avoid a dependency.

use rc_core::schema::{ParamType, Parameter, Schema};
use rc_core::tool::{ToolContext, ToolSourceKind, ToolArc};
use rc_core::{arc, Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::need_param;

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(B64[((n >> 6) & 63) as usize] as char);
        out.push(B64[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(B64[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 4 != 0 {
        return Err(format!(
            "invalid base64: length {} is not a multiple of 4",
            s.len()
        ));
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let mut vals = [0u8; 4];
        let mut pad = 0;
        for j in 0..4 {
            let c = bytes[i + j];
            if c == b'=' {
                // '=' is only valid as one of the last two characters of the
                // (final) group — `=AAA`, `AB=C`, `====` are malformed and
                // must be rejected instead of silently decoding garbage.
                if j < 2 {
                    return Err(format!(
                        "invalid base64: '=' at position {} (only the last two characters may be padding)",
                        i + j
                    ));
                }
                vals[j] = 0;
                pad += 1;
            } else {
                if pad > 0 {
                    return Err("invalid base64: data after '=' padding".into());
                }
                vals[j] = B64.iter().position(|&b| b == c).ok_or_else(|| {
                    format!("invalid base64 char {:?} at position {}", c as char, i + j)
                })? as u8;
            }
        }
        let n = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
        i += 4;
    }
    Ok(out)
}

pub struct Base64Encode;
#[async_trait]
impl Tool for Base64Encode {
    fn name(&self) -> &str { "code.base64_encode" }
    fn description(&self) -> &str { "Base64-encode a string (UTF-8 bytes)." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("s", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        Ok(serde_json::json!(b64_encode(s.as_bytes())))
    }
}

pub struct Base64Decode;
#[async_trait]
impl Tool for Base64Decode {
    fn name(&self) -> &str { "code.base64_decode" }
    fn description(&self) -> &str { "Base64-decode a string to UTF-8 (errors on bad input)." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("s", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let bytes = b64_decode(s).map_err(rc_core::error::ToolError::Other)?;
        let text = String::from_utf8(bytes).map_err(|e| {
            rc_core::error::ToolError::Other(format!("decoded bytes are not valid UTF-8: {e}"))
        })?;
        Ok(serde_json::json!(text))
    }
}

pub struct ParseJson;
#[async_trait]
impl Tool for ParseJson {
    fn name(&self) -> &str { "code.parse_json" }
    fn description(&self) -> &str { "Parse a JSON string into a value." }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("s", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let s = need_param(input, "s", ParamType::String, Value::as_str)?;
        let v: Value = serde_json::from_str(s)?;
        Ok(v)
    }
}

pub struct EvalExpr;
#[async_trait]
impl Tool for EvalExpr {
    fn name(&self) -> &str { "code.eval_expr" }
    fn description(&self) -> &str {
        "Evaluate a simple arithmetic expression over + - * / and parentheses (shunting-yard). Numbers are f64."
    }
    fn schema(&self) -> Schema {
        Schema::new().param(Parameter::required("expr", ParamType::String))
    }
    fn source(&self) -> ToolSourceKind { ToolSourceKind::Builtin }
    async fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolResult<Value> {
        let expr = need_param(input, "expr", ParamType::String, Value::as_str)?;
        let v = eval_arith(expr).map_err(rc_core::error::ToolError::Other)?;
        Ok(serde_json::json!(v))
    }
}

#[derive(Debug, Clone)]
enum Tok { Num(f64), Op(char) }

/// A tiny shunting-yard evaluator for + - * / and parens.
/// Unary minus is represented as the internal operator `~` (highest
/// precedence, right-associative), so negative numbers like `-5`, `2*-3`,
/// `-(2+3)` and `3 - -2` are mathematically valid input.
fn eval_arith(s: &str) -> Result<f64, String> {
    let toks = tokenize(s)?;
    let rpn = to_rpn(&toks)?;
    eval_rpn(&rpn)
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut num = String::new();
    // True when the next '+'/'-' is a unary sign (at the start of the
    // expression, or right after an operator or '(').
    let mut unary_pos = true;
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push(Tok::Num(
                    num.parse::<f64>()
                        .map_err(|_| format!("invalid number `{num}`"))?,
                ));
                num.clear();
                unary_pos = false;
            }
            match c {
                '+' | '-' => {
                    if unary_pos {
                        // Unary sign: '-' becomes the internal `~` operator;
                        // unary '+' is a no-op.
                        if c == '-' {
                            out.push(Tok::Op('~'));
                        }
                    } else {
                        out.push(Tok::Op(c));
                    }
                    unary_pos = true;
                }
                '*' | '/' | '(' | ')' => {
                    out.push(Tok::Op(c));
                    unary_pos = c != ')';
                }
                ws if ws.is_whitespace() => {}
                other => return Err(format!("unexpected char {other:?}")),
            }
        }
    }
    if !num.is_empty() {
        out.push(Tok::Num(
            num.parse::<f64>()
                .map_err(|_| format!("invalid number `{num}`"))?,
        ));
    }
    Ok(out)
}

fn prec(op: char) -> u8 {
    match op {
        '+' | '-' => 1,
        '*' | '/' => 2,
        '~' => 3, // unary minus: binds tightest, right-associative
        _ => 0,
    }
}

fn to_rpn(toks: &[Tok]) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    for t in toks {
        match t {
            Tok::Num(_) => out.push(t.clone()),
            Tok::Op(c) => {
                if *c == '(' {
                    ops.push(*c);
                } else if *c == ')' {
                    loop {
                        match ops.pop() {
                            Some('(') => break,
                            Some(op) => out.push(Tok::Op(op)),
                            None => return Err("mismatched parentheses: unmatched ')'".into()),
                        }
                    }
                } else if *c == '~' {
                    // Unary minus is right-associative: only pop *higher*
                    // precedence operators.
                    while let Some(&top) = ops.last() {
                        if top != '(' && prec(top) > prec(*c) {
                            out.push(Tok::Op(ops.pop().unwrap()));
                        } else {
                            break;
                        }
                    }
                    ops.push(*c);
                } else {
                    while let Some(&top) = ops.last() {
                        if top != '(' && prec(top) >= prec(*c) {
                            out.push(Tok::Op(ops.pop().unwrap()));
                        } else {
                            break;
                        }
                    }
                    ops.push(*c);
                }
            }
        }
    }
    while let Some(op) = ops.pop() {
        if op == '(' || op == ')' {
            return Err("mismatched parentheses".into());
        }
        out.push(Tok::Op(op));
    }
    Ok(out)
}

fn eval_rpn(rpn: &[Tok]) -> Result<f64, String> {
    let mut st: Vec<f64> = Vec::new();
    for t in rpn {
        match t {
            Tok::Num(n) => st.push(*n),
            Tok::Op('~') => {
                let a = st
                    .pop()
                    .ok_or("malformed expression: unary minus missing operand")?;
                st.push(-a);
            }
            Tok::Op(c) => {
                let b = st
                    .pop()
                    .ok_or("malformed expression: operator missing right operand")?;
                let a = st
                    .pop()
                    .ok_or("malformed expression: operator missing left operand")?;
                let r = match c {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => {
                        if b == 0.0 {
                            return Err("division by zero".into());
                        }
                        a / b
                    }
                    _ => return Err(format!("bad op {:?}", c)),
                };
                st.push(r);
            }
        }
    }
    if st.len() != 1 {
        return Err("malformed expression: wrong number of operands".into());
    }
    Ok(st.pop().unwrap())
}

pub fn all() -> Vec<ToolArc> {
    vec![arc(Base64Encode), arc(Base64Decode), arc(ParseJson), arc(EvalExpr)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b64_roundtrip() {
        let s = "Hello, RomaCode!";
        let enc = b64_encode(s.as_bytes());
        assert_eq!(enc, "SGVsbG8sIFJvbWFDb2RlIQ==");
        let dec = b64_decode(&enc).unwrap();
        assert_eq!(dec, s.as_bytes());
    }
    #[test]
    fn arith() {
        assert_eq!(eval_arith("2 + 3 * 4").unwrap(), 14.0);
        assert_eq!(eval_arith("(2 + 3) * 4").unwrap(), 20.0);
        assert_eq!(eval_arith("10 / 2 - 1").unwrap(), 4.0);
    }

    #[test]
    fn arith_accepts_negative_numbers() {
        // Negative numbers are mathematically valid and must be accepted.
        assert_eq!(eval_arith("-5").unwrap(), -5.0);
        assert_eq!(eval_arith("-5 + 2").unwrap(), -3.0);
        assert_eq!(eval_arith("2 * -3").unwrap(), -6.0);
        assert_eq!(eval_arith("-(2 + 3)").unwrap(), -5.0);
        assert_eq!(eval_arith("3 - -2").unwrap(), 5.0);
        assert_eq!(eval_arith("-2/-4").unwrap(), 0.5);
    }

    #[test]
    fn arith_malformed_input_errors_clearly() {
        // `+ 2` (unary plus) is mathematically valid and returns 2.
        assert_eq!(eval_arith("+ 2").unwrap(), 2.0);
        for bad in ["", "2 +", "2 3", "(2 + 3", "2 + 3)", "1/0", "a + b"] {
            assert!(eval_arith(bad).is_err(), "expected error for {bad:?}");
        }
    }

    #[test]
    fn b64_decode_rejects_malformed_input() {
        // Silently decoding garbage is worse than an error.
        for bad in ["====", "A===", "AB=C", "A", "ABCDE", "QQ=A"] {
            assert!(b64_decode(bad).is_err(), "expected error for {bad:?}");
        }
        // Valid input (with whitespace) still decodes.
        assert_eq!(b64_decode("QQ == ").unwrap(), b"A");
        assert_eq!(b64_decode("SGVsbG8=").unwrap(), b"Hello");
    }

    #[tokio::test]
    async fn missing_params_error_not_panic() {
        let ctx = ToolContext::default();
        assert!(Base64Encode.execute(&serde_json::json!({}), &ctx).await.is_err());
        assert!(Base64Decode.execute(&serde_json::json!({}), &ctx).await.is_err());
        assert!(ParseJson.execute(&serde_json::json!({}), &ctx).await.is_err());
        assert!(EvalExpr.execute(&serde_json::json!({}), &ctx).await.is_err());
    }
}

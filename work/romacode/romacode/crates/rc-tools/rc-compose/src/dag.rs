//! **DAG** — run a dependency graph of tool calls in topological order. Each
//! node names a tool, an optional input template, and the nodes it depends on.
//! A node's template may reference `{{input.x}}` (the composition's input) and
//! `{{nodes.<name>.<path>}}` (a dependency's output). Returns a map of every
//! node's output, or a single nominated output node's value.

use crate::templating::{compose_kind, resolve_ctx};
use async_trait::async_trait;
use rc_core::error::{ToolError, ToolResult};
use rc_core::executor::Executor;
use rc_core::schema::Schema;
use rc_core::tool::{arc, Tool, ToolArc, ToolContext, ToolSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagNode {
    pub name: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Names of nodes whose outputs this node reads. Used for ordering only —
    /// a node may still reference a non-depended node at its own risk.
    #[serde(default)]
    pub deps: Vec<String>,
}

pub struct DagTool {
    pub name: String,
    pub description: String,
    pub schema: Schema,
    pub executor: Executor,
    pub nodes: Vec<DagNode>,
    /// If set, return only this node's output; else return a map of all.
    pub output_node: Option<String>,
}

impl DagTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Schema,
        executor: Executor,
        nodes: Vec<DagNode>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            executor,
            nodes,
            output_node: None,
        }
    }
    pub fn with_output_node(mut self, n: impl Into<String>) -> Self {
        self.output_node = Some(n.into());
        self
    }
    pub fn into_arc(self) -> ToolArc {
        arc(self)
    }

    /// Topologically sort the nodes (Kahn's algorithm). Errors on cycles or
    /// missing deps.
    fn topo(&self) -> Result<Vec<usize>, ToolError> {
        let n = self.nodes.len();
        let name_to_idx: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, nd)| (nd.name.as_str(), i))
            .collect();
        let mut indeg = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, nd) in self.nodes.iter().enumerate() {
            for d in &nd.deps {
                let j = name_to_idx.get(d.as_str()).ok_or_else(|| {
                    ToolError::Other(format!("dag node `{}` depends on unknown node `{}`", nd.name, d))
                })?;
                adj[*j].push(i);
                indeg[i] += 1;
            }
        }
        let mut queue: std::collections::VecDeque<usize> =
            (0..n).filter(|&i| indeg[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = queue.pop_front() {
            order.push(i);
            for &j in &adj[i] {
                indeg[j] -= 1;
                if indeg[j] == 0 {
                    queue.push_back(j);
                }
            }
        }
        if order.len() != n {
            return Err(ToolError::Other("dag has a cycle".into()));
        }
        Ok(order)
    }
}

#[async_trait]
impl Tool for DagTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Schema {
        self.schema.clone()
    }
    fn source(&self) -> ToolSourceKind {
        compose_kind("dag")
    }
    async fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolResult<Value> {
        if ctx.is_cancelled() {
            return Err(ToolError::Other("dag cancelled".into()));
        }
        let order = self.topo()?;
        let mut outputs: HashMap<String, Value> = HashMap::new();
        for &i in &order {
            let node = &self.nodes[i];
            let nodes_ctx: serde_json::Map<String, Value> = outputs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let ctx_val = serde_json::json!({
                "input": input,
                "nodes": Value::Object(nodes_ctx),
            });
            let step_input = match &node.input {
                Some(t) => resolve_ctx(t, &ctx_val),
                None => input.clone(),
            };
            let out = self.executor.run_inner(&node.tool, &step_input).await?;
            outputs.insert(node.name.clone(), out);
        }
        match &self.output_node {
            Some(name) => Ok(outputs
                .get(name)
                .cloned()
                .unwrap_or(Value::Null)),
            None => {
                let mut map = serde_json::Map::new();
                for (k, v) in outputs {
                    map.insert(k, v);
                }
                Ok(Value::Object(map))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc_core::permissions::PermissionPolicy;
    use rc_core::sandbox::Sandbox;
    use rc_core::Registry;

    fn exec() -> Executor {
        let r = Registry::new();
        r.register_all(rc_builtin::all_tools());
        Executor::new(r, PermissionPolicy::default(), Sandbox::default())
    }

    #[tokio::test]
    async fn diamond_dependency() {
        // input.x → a = x², b = x³ ; c = a + b. Returns c.
        let ex = exec();
        let dag = DagTool::new(
            "compose.diamond",
            "x² + x³",
            Schema::new().param(rc_core::schema::Parameter::required("x", rc_core::schema::ParamType::Number)),
            ex,
            vec![
                DagNode {
                    name: "sq".into(),
                    tool: "math.multiply".into(),
                    input: Some(serde_json::json!({ "a": "{{input.x}}", "b": "{{input.x}}" })),
                    deps: vec![],
                },
                DagNode {
                    name: "cu".into(),
                    tool: "math.multiply".into(),
                    input: Some(serde_json::json!({ "a": "{{nodes.sq}}", "b": "{{input.x}}" })),
                    deps: vec!["sq".into()],
                },
                DagNode {
                    name: "sum".into(),
                    tool: "math.add".into(),
                    input: Some(serde_json::json!({ "a": "{{nodes.sq}}", "b": "{{nodes.cu}}" })),
                    deps: vec!["sq".into(), "cu".into()],
                },
            ],
        )
        .with_output_node("sum");
        let out = dag
            .execute(&serde_json::json!({ "x": 3 }), &ToolContext::default())
            .await
            .unwrap();
        // sq = 9, cu = 27, sum = 36
        assert!((out.as_f64().unwrap() - 36.0).abs() < 1e-9);
    }

    #[test]
    fn topo_detects_cycle() {
        let ex = exec();
        let dag = DagTool::new(
            "compose.cyc",
            "",
            Schema::new(),
            ex,
            vec![
                DagNode { name: "a".into(), tool: "math.add".into(), input: None, deps: vec!["b".into()] },
                DagNode { name: "b".into(), tool: "math.add".into(), input: None, deps: vec!["a".into()] },
            ],
        );
        assert!(dag.topo().is_err());
    }
}

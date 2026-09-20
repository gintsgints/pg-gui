//! `EXPLAIN (FORMAT JSON)` output turned into the frame tree the flame graph
//! draws (cmd-e / cmd-shift-e, rendered by `app::render_plan_view`).
//!
//! A query plan is already a tree whose nodes carry an inclusive measure —
//! `Total Cost` without `ANALYZE`, `Actual Total Time` with it — which is
//! exactly the shape a flame graph wants: a node is as wide as its measure and
//! its children pack inside it. Nothing is aggregated or re-scaled here; the
//! planner's own numbers are what the widths mean.
//!
//! Two plan shapes over-run their parent, and the chart clips them rather than
//! hiding it: a `Gather` whose workers' time sums past the wall time of the
//! node above, and `InitPlan`/`SubPlan`/CTE nodes, whose cost the planner does
//! not fold into the parent it hangs them off. Both read as a child row wider
//! than the frame it sits under, which is the truth about the numbers.

use std::fmt::Write as _;
use std::rc::Rc;

use gpui::SharedString;
use serde_json::Value;

/// One plan node: a frame of the flame graph.
pub struct PlanNode {
    /// What the frame is labelled with: the node type plus the relation or
    /// index it works on (`Index Scan using orders_pkey on orders`).
    pub label: SharedString,
    /// The node's inclusive measure, in the plan's [`Unit`].
    pub value: f64,
    pub children: Vec<PlanNode>,
}

/// What the widths of a parsed plan mean.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Planner cost units, from a plain `EXPLAIN`.
    Cost,
    /// Milliseconds actually spent, from `EXPLAIN (ANALYZE)`.
    Millis,
    /// Neither: every node measured zero, so the frames are sized by how many
    /// nodes their subtree holds and only the shape is meaningful.
    Nodes,
}

impl Unit {
    /// A measure as the tooltip shows it.
    pub fn format(self, value: f64) -> String {
        match self {
            Self::Cost => format!("cost {value:.2}"),
            Self::Millis if value < 1. => format!("{value:.3} ms"),
            Self::Millis => format!("{value:.2} ms"),
            Self::Nodes => format!("{value:.0} node(s)"),
        }
    }

    /// What the panel header calls this measure.
    pub fn caption(self) -> &'static str {
        match self {
            Self::Cost => "estimated cost",
            Self::Millis => "actual time",
            Self::Nodes => "node count (plan reported no cost)",
        }
    }
}

/// A parsed plan, ready for the chart.
pub struct Plan {
    /// The plan's root node(s). Behind an `Rc` because `FlameGraph::shared`
    /// adopts the tree rather than copying it on every render.
    pub roots: Rc<Vec<PlanNode>>,
    pub unit: Unit,
    /// Deepest stack in the tree, for sizing the chart's scroll container.
    pub depth: usize,
    /// How many nodes the plan holds, for the panel header.
    pub nodes: usize,
    /// One line under the header: the totals Postgres reports alongside the
    /// tree (planning/execution time), or the root's own measure.
    pub summary: String,
    /// The statement the plan is of, collapsed onto one line for the header.
    pub statement: SharedString,
    /// Whether this came from `EXPLAIN (ANALYZE)`.
    pub analyze: bool,
}

/// Parse the JSON document `EXPLAIN (FORMAT JSON)` returns.
///
/// `analyze` says which measure to read: with it, `Actual Total Time` times
/// `Actual Loops` (Postgres reports the per-loop average, so a node visited a
/// thousand times reports a thousandth of what it spent); without it,
/// `Total Cost`.
pub fn parse(json: &str, analyze: bool, statement: &str) -> Result<Plan, String> {
    let document: Value =
        serde_json::from_str(json).map_err(|err| format!("plan is not JSON: {err}"))?;
    let entries = document
        .as_array()
        .ok_or_else(|| "plan is not a JSON array".to_string())?;

    let mut roots = Vec::new();
    let mut planning = None;
    let mut execution = None;
    for entry in entries {
        let Some(plan) = entry.get("Plan") else {
            continue;
        };
        roots.push(node(plan, analyze));
        planning = planning.or_else(|| number(entry, "Planning Time"));
        execution = execution.or_else(|| number(entry, "Execution Time"));
    }
    if roots.is_empty() {
        return Err("plan holds no nodes".to_string());
    }

    // A plan whose every node measured zero — a trivial query under ANALYZE,
    // where each node rounds to 0.000 ms — would draw nothing at all. Size the
    // frames by subtree size instead, which keeps every parent at least as
    // wide as its children and leaves the shape readable.
    let mut unit = if analyze { Unit::Millis } else { Unit::Cost };
    if total(&roots) <= 0. {
        for root in &mut roots {
            size_by_nodes(root);
        }
        unit = Unit::Nodes;
    }

    let summary = summary(&roots, unit, planning, execution);
    let depth = depth(&roots);
    let nodes = count(&roots);
    Ok(Plan {
        roots: Rc::new(roots),
        unit,
        depth,
        nodes,
        summary,
        statement: SharedString::from(one_line(statement)),
        analyze,
    })
}

/// Build one node and its subtree.
fn node(plan: &Value, analyze: bool) -> PlanNode {
    let value = if analyze {
        let loops = number(plan, "Actual Loops").unwrap_or(1.).max(1.);
        number(plan, "Actual Total Time").unwrap_or(0.) * loops
    } else {
        number(plan, "Total Cost").unwrap_or(0.)
    };
    let children = plan
        .get("Plans")
        .and_then(Value::as_array)
        .map(|plans| plans.iter().map(|plan| node(plan, analyze)).collect())
        .unwrap_or_default();
    PlanNode {
        label: SharedString::from(label(plan)),
        value: value.max(0.),
        children,
    }
}

/// A node's label: what it does and what it does it to, the way `EXPLAIN`'s
/// own text output heads each line.
fn label(plan: &Value) -> String {
    let mut label = String::new();
    if let Some(name) = text(plan, "Subplan Name") {
        // `InitPlan 1 (returns $0)` and friends already name the node type of
        // the frame below them, so they head the label rather than repeat it.
        label.push_str(name);
        label.push_str(": ");
    }
    if plan.get("Parallel Aware").and_then(Value::as_bool) == Some(true) {
        label.push_str("Parallel ");
    }
    label.push_str(text(plan, "Node Type").unwrap_or("?"));
    // Writing to a String cannot fail.
    if let Some(strategy) = text(plan, "Join Type") {
        let _ = write!(label, " ({strategy})");
    }
    if let Some(index) = text(plan, "Index Name") {
        let _ = write!(label, " using {index}");
    }
    if let Some(relation) = text(plan, "Relation Name") {
        let _ = write!(label, " on {relation}");
        if let Some(alias) = text(plan, "Alias").filter(|alias| *alias != relation) {
            let _ = write!(label, " {alias}");
        }
    } else if let Some(cte) = text(plan, "CTE Name") {
        let _ = write!(label, " on {cte}");
    } else if let Some(function) = text(plan, "Function Name") {
        let _ = write!(label, " on {function}");
    }
    label
}

/// Re-value a subtree by how many nodes it holds, for a plan that measured
/// zero throughout.
fn size_by_nodes(node: &mut PlanNode) -> f64 {
    let children: f64 = node.children.iter_mut().map(size_by_nodes).sum();
    node.value = 1. + children;
    node.value
}

/// The header's second line: what Postgres reported alongside the tree.
fn summary(
    roots: &[PlanNode],
    unit: Unit,
    planning: Option<f64>,
    execution: Option<f64>,
) -> String {
    let mut parts = Vec::new();
    if let Some(planning) = planning {
        parts.push(format!("planning {planning:.2} ms"));
    }
    if let Some(execution) = execution {
        parts.push(format!("execution {execution:.2} ms"));
    }
    if parts.is_empty() {
        parts.push(format!("total {}", unit.format(total(roots))));
    }
    parts.join(" · ")
}

/// How many nodes the forest holds.
fn count(roots: &[PlanNode]) -> usize {
    roots.iter().map(|root| 1 + count(&root.children)).sum()
}

fn total(roots: &[PlanNode]) -> f64 {
    roots.iter().map(|root| root.value).sum()
}

/// The deepest stack in the tree, counting a lone root as depth 1.
fn depth(roots: &[PlanNode]) -> usize {
    roots
        .iter()
        .map(|root| 1 + depth(&root.children))
        .max()
        .unwrap_or(0)
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// How much of the statement the panel header shows before eliding it.
const HEADER_SQL_LEN: usize = 100;

/// The explained statement on one line, for the panel header.
fn one_line(sql: &str) -> String {
    let mut out = String::new();
    for (i, word) in sql.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > HEADER_SQL_LEN {
            let cut = out
                .char_indices()
                .nth(HEADER_SQL_LEN)
                .map_or(out.len(), |(at, _)| at);
            out.truncate(cut);
            out.push('…');
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-node plan as `EXPLAIN (FORMAT JSON)` writes it.
    const COSTS: &str = r#"[
      {
        "Plan": {
          "Node Type": "Aggregate",
          "Total Cost": 25.88,
          "Plans": [
            {
              "Node Type": "Seq Scan",
              "Relation Name": "orders",
              "Alias": "o",
              "Total Cost": 22.00
            }
          ]
        }
      }
    ]"#;

    #[test]
    fn a_cost_plan_takes_its_widths_from_total_cost() {
        let plan = parse(COSTS, false, "select count(*) from orders o").unwrap();
        assert!(plan.unit == Unit::Cost);
        assert_eq!(plan.depth, 2);
        assert_eq!(plan.nodes, 2);
        assert_eq!(plan.roots.len(), 1);
        assert!((plan.roots[0].value - 25.88).abs() < f64::EPSILON);
        assert_eq!(plan.roots[0].label, "Aggregate");
        assert_eq!(plan.roots[0].children[0].label, "Seq Scan on orders o");
        assert_eq!(plan.summary, "total cost 25.88");
    }

    #[test]
    fn an_analyzed_node_is_as_wide_as_every_loop_it_ran() {
        // Postgres reports the per-loop average, so a node visited 10 times
        // for 2 ms each reports 2 ms and has spent 20.
        let json = r#"[
          {
            "Plan": {
              "Node Type": "Nested Loop",
              "Actual Total Time": 30.0,
              "Actual Loops": 1,
              "Plans": [
                {
                  "Node Type": "Index Scan",
                  "Index Name": "orders_pkey",
                  "Relation Name": "orders",
                  "Alias": "orders",
                  "Actual Total Time": 2.0,
                  "Actual Loops": 10
                }
              ]
            },
            "Planning Time": 0.4,
            "Execution Time": 31.2
          }
        ]"#;
        let plan = parse(json, true, "select 1").unwrap();
        assert!(plan.unit == Unit::Millis);
        assert!((plan.roots[0].value - 30.0).abs() < f64::EPSILON);
        assert!((plan.roots[0].children[0].value - 20.0).abs() < f64::EPSILON);
        assert_eq!(
            plan.roots[0].children[0].label,
            "Index Scan using orders_pkey on orders"
        );
        assert_eq!(plan.summary, "planning 0.40 ms · execution 31.20 ms");
    }

    #[test]
    fn a_plan_that_measured_nothing_is_sized_by_its_nodes() {
        let json = r#"[
          {
            "Plan": {
              "Node Type": "Result",
              "Actual Total Time": 0.0,
              "Actual Loops": 1,
              "Plans": [
                { "Node Type": "Result", "Actual Total Time": 0.0, "Actual Loops": 1 },
                { "Node Type": "Result", "Actual Total Time": 0.0, "Actual Loops": 1 }
              ]
            }
          }
        ]"#;
        let plan = parse(json, true, "select 1").unwrap();
        assert!(plan.unit == Unit::Nodes);
        // The root covers itself and both children; each child covers itself,
        // so no child is ever wider than its parent.
        assert!((plan.roots[0].value - 3.0).abs() < f64::EPSILON);
        assert!((plan.roots[0].children[0].value - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_subplan_keeps_the_name_it_is_referenced_by() {
        let json = r#"[
          {
            "Plan": {
              "Node Type": "Result",
              "Total Cost": 1.0,
              "Plans": [
                {
                  "Subplan Name": "InitPlan 1 (returns $0)",
                  "Node Type": "Aggregate",
                  "Total Cost": 0.5
                }
              ]
            }
          }
        ]"#;
        let plan = parse(json, false, "select 1").unwrap();
        assert_eq!(
            plan.roots[0].children[0].label,
            "InitPlan 1 (returns $0): Aggregate"
        );
    }

    #[test]
    fn junk_is_reported_rather_than_drawn() {
        assert!(parse("not json", false, "").is_err());
        assert!(parse("[]", false, "").is_err());
    }
}

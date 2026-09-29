//! 工作流图的规范化与自动布局（供 MCP workflow_create / workflow_update 使用）。
//!
//! 背景：Agent 通过 MCP 写工作流时常见三类问题——
//! 1. 连线写法五花八门（source/target、[0,1]、按节点名、字符串数字），此前一律静默
//!    解析失败 → 变成空连线，画布上只剩「开始 → 第一个节点」一条线；
//! 2. 并行分支只把「开始」连到原首节点，其它入度为 0 的分支永远不会被执行；
//! 3. 布局只按索引排成一列（且 update 完全不布局，所有节点叠在 (0,0)），并行分支
//!    的连线穿过节点，很难看。
//!
//! 这里统一处理：宽松解析 + 严格校验（越界/自环/成环直接报错让 Agent 修正）、
//! 自动补开始节点并连到所有根、与画布「⚙ 自动重排」一致的分层布局。

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use crate::models::{Edge, WorkflowStep};

// 与 WorkflowsView.svelte 的常量保持一致
const NODE_W: f64 = 210.0;
const WIDE_W: f64 = 290.0; // note / env
const NODE_H: f64 = 54.0;
const ROW_GAP: f64 = 90.0;
const COL_GAP: f64 = 70.0;
const LEFT: f64 = 60.0;
const TOP: f64 = 40.0;

fn is_note(s: &WorkflowStep) -> bool {
    matches!(s, WorkflowStep::Note { .. })
}

fn node_w(s: &WorkflowStep) -> f64 {
    match s {
        WorkflowStep::Note { .. } | WorkflowStep::EnvSet { .. } => WIDE_W,
        _ => NODE_W,
    }
}

/// 解析单个端点：数字、数字字符串或节点名称。
fn endpoint(v: &Value, steps: &[WorkflowStep]) -> Result<usize, String> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .map(|n| n as usize)
            .ok_or_else(|| format!("连线端点 {n} 不是非负整数")),
        Value::String(s) => {
            let t = s.trim();
            if let Ok(n) = t.parse::<usize>() {
                return Ok(n);
            }
            let hits: Vec<usize> = steps
                .iter()
                .enumerate()
                .filter(|(_, st)| st.meta().name == t)
                .map(|(i, _)| i)
                .collect();
            match hits.len() {
                1 => Ok(hits[0]),
                0 => Err(format!("连线端点「{t}」既不是步骤索引，也不匹配任何步骤名称")),
                _ => Err(format!("连线端点「{t}」匹配到多个同名步骤，请改用索引")),
            }
        }
        _ => Err(format!("无法识别的连线端点：{v}")),
    }
}

/// 宽松解析连线：{from,to} / {source,target} / [a,b]，端点可为索引或步骤名。
/// 注意：索引基于**调用方传入的 steps 数组**（尚未插入自动开始节点）。
pub fn parse_edges(raw: &Value, steps: &[WorkflowStep]) -> Result<Vec<(usize, usize)>, String> {
    let arr = raw
        .as_array()
        .ok_or("edges 必须是数组，形如 [{\"from\":0,\"to\":1}]")?;
    let mut out = Vec::with_capacity(arr.len());
    for (k, e) in arr.iter().enumerate() {
        let (f, t) = match e {
            Value::Array(p) if p.len() == 2 => (&p[0], &p[1]),
            Value::Object(o) => {
                let f = o.get("from").or_else(|| o.get("source"));
                let t = o.get("to").or_else(|| o.get("target"));
                match (f, t) {
                    (Some(f), Some(t)) => (f, t),
                    _ => return Err(format!("edges[{k}] 缺少 from/to：{e}")),
                }
            }
            _ => return Err(format!("edges[{k}] 格式不正确：{e}。应为 {{\"from\":0,\"to\":1}}")),
        };
        out.push((endpoint(f, steps).map_err(|m| format!("edges[{k}]：{m}"))?, endpoint(t, steps).map_err(|m| format!("edges[{k}]：{m}"))?));
    }
    Ok(out)
}

fn validate(n: usize, edges: &[(usize, usize)], steps: &[WorkflowStep]) -> Result<(), String> {
    for &(f, t) in edges {
        if f >= n || t >= n {
            return Err(format!("连线 {f}→{t} 引用了不存在的步骤（共 {n} 个步骤，索引 0..{}）", n.saturating_sub(1)));
        }
        if f == t {
            return Err(format!("连线 {f}→{t} 是自环，不允许"));
        }
        if is_note(&steps[f]) || is_note(&steps[t]) {
            return Err(format!("连线 {f}→{t} 连接了 note（注释）节点；注释节点不参与执行，不要为它连线"));
        }
        if matches!(steps[t], WorkflowStep::Start { .. }) {
            return Err(format!("连线 {f}→{t} 指向了开始节点；开始节点只能作为起点"));
        }
    }
    // Kahn 判环
    let mut indeg = vec![0usize; n];
    let mut outs = vec![Vec::new(); n];
    for &(f, t) in edges {
        outs[f].push(t);
        indeg[t] += 1;
    }
    let mut q: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut seen = 0;
    while let Some(i) = q.pop_front() {
        seen += 1;
        for &t in &outs[i] {
            indeg[t] -= 1;
            if indeg[t] == 0 {
                q.push_back(t);
            }
        }
    }
    if seen != n {
        return Err("连线存在循环（工作流必须是有向无环图）".into());
    }
    Ok(())
}

pub struct Normalized {
    pub steps: Vec<WorkflowStep>,
    pub edges: Vec<Edge>,
    /// 自动做了哪些处理（返回给 Agent，便于它核对）
    pub notes: Vec<String>,
}

/// 规范化：
/// - edges 为 None 或空数组 → 按顺序串联（跳过 note）；
/// - 没有开始节点 → 在最前插入，并连到所有入度为 0 的可执行节点；
/// - 已有开始节点 → 同样把未被任何节点指向的可执行节点接到开始节点（否则永不执行）；
/// - `relayout` 为 true 或存在未定位节点 → 按连线分层自动布局。
pub fn normalize(
    mut steps: Vec<WorkflowStep>,
    raw_edges: Option<Vec<(usize, usize)>>,
    relayout: bool,
) -> Result<Normalized, String> {
    let mut notes = Vec::new();
    let starts = steps.iter().filter(|s| s.is_start()).count();
    if starts > 1 {
        return Err("只能有一个 start（开始）节点".into());
    }

    let mut edges: Vec<(usize, usize)> = match raw_edges {
        Some(e) if !e.is_empty() => e,
        _ => {
            let exec: Vec<usize> = (0..steps.len()).filter(|&i| !is_note(&steps[i])).collect();
            if exec.len() > 1 {
                notes.push("未提供 edges：已按 steps 顺序串联（note 节点除外）".into());
            }
            exec.windows(2).map(|w| (w[0], w[1])).collect()
        }
    };
    edges.sort_unstable();
    edges.dedup();
    validate(steps.len(), &edges, &steps)?;

    // 开始节点
    let start_idx = match steps.iter().position(|s| s.is_start()) {
        Some(i) => i,
        None => {
            steps.insert(0, WorkflowStep::Start { name: String::new(), x: 0.0, y: 0.0 });
            for e in edges.iter_mut() {
                e.0 += 1;
                e.1 += 1;
            }
            notes.push("已自动在最前插入「开始」节点（索引 0），你给出的步骤索引整体 +1".into());
            0
        }
    };
    let n = steps.len();
    let mut has_in = vec![false; n];
    for &(_, t) in &edges {
        has_in[t] = true;
    }
    let mut linked = Vec::new();
    for i in 0..n {
        if i != start_idx && !has_in[i] && !is_note(&steps[i]) && !edges.iter().any(|&(f, t)| f == start_idx && t == i) {
            edges.push((start_idx, i));
            linked.push(i);
        }
    }
    if linked.len() > 1 {
        notes.push(format!("存在多个无前驱节点 {linked:?}，已全部从「开始」节点并行起跑"));
    }

    edges.sort_unstable();
    let unplaced = steps.iter().any(|s| {
        let m = s.meta();
        m.x == 0.0 && m.y == 0.0
    });
    if relayout || unplaced {
        layout(&mut steps, &edges);
        notes.push("已按连线自动分层布局（自上而下，并行分支横向排开）".into());
    }

    Ok(Normalized {
        steps,
        edges: edges.into_iter().map(|(f, t)| Edge { from: f as i64, to: t as i64 }).collect(),
        notes,
    })
}

/// 与画布「⚙ 自动重排」同规则：最长路径分层，行内按父节点重心排序，整体居中对齐。
pub fn layout(steps: &mut [WorkflowStep], edges: &[(usize, usize)]) {
    let n = steps.len();
    if n == 0 {
        return;
    }
    let mut outs = vec![Vec::new(); n];
    let mut ins = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(f, t) in edges {
        if f < n && t < n {
            outs[f].push(t);
            ins[t].push(f);
            indeg[t] += 1;
        }
    }
    // 拓扑序上求最长路径深度
    let mut depth = vec![0usize; n];
    let mut q: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut d = indeg.clone();
    while let Some(i) = q.pop_front() {
        for &t in &outs[i] {
            depth[t] = depth[t].max(depth[i] + 1);
            d[t] -= 1;
            if d[t] == 0 {
                q.push_back(t);
            }
        }
    }
    // note 节点单独放右侧一列
    let mut layers: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut notes_col = Vec::new();
    for i in 0..n {
        if is_note(&steps[i]) {
            notes_col.push(i);
        } else {
            layers.entry(depth[i]).or_default().push(i);
        }
    }
    // 行内排序：按父节点在上一行的位置重心，减少连线交叉
    let mut order_pos = vec![0.0f64; n];
    for (_, idxs) in layers.iter_mut() {
        idxs.sort_by(|&a, &b| {
            let bary = |i: usize| {
                if ins[i].is_empty() {
                    i as f64
                } else {
                    ins[i].iter().map(|&p| order_pos[p]).sum::<f64>() / ins[i].len() as f64
                }
            };
            bary(a).partial_cmp(&bary(b)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
        });
        for (k, &i) in idxs.iter().enumerate() {
            order_pos[i] = k as f64;
        }
    }
    let row_w = |idxs: &Vec<usize>, steps: &[WorkflowStep]| {
        idxs.iter().map(|&i| node_w(&steps[i])).sum::<f64>() + (idxs.len().saturating_sub(1)) as f64 * COL_GAP
    };
    let max_row = layers.values().map(|r| row_w(r, steps)).fold(0.0, f64::max);
    for (&dep, idxs) in &layers {
        let mut x = LEFT + ((max_row - row_w(idxs, steps)) / 2.0).round();
        let y = TOP + dep as f64 * (NODE_H + ROW_GAP);
        for &i in idxs {
            steps[i].set_pos(x, y);
            x += node_w(&steps[i]) + COL_GAP;
        }
    }
    let note_x = LEFT + max_row + COL_GAP * 1.5;
    for (k, &i) in notes_col.iter().enumerate() {
        steps[i].set_pos(note_x, TOP + k as f64 * (NODE_H + ROW_GAP));
    }
}

/// 给 Agent 看的简明结构：「0 开始 → 1 构建, 2 测试」。
pub fn describe(steps: &[WorkflowStep], edges: &[Edge]) -> Vec<String> {
    let label = |i: usize| {
        let s = &steps[i];
        let m = s.meta();
        let kind = serde_json::to_value(s).ok().and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from)).unwrap_or_default();
        if m.name.is_empty() { format!("{i}[{kind}]") } else { format!("{i}[{kind}]{}", m.name) }
    };
    (0..steps.len())
        .map(|i| {
            let next: Vec<String> = edges.iter().filter(|e| e.from as usize == i).map(|e| label(e.to as usize)).collect();
            if next.is_empty() { label(i) } else { format!("{} → {}", label(i), next.join(", ")) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sh(name: &str) -> WorkflowStep {
        serde_json::from_value(json!({"type":"shell","name":name,"command":"echo"})).unwrap()
    }

    #[test]
    fn chains_when_edges_missing_and_inserts_start() {
        let r = normalize(vec![sh("a"), sh("b"), sh("c")], None, false).unwrap();
        assert!(r.steps[0].is_start());
        let e: Vec<(i64, i64)> = r.edges.iter().map(|e| (e.from, e.to)).collect();
        assert_eq!(e, vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn parallel_roots_all_linked_to_start() {
        let steps = vec![sh("a"), sh("b"), sh("join")];
        let edges = parse_edges(&json!([{"source":"a","target":"join"},[1,2]]), &steps).unwrap();
        let r = normalize(steps, Some(edges), false).unwrap();
        let e: Vec<(i64, i64)> = r.edges.iter().map(|e| (e.from, e.to)).collect();
        assert!(e.contains(&(0, 1)) && e.contains(&(0, 2)) && e.contains(&(1, 3)) && e.contains(&(2, 3)));
        // 并行的 a、b 在同一行且不重叠
        let (ma, mb) = (r.steps[1].meta(), r.steps[2].meta());
        assert_eq!(ma.y, mb.y);
        assert!((ma.x - mb.x).abs() >= NODE_W);
        assert!(r.steps[3].meta().y > ma.y);
    }

    #[test]
    fn rejects_cycles_and_bad_refs() {
        let steps = vec![sh("a"), sh("b")];
        assert!(normalize(steps.clone(), Some(vec![(0, 1), (1, 0)]), false).is_err());
        assert!(normalize(steps.clone(), Some(vec![(0, 5)]), false).is_err());
        assert!(parse_edges(&json!([{"from":"nope","to":0}]), &steps).is_err());
    }
}

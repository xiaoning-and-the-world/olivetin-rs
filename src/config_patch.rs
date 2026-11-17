//! 配置写回：以原文件文本为底做「最小差异补丁」。
//!
//! 为什么不用 `serde_yaml::to_string(&config)` 全量重写：
//! 1. 丢掉所有注释（含 actions 里的分组注释）与行尾注释；
//! 2. 丢掉结构体里没有的未知配置项（历史配置里的 `push.quota_per_day` 就是这样没的）；
//! 3. 把 `${VAR}` 插值后的明文 / 环境变量覆盖值固化进文件；
//! 4. 一旦内存配置不完整（例如被 sed 改坏过），写回就会连带丢掉 action。
//!
//! 本模块的做法：原文本逐行保留，只针对调用方显式声明的改动路径替换对应行，
//! 其余字节（注释、空行、缩进风格、未知字段）原样输出。
//! `baseline` 是「把原文件反序列化成 Config 再序列化」得到的结构，
//! 用于区分「用户真的改了」和「只是默认值被展开」，避免把整个块重写一遍。

use serde_yaml::{Mapping, Value};
use std::collections::{HashMap, HashSet};

/// 一次写回要触及的路径。未列入的路径一律不动。
#[derive(Debug, Default, Clone)]
pub struct ConfigPatch {
    /// 要更新的顶层键，例如 `log_retention_days`、`max_versions`
    pub scalars: Vec<String>,
    /// 要更新的二级键，例如 `("push", "enabled")`
    pub nested: Vec<(String, String)>,
    /// Some 表示同步整个 actions 列表（新增/编辑/删除脚本）
    pub actions: Option<Vec<Value>>,
}

impl ConfigPatch {
    pub fn scalar(mut self, key: &str) -> Self {
        if !self.scalars.iter().any(|s| s == key) {
            self.scalars.push(key.to_string());
        }
        self
    }
    pub fn nested(mut self, section: &str, key: &str) -> Self {
        let pair = (section.to_string(), key.to_string());
        if !self.nested.contains(&pair) {
            self.nested.push(pair);
        }
        self
    }
    pub fn with_actions(mut self, actions: Vec<Value>) -> Self {
        self.actions = Some(actions);
        self
    }
}

fn key_value(k: &str) -> Value {
    Value::String(k.to_string())
}

/// 生成写回后的完整文本。
pub fn apply(
    original: &str,
    baseline: &Mapping,
    new_root: &Mapping,
    patch: &ConfigPatch,
) -> Result<String, String> {
    // split('\n') 而非 lines()：保留末尾换行，join 后字节级还原未改动的部分
    let lines: Vec<String> = original.split('\n').map(|s| s.to_string()).collect();
    let tops = top_level_keys(&lines);
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 16);

    let header_end = tops.first().map(|t| t.0).unwrap_or(lines.len());
    out.extend_from_slice(&lines[..header_end]);

    let scalar_set: HashSet<&str> = patch.scalars.iter().map(|s| s.as_str()).collect();
    let mut nested_by_section: HashMap<&str, Vec<&str>> = HashMap::new();
    for (sec, key) in &patch.nested {
        nested_by_section
            .entry(sec.as_str())
            .or_default()
            .push(key.as_str());
    }

    for n in 0..tops.len() {
        let (start, key) = &tops[n];
        let end = if n + 1 < tops.len() {
            tops[n + 1].0
        } else {
            lines.len()
        };
        let block = &lines[*start..end];

        let rendered = if key == "actions" && patch.actions.is_some() {
            render_actions_block(block, baseline, patch.actions.as_ref().unwrap())?
        } else if scalar_set.contains(key.as_str()) {
            let mut b = block.to_vec();
            if let Some(v) = new_root.get(key_value(key)) {
                if is_scalar(v) {
                    b[0] = replace_line_value(&b[0], v)?;
                } else {
                    // 顶层复合值（如 group_order 序列）：替换整块，保留首行注释
                    b = vec![b[0].clone()];
                    b.extend(render_nested_value("", key, v)?.into_iter().skip(1));
                    // 首行也要变成 `key:`（复合值不能有同行标量）
                    if let Some(first) = b.first_mut() {
                        let (prefix, _, comment) = split_value_comment(first);
                        *first = format!(
                            "{prefix}{comment}",
                            comment = if comment.is_empty() { String::new() } else { format!(" {comment}") }
                        );
                    }
                }
            }
            b
        } else if let Some(keys) = nested_by_section.get(key.as_str()) {
            let old_sec = baseline.get(key_value(key)).and_then(Value::as_mapping);
            let new_sec = new_root.get(key_value(key)).and_then(Value::as_mapping);
            match (old_sec, new_sec) {
                (Some(o), Some(nw)) => patch_mapping_lines(block, o, nw, Some(keys), false)?,
                _ => block.to_vec(), // 结构异常时保守：原样保留，绝不破坏文件
            }
        } else {
            block.to_vec()
        };
        out.extend(rendered);
    }

    // 原文件里不存在的顶层键：追加到末尾（新增设置项时）
    let existing: HashSet<&str> = tops.iter().map(|(_, k)| k.as_str()).collect();
    for key in &patch.scalars {
        if existing.contains(key.as_str()) {
            continue;
        }
        if let Some(v) = new_root.get(key_value(key)) {
            push_blank_if_needed(&mut out);
            out.extend(render_nested_value("", key, v)?);
        }
    }

    // 缺失的 section：按 section 分组，每个只追加一次
    let mut missing_sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for (sec, key) in &patch.nested {
        if existing.contains(sec.as_str()) {
            continue;
        }
        if let Some((_, keys)) = missing_sections.iter_mut().find(|(s, _)| *s == sec.as_str()) {
            keys.push(key.as_str());
        } else {
            missing_sections.push((sec.as_str(), vec![key.as_str()]));
        }
    }
    for (sec, keys) in missing_sections {
        push_blank_if_needed(&mut out);
        out.push(format!("{sec}:"));
        for key in keys {
            if let Some(v) = new_root
                .get(key_value(sec))
                .and_then(Value::as_mapping)
                .and_then(|m| m.get(key_value(key)))
            {
                out.extend(render_nested_value("  ", key, v)?);
            }
        }
    }

    if patch.actions.is_some() && !existing.contains("actions") {
        push_blank_if_needed(&mut out);
        out.push("actions:".to_string());
        for a in patch.actions.as_ref().unwrap() {
            out.extend(render_action_new(a)?);
        }
    }

    Ok(out.join("\n"))
}

fn push_blank_if_needed(out: &mut Vec<String>) {
    if !out.is_empty() && !out.last().unwrap().trim().is_empty() {
        out.push(String::new());
    }
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_))
}

// ============ 顶层 / 行解析 ============

/// 找出所有顶层键行（行首无缩进、形如 `key:`）。
fn top_level_keys(lines: &[String]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(k) = parse_key(line, "") {
            out.push((i, k));
        }
    }
    out
}

/// 解析 `indent + key: rest` 中的 key。
fn parse_key(line: &str, indent: &str) -> Option<String> {
    if !line.starts_with(indent) {
        return None;
    }
    let rest = &line[indent.len()..];
    let first = rest.chars().next()?;
    if first.is_whitespace() || first == '#' || first == '-' {
        return None;
    }
    let colon = rest.find(':')?;
    let key = rest[..colon].trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(key.to_string())
}

/// 拆分一行 `key: value # comment` → (前缀 `indent + key:`, 值, 行尾注释)。
fn split_value_comment(line: &str) -> (String, &str, &str) {
    let colon = match line.find(':') {
        Some(c) => c,
        None => return (line.to_string(), "", ""),
    };
    let prefix = line[..=colon].to_string();
    let after = &line[colon + 1..];
    // YAML 注释必须以空白 + '#' 起
    let mut comment_at = after.len();
    let bytes = after.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            comment_at = i;
            break;
        }
    }
    let value = after[..comment_at].trim();
    let comment = after[comment_at..].trim_end();
    (prefix, value, comment)
}

/// 把一行标量值替换为 `v` 的 YAML 表示，保留缩进、key 与行尾注释。
fn replace_line_value(line: &str, v: &Value) -> Result<String, String> {
    let (prefix, _, comment) = split_value_comment(line);
    let rendered = scalar_text(v)?;
    Ok(format!(
        "{prefix} {rendered}{comment}",
        comment = if comment.is_empty() { String::new() } else { format!(" {comment}") }
    ))
}

/// 标量 → YAML 文本（需要时加双引号）。
pub fn scalar_text(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(quote_string(s)),
        _ => Err("期望标量值".to_string()),
    }
}

fn quote_string(s: &str) -> String {
    let special_start = [
        '-', '?', '!', '&', '*', '[', ']', '{', '}', ',', '#', '|', '>', '@', '`', '"', '\'', '%',
    ];
    let needs_quote = s.is_empty()
        || s.trim() != s
        || s.contains('\n')
        || s.contains('\r')
        || s.contains(" #")
        || s.contains(": ")
        || s.chars().next().map(|c| special_start.contains(&c)).unwrap_or(true)
        || matches!(
            s.to_ascii_lowercase().as_str(),
            "null" | "true" | "false" | "~" | "yes" | "no" | "on" | "off"
        )
        || s.parse::<f64>().is_ok();
    if needs_quote {
        let escaped = s
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

/// 渲染一个 `key: value` 片段（支持复合值，自动缩进），返回多行。
fn render_nested_value(indent: &str, key: &str, v: &Value) -> Result<Vec<String>, String> {
    if is_scalar(v) {
        return Ok(vec![format!("{indent}{key}: {}", scalar_text(v)?)]);
    }
    let mut one = Mapping::new();
    one.insert(key_value(key), v.clone());
    let text = serde_yaml::to_string(&one).map_err(|e| format!("序列化失败: {e}"))?;
    Ok(text
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| format!("{indent}{line}"))
        .collect())
}

// ============ mapping 块（顶层 section / action 字段） ============

/// 对一段 mapping 文本做字段级补丁：
/// - `block[0]` 是起始行（`section:` 或占位行），原样保留；
/// - 只有 `old_map` 与 `new_map` 中值不同的字段会被替换；
/// - 注释行、空行、未知字段、未变更字段原样保留；
/// - 新增字段插到块尾。
fn patch_mapping_lines(
    block: &[String],
    old_map: &Mapping,
    new_map: &Mapping,
    keys_filter: Option<&Vec<&str>>,
    // true：新结构里没有的键视为「被显式清除」，删除该行（action 由结构体整体重建，
    //       无 skip 的字段写 null、skip 的字段直接消失，两种都表示清空）；
    // false：保留原文（顶层 section 要保住用户手写的未知字段，如 push.quota_per_day）。
    drop_missing: bool,
) -> Result<Vec<String>, String> {
    if block.is_empty() {
        return Ok(Vec::new());
    }
    let indent = detect_child_indent(&block[1..]);
    let starts = child_starts(block, &indent);

    let mut out = vec![block[0].clone()];
    let mut seen: HashSet<String> = HashSet::new();

    for idx in 0..starts.len() {
        let (line_idx, key) = &starts[idx];
        let end = if idx + 1 < starts.len() {
            starts[idx + 1].0
        } else {
            block.len()
        };
        let sub = &block[*line_idx..end];
        seen.insert(key.clone());

        let allowed = keys_filter
            .map(|f| f.iter().any(|k| *k == key.as_str()))
            .unwrap_or(true);
        let old_v = old_map.get(key_value(key));
        let new_v = new_map.get(key_value(key));
        let unchanged = match (old_v, new_v) {
            (Some(o), Some(n)) => o == n,
            (None, None) => true,
            _ => false,
        };

        if !allowed || unchanged {
            out.extend_from_slice(sub);
            continue;
        }
        let nv = match new_v {
            Some(v) => v,
            None => {
                // 字段在原文里有、新结构里没有：action 场景下视为被清空（如 push_title 置空）
                if drop_missing && old_map.contains_key(key_value(key)) {
                    continue;
                }
                out.extend_from_slice(sub);
                continue;
            }
        };
        if is_scalar(nv) {
            // 只保留首行（改成新标量），其余续行必须按情况处理：
            // - 旧值是块标量（`|` / `>` / `|-` / `>-`）：后续缩进行是它的正文，
            //   不清掉就会留下 `key: null` + 一堆缩进正文 → 非法 YAML
            //   （清空 command / push_content 时踩过）；
            // - 旧值是 mapping/sequence：后续 `- xxx` / `sub: yyy` 也必须整段丢弃；
            // - 其余（普通单行标量后跟注释）：保留注释行，避免吞掉用户注释。
            let (_, old_lit, _) = split_value_comment(&sub[0]);
            let old_lit = old_lit.trim_start();
            let old_was_block = old_lit.starts_with('|') || old_lit.starts_with('>');
            let mut sub2 = vec![replace_line_value(&sub[0], nv)?];
            if !old_was_block {
                for l in sub.iter().skip(1) {
                    if l.trim_start().starts_with('#') {
                        sub2.push(l.clone());
                    }
                }
            }
            out.extend(sub2);
        } else {
            // 复合值：保留子块内的注释行，其余用新值替换
            let comments: Vec<String> = sub
                .iter()
                .skip(1)
                .filter(|l| l.trim_start().starts_with('#'))
                .cloned()
                .collect();
            let rendered = render_nested_value(&indent, key, nv)?;
            out.extend(rendered);
            out.extend(comments);
        }
    }

    // 首行之后、第一个子字段之前的注释/空行
    if starts.is_empty() {
        out.extend_from_slice(&block[1..]);
    } else if starts[0].0 > 1 {
        out.splice(1..1, block[1..starts[0].0].iter().cloned());
    }

    // 新增字段（文本里没有该行，且值确实变了）
    for (k, nv) in new_map {
        let key = match k.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        if seen.contains(&key) {
            continue;
        }
        if let Some(f) = keys_filter {
            if !f.iter().any(|x| *x == key.as_str()) {
                continue;
            }
        }
        if old_map.get(key_value(&key)) == Some(nv) {
            continue;
        }
        out.extend(render_nested_value(&indent, &key, nv)?);
    }
    Ok(out)
}

/// 检测子字段缩进（第一个非空非注释、有前导空白的行）。
fn detect_child_indent(lines: &[String]) -> String {
    for l in lines {
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        let n = l.len() - l.trim_start().len();
        if n > 0 {
            return " ".repeat(n);
        }
    }
    "  ".to_string()
}

/// 找出 mapping 块里的子字段起始行（索引相对 block）。
fn child_starts(block: &[String], indent: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for i in 1..block.len() {
        if let Some(k) = parse_key(&block[i], indent) {
            out.push((i, k));
        }
    }
    out
}

// ============ actions 块 ============

fn value_id(v: &Value) -> Option<String> {
    v.as_mapping()
        .and_then(|m| m.get(key_value("id")))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn by_id(seq: Option<&[Value]>) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    if let Some(items) = seq {
        for v in items {
            if let Some(id) = value_id(v) {
                m.insert(id, v.clone());
            }
        }
    }
    m
}

fn render_actions_block(
    block: &[String],
    baseline: &Mapping,
    new_actions: &[Value],
) -> Result<Vec<String>, String> {
    let old_by_id = by_id(
        baseline
            .get(key_value("actions"))
            .and_then(Value::as_sequence)
            .map(|v| v.as_slice()),
    );
    let new_by_id = by_id(Some(new_actions));

    let mut out = vec![block[0].clone()]; // `actions:` 行（含行尾注释）
    let rest = &block[1..];

    let starts: Vec<usize> = rest
        .iter()
        .enumerate()
        .filter(|(_, l)| is_action_start(l))
        .map(|(i, _)| i)
        .collect();

    if starts.is_empty() {
        // 原文件没有 action：注释全部保留，再追加新 action
        out.extend_from_slice(rest);
        for v in new_actions {
            out.extend(render_action_new(v)?);
        }
        return Ok(out);
    }

    // 关键：注释行（如 "# 2. 亮灯"）写在下一个 `- id:` 之前，
    // 必须归属「下一个」action，不能算进上一个 action 的尾部，
    // 否则删除上一个 action 时会把下一个 action 的注释一起删掉。
    // 因此每段先回退掉尾部的注释/空行，剩下的才是本段内容。
    let mut content_ends: Vec<usize> = Vec::with_capacity(starts.len());
    for n in 0..starts.len() {
        let s = starts[n];
        let e = if n + 1 < starts.len() {
            starts[n + 1]
        } else {
            rest.len()
        };
        let mut ce = e;
        while ce > s {
            let t = rest[ce - 1].trim();
            if t.is_empty() || t.starts_with('#') {
                ce -= 1;
            } else {
                break;
            }
        }
        content_ends.push(ce);
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut prev_end = 0usize;
    for n in 0..starts.len() {
        let s = starts[n];
        let ce = content_ends[n];
        let leading = &rest[prev_end..s]; // 本 action 的前导注释
        let content = &rest[s..ce]; // 本 action 的内容
        prev_end = ce;

        let id = extract_text_id(content).unwrap_or_default();
        if !id.is_empty() {
            seen.insert(id.clone());
        }
        let Some(nv) = new_by_id.get(&id) else {
            continue; // 删除：连同它自己的前导注释一起移除
        };
        out.extend_from_slice(leading);
        let old_v = old_by_id.get(&id);
        if old_v == Some(nv) {
            out.extend_from_slice(content); // 完全未变：原文照搬
            continue;
        }
        out.extend(patch_action_segment(content, old_v, nv)?);
    }
    // actions 块末尾的注释/空行
    if prev_end < rest.len() {
        out.extend_from_slice(&rest[prev_end..]);
    }

    // 新增的 action（原文件里没有）：沿用原块的序列项缩进风格，
    // 否则 `  - id:` 与 `- id:` 混用会产生非法 YAML。
    let action_indent: String = rest[starts[0]]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    for v in new_actions {
        if let Some(id) = value_id(v) {
            if !seen.contains(&id) {
                for line in render_action_new(v)? {
                    out.push(format!("{action_indent}{line}"));
                }
            }
        }
    }
    Ok(out)
}

fn is_action_start(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- ") && t.contains("id:")
}

/// 从 action 文本段里提取 `id:` 的值（去引号）。
fn extract_text_id(seg: &[String]) -> Option<String> {
    for l in seg.iter().take(3) {
        let t = l.trim_start().trim_start_matches("- ");
        if let Some(pos) = t.find("id:") {
            let v = t[pos + 3..].trim();
            let v = v
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// 变更的 action：保留前导注释与未变字段，只替换真正变化的字段行。
fn patch_action_segment(
    seg: &[String],
    old_v: Option<&Value>,
    new_v: &Value,
) -> Result<Vec<String>, String> {
    let start = seg.iter().position(|l| is_action_start(l)).unwrap_or(0);
    let mut out = seg[..start].to_vec(); // 前导注释
    out.push(seg[start].clone()); // `- id: xxx` 原样

    let fields = &seg[start + 1..];
    let (old_map, new_map) = match (old_v.and_then(Value::as_mapping), new_v.as_mapping()) {
        (Some(o), Some(n)) => (o.clone(), n.clone()),
        _ => {
            // 无法结构化比较时退化为整段替换，但注释仍保留
            out.extend(render_action_new(new_v)?);
            return Ok(out);
        }
    };
    let mut fake = vec!["- placeholder".to_string()];
    fake.extend_from_slice(fields);
    let patched = patch_mapping_lines(&fake, &old_map, &new_map, None, true)?;
    out.extend(patched.into_iter().skip(1));
    Ok(out)
}

/// 渲染一个全新 action 的文本片段（`- id: ...` 起）。
fn render_action_new(v: &Value) -> Result<Vec<String>, String> {
    let text = serde_yaml::to_string(&Value::Sequence(vec![v.clone()]))
        .map_err(|e| format!("序列化 action 失败: {e}"))?;
    Ok(text
        .trim_end_matches('\n')
        .split('\n')
        .map(|s| s.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# 顶部注释\nconfig_version: 1\nlog_retention_days: 7  # 保留天数\n\npush:\n  enabled: false\n  quota_per_day: 100  # 未知字段\n  # 推送说明注释\n  timeout: 10\n\nactions:\n  # 1. 签到任务\n  - id: \"checkin\"\n    title: 签到任务\n    exec: [\"/bin/echo\"]\n    timeout: 60\n  # 2. 亮灯\n  - id: \"led-on\"\n    title: 路由器亮灯\n    exec: [\"/opt/olivetin/led-on.sh\"]\n    timeout: 30\n";

    fn root(s: &str) -> Mapping {
        serde_yaml::from_str::<Value>(s)
            .unwrap()
            .as_mapping()
            .unwrap()
            .clone()
    }
    fn num(n: i64) -> Value {
        serde_yaml::to_value(n).unwrap()
    }

    #[test]
    fn scalar_change_keeps_comments_and_unknown_fields() {        let old = root(SAMPLE);
        let mut new = old.clone();
        new.insert(key_value("log_retention_days"), num(14));
        let patch = ConfigPatch::default().scalar("log_retention_days");
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(out.contains("# 顶部注释"), "顶部注释丢失");
        assert!(out.contains("log_retention_days: 14"), "标量未更新: {out}");
        assert!(out.contains("# 保留天数"), "行尾注释丢失: {out}");
        assert!(out.contains("quota_per_day: 100  # 未知字段"), "未知字段丢失");
        assert!(out.contains("# 推送说明注释"));
        assert!(out.contains("# 1. 签到任务"));
        assert!(out.contains("- id: \"checkin\""));
    }

    #[test]
    fn actions_add_keeps_all_existing_verbatim() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        let mut seq = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .clone();
        let added: Value = serde_yaml::from_str(
            "id: \"led-off\"\ntitle: 关灯\nexec: [\"/opt/olivetin/led-off.sh\"]\ntimeout: 30\n",
        )
        .unwrap();
        seq.push(added);
        new.insert(key_value("actions"), Value::Sequence(seq.clone()));
        let patch = ConfigPatch::default().with_actions(seq);
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(out.contains("# 1. 签到任务"));
        assert!(out.contains("# 2. 亮灯"));
        assert!(out.contains("- id: \"checkin\""));
        assert!(out.contains("- id: \"led-on\""));
        assert!(out.contains("led-off"), "新增 action 丢失: {out}");
        assert_eq!(out.matches("- id:").count(), 3);
        assert!(serde_yaml::from_str::<Value>(&out).is_ok());
    }

    #[test]
    fn actions_delete_only_removes_target() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        let seq: Vec<Value> = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .filter(|v| value_id(v).as_deref() != Some("checkin"))
            .cloned()
            .collect();
        new.insert(key_value("actions"), Value::Sequence(seq.clone()));
        let patch = ConfigPatch::default().with_actions(seq);
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(!out.contains("- id: \"checkin\""));
        assert!(out.contains("- id: \"led-on\""));
        assert!(out.contains("# 2. 亮灯"));
        assert!(serde_yaml::from_str::<Value>(&out).is_ok());
    }

    #[test]
    fn nested_change_keeps_unknown_key() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        let mut push = new
            .get(key_value("push"))
            .unwrap()
            .as_mapping()
            .unwrap()
            .clone();
        push.insert(key_value("enabled"), Value::Bool(true));
        new.insert(key_value("push"), Value::Mapping(push));
        let patch = ConfigPatch::default().nested("push", "enabled");
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(out.contains("enabled: true"));
        assert!(out.contains("quota_per_day: 100  # 未知字段"));
        assert!(out.contains("# 推送说明注释"));
    }

    #[test]
    fn new_top_level_key_appended() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        new.insert(key_value("max_versions"), num(5));
        let patch = ConfigPatch::default().scalar("max_versions");
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(out.contains("max_versions: 5"));
        assert!(out.contains("# 顶部注释"));
        assert!(serde_yaml::from_str::<Value>(&out).is_ok());
    }

    #[test]
    fn output_is_valid_yaml_and_keeps_action_count() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        new.insert(key_value("log_retention_days"), num(30));
        let seq = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .clone();
        let patch = ConfigPatch::default()
            .scalar("log_retention_days")
            .with_actions(seq);
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        let parsed: Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(parsed["actions"].as_sequence().unwrap().len(), 2);
    }

    #[test]
    fn action_field_change_keeps_comment_and_other_actions() {
        let old = root(SAMPLE);
        let mut new = old.clone();
        let mut seq = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .clone();
        let m = seq[0].as_mapping_mut().unwrap();
        m.insert(key_value("timeout"), num(120));
        new.insert(key_value("actions"), Value::Sequence(seq.clone()));
        let patch = ConfigPatch::default().with_actions(seq);
        let out = apply(SAMPLE, &old, &new, &patch).unwrap();
        assert!(out.contains("# 1. 签到任务"));
        assert!(out.contains("timeout: 120"));
        assert!(out.contains("# 2. 亮灯"));
        assert!(out.contains("- id: \"led-on\""));
        assert!(serde_yaml::from_str::<Value>(&out).is_ok(), "输出 YAML 非法: {out}");
    }

    // 清空「多行命令 / 多行推送正文」：旧实现只改首行，块标量的续行留了下来，
    // 写出 `command: null` + 一堆缩进正文 → 非法 YAML（面板保存直接失败）。
    #[test]
    fn scalar_null_removes_block_scalar_body() {
        let sample = "actions:\n  - id: \"a\"\n    title: 测试\n    command: |-\n      echo hi\n      echo there\n    timeout: 30\n";
        let old = root(sample);
        let mut new = old.clone();
        let mut seq = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .clone();
        seq[0]
            .as_mapping_mut()
            .unwrap()
            .insert(key_value("command"), Value::Null);
        new.insert(key_value("actions"), Value::Sequence(seq.clone()));
        let patch = ConfigPatch::default().with_actions(seq);
        let out = apply(sample, &old, &new, &patch).unwrap();
        assert!(out.contains("command: null"), "未写成 null: {out}");
        assert!(!out.contains("echo there"), "块标量正文残留: {out}");
        let parsed: Value = serde_yaml::from_str(&out).expect("输出必须是合法 YAML");
        assert!(parsed["actions"][0]["command"].is_null());
        assert_eq!(parsed["actions"][0]["timeout"], 30, "其它字段不应被改动");
    }

    // 序列 / mapping 改成 null 时同理：不能只换首行、把 `- xxx` 留在原地。
    #[test]
    fn scalar_null_removes_sequence_body() {
        let sample = "actions:\n  - id: \"a\"\n    title: 测试\n    push_uids:\n      - UID_1\n      - UID_2\n    timeout: 30\n";
        let old = root(sample);
        let mut new = old.clone();
        let mut seq = new
            .get(key_value("actions"))
            .unwrap()
            .as_sequence()
            .unwrap()
            .clone();
        seq[0]
            .as_mapping_mut()
            .unwrap()
            .insert(key_value("push_uids"), Value::Null);
        new.insert(key_value("actions"), Value::Sequence(seq.clone()));
        let patch = ConfigPatch::default().with_actions(seq);
        let out = apply(sample, &old, &new, &patch).unwrap();
        assert!(out.contains("push_uids: null"), "未写成 null: {out}");
        assert!(!out.contains("UID_1"), "序列项残留: {out}");
        let parsed: Value = serde_yaml::from_str(&out).expect("输出必须是合法 YAML");
        assert!(parsed["actions"][0]["push_uids"].is_null());
    }
}

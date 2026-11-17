//! 参数合并与拼接测试。

use olivetin_rs::config::ParamDef;
use olivetin_rs::discover::merge_params;

fn param(name: &str, required: bool, style: &str) -> ParamDef {
    ParamDef {
        name: name.to_string(),
        description: None,
        required,
        default: None,
        r#type: "string".to_string(),
        style: style.to_string(),
        options: None,
    }
}

#[test]
fn merge_manual_overrides_discovered() {
    let mut manual = param("port", true, "option");
    manual.default = Some("8080".to_string());
    let discovered = param("port", false, "option");
    let merged = merge_params(&[manual], &[discovered]);
    assert_eq!(merged.len(), 1);
    assert!(merged[0].required);
    assert_eq!(merged[0].default.as_deref(), Some("8080"));
}

#[test]
fn merge_combines_distinct() {
    let a = param("a", false, "option");
    let b = param("b", false, "option");
    let merged = merge_params(&[a], &[b]);
    assert_eq!(merged.len(), 2);
}

#[test]
fn merge_empty_both() {
    let merged = merge_params(&[], &[]);
    assert!(merged.is_empty());
}

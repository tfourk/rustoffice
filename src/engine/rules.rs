//! Client-side mail rules ("Inbox rules").

use crate::config::{Rule, RuleAction, RuleCondition, RuleField, RuleOp};
use crate::model::MessageSummary;

fn test(c: &RuleCondition, m: &MessageSummary, body: &str) -> bool {
    let hay = match c.field {
        RuleField::From => format!("{} <{}>", m.from_name, m.from_addr),
        RuleField::To => format!("{} {}", m.to, m.cc),
        RuleField::Subject => m.subject.clone(),
        RuleField::Body => body.to_string(),
        RuleField::HasAttachment => return m.has_attachments,
    }
    .to_lowercase();
    let needle = c.value.trim().to_lowercase();
    if needle.is_empty() {
        return false;
    }
    match c.op {
        RuleOp::Contains => hay.contains(&needle),
        RuleOp::NotContains => !hay.contains(&needle),
        RuleOp::Is => match c.field {
            RuleField::From => m.from_addr.eq_ignore_ascii_case(&needle) || m.from_name.trim().eq_ignore_ascii_case(&needle),
            _ => hay.trim() == needle,
        },
        RuleOp::StartsWith => match c.field {
            RuleField::From => m.from_addr.to_lowercase().starts_with(&needle) || m.from_name.to_lowercase().starts_with(&needle),
            _ => hay.starts_with(&needle),
        },
        RuleOp::EndsWith => match c.field {
            RuleField::From => m.from_addr.to_lowercase().ends_with(&needle),
            _ => hay.trim_end().ends_with(&needle),
        },
    }
}

pub fn matches(rule: &Rule, m: &MessageSummary, body: &str) -> bool {
    if !rule.enabled || rule.conditions.is_empty() {
        return false;
    }
    if rule.match_all {
        rule.conditions.iter().all(|c| test(c, m, body))
    } else {
        rule.conditions.iter().any(|c| test(c, m, body))
    }
}

pub fn actions_for(rules: &[Rule], account_id: &str, m: &MessageSummary, body: &str) -> Vec<RuleAction> {
    let mut out = Vec::new();
    for r in rules {
        if !r.account_id.is_empty() && r.account_id != account_id {
            continue;
        }
        if matches(r, m, body) {
            out.extend(r.actions.iter().cloned());
            if r.stop_processing {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_matching() {
        let m = MessageSummary {
            from_name: "GitHub".into(),
            from_addr: "notifications@github.com".into(),
            subject: "[repo] New issue".into(),
            ..Default::default()
        };
        let mut r = Rule {
            conditions: vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::EndsWith,
                value: "@github.com".into(),
            }],
            actions: vec![RuleAction::MarkRead],
            ..Default::default()
        };
        assert!(matches(&r, &m, ""));
        r.conditions.push(RuleCondition {
            field: RuleField::Subject,
            op: RuleOp::Contains,
            value: "pull request".into(),
        });
        assert!(!matches(&r, &m, ""));
        r.match_all = false;
        assert!(matches(&r, &m, ""));
        assert_eq!(actions_for(&[r], "x", &m, "").len(), 1);
    }
}

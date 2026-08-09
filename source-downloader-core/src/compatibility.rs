use serde::Serialize;
use source_downloader_sdk::component::{
    ComponentCompatibilityConstraint, ComponentCompatibilityRelation,
    ComponentCompatibilityRule, ComponentId,
};
use std::sync::Arc;

pub const COMPATIBILITY_DSL_VERSION: u32 = 1;

pub(crate) struct CompositionComponent {
    pub id: ComponentId,
    pub rules: Option<Arc<[ComponentCompatibilityRule]>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessorCompatibilityReport {
    pub dsl_version: u32,
    pub valid: bool,
    pub violations: Vec<ComponentCompatibilityViolation>,
}

impl ProcessorCompatibilityReport {
    fn new(violations: Vec<ComponentCompatibilityViolation>) -> Self {
        Self {
            dsl_version: COMPATIBILITY_DSL_VERSION,
            valid: violations.is_empty(),
            violations,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentCompatibilityViolation {
    pub rule_code: String,
    pub owner: ComponentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ComponentId>,
    pub reason: ComponentCompatibilityViolationReason,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentCompatibilityViolationReason {
    RequiredTargetMissing,
    RelationMismatch,
    ForbiddenTarget,
}

pub(crate) fn evaluate_compatibility(
    components: &[CompositionComponent],
) -> ProcessorCompatibilityReport {
    let mut violations = Vec::new();

    for owner in components {
        for rule in owner
            .rules
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|rule| rule.owner == owner.id.component_type)
        {
            match &rule.constraint {
                ComponentCompatibilityConstraint::Requires { target, relations } => {
                    let candidates = components
                        .iter()
                        .filter(|component| target.matches(&component.id.component_type))
                        .collect::<Vec<_>>();
                    if candidates.is_empty() {
                        violations.push(violation(
                            rule,
                            owner,
                            None,
                            ComponentCompatibilityViolationReason::RequiredTargetMissing,
                        ));
                        continue;
                    }

                    let mut failures = Vec::new();
                    let mut matched = false;
                    for candidate in candidates {
                        if relations_match(owner, candidate, relations) {
                            matched = true;
                            break;
                        }
                        failures.push(violation(
                            rule,
                            owner,
                            Some(candidate),
                            ComponentCompatibilityViolationReason::RelationMismatch,
                        ));
                    }
                    if !matched {
                        violations.extend(failures);
                    }
                }
                ComponentCompatibilityConstraint::Forbids { target, relations } => {
                    for candidate in components
                        .iter()
                        .filter(|component| target.matches(&component.id.component_type))
                    {
                        if relations_match(owner, candidate, relations) {
                            violations.push(violation(
                                rule,
                                owner,
                                Some(candidate),
                                ComponentCompatibilityViolationReason::ForbiddenTarget,
                            ));
                        }
                    }
                }
            }
        }
    }

    ProcessorCompatibilityReport::new(violations)
}

fn relations_match(
    owner: &CompositionComponent,
    target: &CompositionComponent,
    relations: &[ComponentCompatibilityRelation],
) -> bool {
    relations.iter().all(|relation| match relation {
        ComponentCompatibilityRelation::InstanceNameEquals => {
            owner.id.name == target.id.name
        }
    })
}

fn violation(
    rule: &ComponentCompatibilityRule,
    owner: &CompositionComponent,
    target: Option<&CompositionComponent>,
    reason: ComponentCompatibilityViolationReason,
) -> ComponentCompatibilityViolation {
    ComponentCompatibilityViolation {
        rule_code: rule.code.clone(),
        owner: owner.id.clone(),
        target: target.map(|component| component.id.clone()),
        reason,
        message: rule.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::{
        ComponentRootType, ComponentSelector, ComponentType,
    };

    fn component(
        root_type: ComponentRootType,
        type_name: &str,
        name: &str,
        rules: Vec<ComponentCompatibilityRule>,
    ) -> CompositionComponent {
        CompositionComponent {
            id: ComponentId::new(
                ComponentType { root_type, name: type_name.to_owned() },
                name,
            ),
            rules: (!rules.is_empty()).then(|| Arc::from(rules)),
        }
    }

    fn same_instance_rule() -> ComponentCompatibilityRule {
        ComponentCompatibilityRule {
            code: "same-instance".to_owned(),
            owner: ComponentType::file_mover("qbittorrent".to_owned()),
            constraint: ComponentCompatibilityConstraint::Requires {
                target: ComponentSelector {
                    root_type: ComponentRootType::Downloader,
                    type_names: vec!["qbittorrent".to_owned()],
                },
                relations: vec![ComponentCompatibilityRelation::InstanceNameEquals],
            },
            message: "instances must match".to_owned(),
        }
    }

    #[test]
    fn requires_accepts_matching_instance_name() {
        let components = vec![
            component(
                ComponentRootType::FileMover,
                "qbittorrent",
                "main",
                vec![same_instance_rule()],
            ),
            component(ComponentRootType::Downloader, "qbittorrent", "main", Vec::new()),
        ];

        let report = evaluate_compatibility(&components);

        assert!(report.valid, "unexpected violations: {:?}", report.violations);
    }

    #[test]
    fn requires_rejects_different_instance_name() {
        let components = vec![
            component(
                ComponentRootType::FileMover,
                "qbittorrent",
                "mover",
                vec![same_instance_rule()],
            ),
            component(
                ComponentRootType::Downloader,
                "qbittorrent",
                "downloader",
                Vec::new(),
            ),
        ];

        let report = evaluate_compatibility(&components);

        assert_eq!(
            report.violations[0].reason,
            ComponentCompatibilityViolationReason::RelationMismatch
        );
    }
}

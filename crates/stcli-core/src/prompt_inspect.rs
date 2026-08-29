use serde::Serialize;

use crate::{MacroEvaluation, PromptPlan, PromptSegment, RegexScriptApplication, StateMutation};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromptSegmentInspection {
    pub selector: String,
    pub segments: Vec<PromptSegmentDetail>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromptSegmentDetail {
    pub index: usize,
    pub segment: PromptSegment,
    pub transformations: PromptSegmentTransformations,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromptSegmentTransformations {
    pub macro_evaluations: Vec<MacroEvaluation>,
    pub regex_applications: Vec<RegexScriptApplication>,
    pub state_mutations: Vec<StateMutation>,
}

impl PromptPlan {
    pub fn inspect_segments(&self, selector: &str) -> Option<PromptSegmentInspection> {
        let matching_indices = self
            .segments
            .iter()
            .enumerate()
            .filter_map(|(index, segment)| (segment.slot == selector).then_some(index))
            .collect::<Vec<_>>();
        let matching_indices = if matching_indices.is_empty() {
            selector
                .parse::<usize>()
                .ok()
                .filter(|index| *index < self.segments.len())
                .into_iter()
                .collect()
        } else {
            matching_indices
        };
        if matching_indices.is_empty() {
            return None;
        }

        let segments = matching_indices
            .into_iter()
            .map(|index| {
                let segment = &self.segments[index];
                PromptSegmentDetail {
                    index,
                    segment: segment.clone(),
                    transformations: PromptSegmentTransformations {
                        macro_evaluations: segment.macro_evaluations.clone(),
                        regex_applications: segment.regex_applications.clone(),
                        state_mutations: segment.state_mutations.clone(),
                    },
                }
            })
            .collect();

        Some(PromptSegmentInspection {
            selector: selector.to_owned(),
            segments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChatRole, GenerationType, LoreResult, PromptPruning, RegexPlacement, StateKey, TokenizerId,
        VariableScope,
    };

    #[test]
    fn slot_selection_precedes_index_and_correlates_transformations() {
        let mut first = PromptSegment::new(
            TokenizerId::Cl100kBase,
            "first",
            "main",
            ChatRole::System,
            "first".to_owned(),
        );
        first.raw_content = "{{setvar::mood::bright}}first".to_owned();
        first.macro_evaluations = vec![MacroEvaluation {
            name: "setvar".to_owned(),
            arguments: vec!["mood".to_owned(), "bright".to_owned()],
            output: String::new(),
        }];
        first.state_mutations = vec![StateMutation {
            key: StateKey {
                scope: VariableScope::Local,
                name: "mood".to_owned(),
            },
            before: None,
            after: None,
        }];
        first.regex_applications = vec![RegexScriptApplication {
            id: "script".to_owned(),
            name: "replace".to_owned(),
            placement: RegexPlacement::UserInput.code(),
            replacements: 1,
        }];
        let numeric_slot = PromptSegment::new(
            TokenizerId::Cl100kBase,
            "numeric-slot",
            "0",
            ChatRole::System,
            "numeric".to_owned(),
        );
        let plan = PromptPlan {
            tokenizer: TokenizerId::Cl100kBase,
            rng_seed: 0,
            segments: vec![first, numeric_slot],
            messages: Vec::new(),
            total_tokens: 0,
            macro_evaluations: Vec::new(),
            macro_warnings: Vec::new(),
            state_mutations: Vec::new(),
            regex_applications: Vec::new(),
            plugin_receipts: Vec::new(),
            lore: LoreResult::default(),
            generation_type: GenerationType::Normal,
            parent_candidate_id: None,
            continuation_prefix: None,
            pruning: PromptPruning {
                context_limit: 0,
                response_reserve: 0,
                prompt_limit: 0,
                kept_tokens: 0,
                pruned_tokens: 0,
            },
        };

        let by_slot = plan.inspect_segments("main").unwrap();
        assert_eq!(by_slot.segments.len(), 1);
        assert_eq!(by_slot.segments[0].index, 0);
        assert_eq!(
            by_slot.segments[0].transformations.macro_evaluations.len(),
            1
        );
        assert_eq!(
            by_slot.segments[0].transformations.regex_applications.len(),
            1
        );
        assert_eq!(by_slot.segments[0].transformations.state_mutations.len(), 1);

        let numeric_slot_wins = plan.inspect_segments("0").unwrap();
        assert_eq!(numeric_slot_wins.segments[0].index, 1);
        assert!(plan.inspect_segments("9").is_none());
    }
}

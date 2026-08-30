use std::collections::{BTreeMap, VecDeque};

use serde::Serialize;
use similar::{ChangeTag, TextDiff as SimilarTextDiff};

use crate::{ChatRole, ContentHash, EntityId, PromptPlan, PromptSegment};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptDiff {
    pub baseline_attempt_id: EntityId,
    pub target_attempt_id: EntityId,
    pub segments: Vec<PromptSegmentDiff>,
    pub token_delta: PromptTokenDelta,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptSegmentDiff {
    pub source: String,
    pub baseline_index: Option<usize>,
    pub target_index: Option<usize>,
    pub baseline: Option<PromptSegmentSnapshot>,
    pub target: Option<PromptSegmentSnapshot>,
    pub changes: Vec<PromptSegmentChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_diff: Option<PromptTextDiff>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptSegmentSnapshot {
    pub slot: String,
    pub role: ChatRole,
    pub content: String,
    pub raw_content: String,
    pub token_count: usize,
    pub source_revision: Option<ContentHash>,
    pub source_field: Option<String>,
    pub in_chat_depth: Option<usize>,
    pub in_chat_order: usize,
    pub truncation_priority: u32,
    pub pruned: bool,
}

impl From<&PromptSegment> for PromptSegmentSnapshot {
    fn from(segment: &PromptSegment) -> Self {
        Self {
            slot: segment.slot.clone(),
            role: segment.role,
            content: segment.content.clone(),
            raw_content: segment.raw_content.clone(),
            token_count: segment.token_count,
            source_revision: segment.source_revision.clone(),
            source_field: segment.source_field.clone(),
            in_chat_depth: segment.in_chat_depth,
            in_chat_order: segment.in_chat_order,
            truncation_priority: segment.truncation_priority,
            pruned: segment.pruned,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptSegmentChange {
    Added,
    Removed,
    Reordered,
    PruningStatusChanged,
    TextModified,
    TokenCountChanged,
    MetadataModified,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptTextDiff {
    pub character: Vec<PromptTextChange>,
    pub word: Vec<PromptTextChange>,
    pub line: Vec<PromptTextChange>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromptTextChange {
    pub kind: PromptTextChangeKind,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptTextChangeKind {
    Equal,
    Insert,
    Delete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PromptTokenDelta {
    pub kept_tokens: i64,
    pub pruned_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SegmentIdentity {
    source: String,
    slot: String,
    role: u8,
    source_revision: Option<String>,
    source_field: Option<String>,
    in_chat_depth: Option<usize>,
    in_chat_order: usize,
}

impl From<&PromptSegment> for SegmentIdentity {
    fn from(segment: &PromptSegment) -> Self {
        Self {
            source: segment.source.clone(),
            slot: segment.slot.clone(),
            role: match segment.role {
                ChatRole::System => 0,
                ChatRole::User => 1,
                ChatRole::Assistant => 2,
            },
            source_revision: segment.source_revision.as_ref().map(ToString::to_string),
            source_field: segment.source_field.clone(),
            in_chat_depth: segment.in_chat_depth,
            in_chat_order: segment.in_chat_order,
        }
    }
}

pub fn diff_prompt_plans(
    baseline_attempt_id: EntityId,
    baseline: &PromptPlan,
    target_attempt_id: EntityId,
    target: &PromptPlan,
) -> PromptDiff {
    let matches = match_segments(&baseline.segments, &target.segments);
    let reordered = reordered_segments(&matches);
    let mut target_matched = vec![false; target.segments.len()];
    let mut segments = Vec::new();

    for (baseline_index, target_index) in matches.into_iter().enumerate() {
        let baseline_segment = &baseline.segments[baseline_index];
        let Some(target_index) = target_index else {
            segments.push(removed_segment(baseline_index, baseline_segment));
            continue;
        };
        target_matched[target_index] = true;
        let target_segment = &target.segments[target_index];
        let mut changes = Vec::new();
        if reordered[baseline_index] {
            changes.push(PromptSegmentChange::Reordered);
        }
        if baseline_segment.pruned != target_segment.pruned {
            changes.push(PromptSegmentChange::PruningStatusChanged);
        }
        if baseline_segment.content != target_segment.content {
            changes.push(PromptSegmentChange::TextModified);
        }
        if baseline_segment.token_count != target_segment.token_count {
            changes.push(PromptSegmentChange::TokenCountChanged);
        }
        if segment_metadata_changed(baseline_segment, target_segment) {
            changes.push(PromptSegmentChange::MetadataModified);
        }
        if changes.is_empty() {
            continue;
        }
        segments.push(PromptSegmentDiff {
            source: target_segment.source.clone(),
            baseline_index: Some(baseline_index),
            target_index: Some(target_index),
            baseline: Some(baseline_segment.into()),
            target: Some(target_segment.into()),
            text_diff: (baseline_segment.content != target_segment.content)
                .then(|| prompt_text_diff(&baseline_segment.content, &target_segment.content)),
            changes,
        });
    }

    for (target_index, target_segment) in target.segments.iter().enumerate() {
        if !target_matched[target_index] {
            segments.push(added_segment(target_index, target_segment));
        }
    }
    segments.sort_by_key(|segment| {
        segment
            .target_index
            .or(segment.baseline_index)
            .unwrap_or(usize::MAX)
    });

    PromptDiff {
        baseline_attempt_id,
        target_attempt_id,
        segments,
        token_delta: PromptTokenDelta {
            kept_tokens: delta(baseline.pruning.kept_tokens, target.pruning.kept_tokens),
            pruned_tokens: delta(baseline.pruning.pruned_tokens, target.pruning.pruned_tokens),
            total_tokens: delta(baseline.total_tokens, target.total_tokens),
        },
    }
}

fn match_segments(baseline: &[PromptSegment], target: &[PromptSegment]) -> Vec<Option<usize>> {
    let mut exact = BTreeMap::<SegmentIdentity, VecDeque<usize>>::new();
    for (index, segment) in target.iter().enumerate() {
        exact.entry(segment.into()).or_default().push_back(index);
    }
    let mut matches = vec![None; baseline.len()];
    let mut target_matched = vec![false; target.len()];
    for (index, segment) in baseline.iter().enumerate() {
        if let Some(target_index) = exact.get_mut(&segment.into()).and_then(VecDeque::pop_front) {
            matches[index] = Some(target_index);
            target_matched[target_index] = true;
        }
    }

    let mut by_source = BTreeMap::<&str, VecDeque<usize>>::new();
    for (index, segment) in target.iter().enumerate() {
        if !target_matched[index] {
            by_source
                .entry(segment.source.as_str())
                .or_default()
                .push_back(index);
        }
    }
    for (index, segment) in baseline.iter().enumerate() {
        if matches[index].is_none()
            && let Some(target_index) = by_source
                .get_mut(segment.source.as_str())
                .and_then(VecDeque::pop_front)
        {
            matches[index] = Some(target_index);
        }
    }
    matches
}

fn reordered_segments(matches: &[Option<usize>]) -> Vec<bool> {
    let common = matches
        .iter()
        .enumerate()
        .filter_map(|(baseline_index, target_index)| {
            target_index.map(|target_index| (baseline_index, target_index))
        })
        .collect::<Vec<_>>();
    let mut prefix_max = Vec::with_capacity(common.len());
    let mut maximum = None;
    for (_, target_index) in &common {
        prefix_max.push(maximum);
        maximum = Some(maximum.map_or(*target_index, |value: usize| value.max(*target_index)));
    }
    let mut suffix_min = vec![None; common.len()];
    let mut minimum = None;
    for (index, (_, target_index)) in common.iter().enumerate().rev() {
        suffix_min[index] = minimum;
        minimum = Some(minimum.map_or(*target_index, |value: usize| value.min(*target_index)));
    }

    let mut reordered = vec![false; matches.len()];
    for (index, (baseline_index, target_index)) in common.into_iter().enumerate() {
        reordered[baseline_index] = prefix_max[index].is_some_and(|prior| prior > target_index)
            || suffix_min[index].is_some_and(|later| later < target_index);
    }
    reordered
}

fn segment_metadata_changed(baseline: &PromptSegment, target: &PromptSegment) -> bool {
    baseline.source != target.source
        || baseline.slot != target.slot
        || baseline.role != target.role
        || baseline.source_revision != target.source_revision
        || baseline.source_field != target.source_field
        || baseline.in_chat_depth != target.in_chat_depth
        || baseline.in_chat_order != target.in_chat_order
        || baseline.truncation_priority != target.truncation_priority
}

fn added_segment(target_index: usize, segment: &PromptSegment) -> PromptSegmentDiff {
    PromptSegmentDiff {
        source: segment.source.clone(),
        baseline_index: None,
        target_index: Some(target_index),
        baseline: None,
        target: Some(segment.into()),
        changes: vec![PromptSegmentChange::Added],
        text_diff: Some(prompt_text_diff("", &segment.content)),
    }
}

fn removed_segment(baseline_index: usize, segment: &PromptSegment) -> PromptSegmentDiff {
    PromptSegmentDiff {
        source: segment.source.clone(),
        baseline_index: Some(baseline_index),
        target_index: None,
        baseline: Some(segment.into()),
        target: None,
        changes: vec![PromptSegmentChange::Removed],
        text_diff: Some(prompt_text_diff(&segment.content, "")),
    }
}

fn prompt_text_diff(baseline: &str, target: &str) -> PromptTextDiff {
    PromptTextDiff {
        character: collect_text_changes(SimilarTextDiff::from_chars(baseline, target)),
        word: collect_text_changes(SimilarTextDiff::from_words(baseline, target)),
        line: collect_text_changes(SimilarTextDiff::from_lines(baseline, target)),
    }
}

fn collect_text_changes(diff: SimilarTextDiff<'_, '_, str>) -> Vec<PromptTextChange> {
    let mut changes: Vec<PromptTextChange> = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => PromptTextChangeKind::Equal,
            ChangeTag::Insert => PromptTextChangeKind::Insert,
            ChangeTag::Delete => PromptTextChangeKind::Delete,
        };
        let value = change.value();
        if let Some(last) = changes.last_mut()
            && last.kind == kind
        {
            last.value.push_str(value);
        } else {
            changes.push(PromptTextChange {
                kind,
                value: value.to_owned(),
            });
        }
    }
    changes
}

fn delta(baseline: usize, target: usize) -> i64 {
    target as i64 - baseline as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GenerationType, LoreResult, PromptPruning, TokenizerId};

    #[test]
    fn itemizes_structural_text_and_token_changes() {
        let baseline = plan(
            vec![
                segment("alpha", "old alpha", 2, false),
                segment("beta", "old beta", 2, false),
                segment("removed", "gone", 1, false),
                segment("pruned", "limited", 2, false),
            ],
            8,
            1,
        );
        let target = plan(
            vec![
                segment("beta", "new beta", 3, false),
                segment("alpha", "old alpha", 2, false),
                segment("pruned", "limited", 2, true),
                segment("added", "new", 1, false),
            ],
            5,
            3,
        );

        let diff = diff_prompt_plans(EntityId::new(), &baseline, EntityId::new(), &target);

        assert_eq!(
            diff.token_delta,
            PromptTokenDelta {
                kept_tokens: -3,
                pruned_tokens: 2,
                total_tokens: -3,
            }
        );
        assert!(has_change(&diff, "alpha", PromptSegmentChange::Reordered));
        assert!(has_change(&diff, "beta", PromptSegmentChange::Reordered));
        assert!(has_change(&diff, "beta", PromptSegmentChange::TextModified));
        assert!(has_change(&diff, "removed", PromptSegmentChange::Removed));
        assert!(has_change(&diff, "added", PromptSegmentChange::Added));
        assert!(has_change(
            &diff,
            "pruned",
            PromptSegmentChange::PruningStatusChanged
        ));
        let added = diff
            .segments
            .iter()
            .find(|segment| segment.source == "added")
            .unwrap();
        assert_eq!(added.target.as_ref().unwrap().content, "new");
        assert!(
            added
                .text_diff
                .as_ref()
                .unwrap()
                .line
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Insert)
        );
        let removed = diff
            .segments
            .iter()
            .find(|segment| segment.source == "removed")
            .unwrap();
        assert_eq!(removed.baseline.as_ref().unwrap().content, "gone");
        assert!(
            removed
                .text_diff
                .as_ref()
                .unwrap()
                .line
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Delete)
        );
        let beta = diff
            .segments
            .iter()
            .find(|segment| segment.source == "beta")
            .unwrap();
        let text = beta.text_diff.as_ref().unwrap();
        assert!(
            text.word
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Delete)
        );
        assert!(
            text.word
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Insert)
        );
        assert!(
            text.character
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Insert)
        );
        assert!(
            text.line
                .iter()
                .any(|change| change.kind == PromptTextChangeKind::Delete)
        );
    }

    fn plan(segments: Vec<PromptSegment>, kept_tokens: usize, pruned_tokens: usize) -> PromptPlan {
        PromptPlan {
            tokenizer: TokenizerId::O200kBase,
            rng_seed: 0,
            segments,
            messages: Vec::new(),
            total_tokens: kept_tokens,
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
                context_limit: 10,
                response_reserve: 0,
                prompt_limit: 10,
                kept_tokens,
                pruned_tokens,
            },
            format_mode: Default::default(),
            text_prompt: None,
            stop_sequences: Vec::new(),
        }
    }

    fn segment(source: &str, content: &str, token_count: usize, pruned: bool) -> PromptSegment {
        let mut segment = PromptSegment::new(
            TokenizerId::O200kBase,
            source,
            "test",
            ChatRole::System,
            content.to_owned(),
        );
        segment.token_count = token_count;
        segment.pruned = pruned;
        segment
    }

    fn has_change(diff: &PromptDiff, source: &str, change: PromptSegmentChange) -> bool {
        diff.segments
            .iter()
            .any(|segment| segment.source == source && segment.changes.contains(&change))
    }
}

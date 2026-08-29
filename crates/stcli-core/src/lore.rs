use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{ContentHash, EcmaRegexError, EcmaRegexWorker, MacroError, TokenizerId};

const DEFAULT_SCAN_DEPTH: usize = 2;
const DEFAULT_ENTRY_DEPTH: usize = 4;
const DEFAULT_GROUP_WEIGHT: u64 = 100;
const DEFAULT_MAX_RECURSION_STEPS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LorePosition {
    Before,
    After,
    AuthorNoteTop,
    AuthorNoteBottom,
    AtDepth,
    ExampleTop,
    ExampleBottom,
    Outlet,
}

impl LorePosition {
    fn from_value(value: Option<&Value>, card_position: Option<&str>) -> Self {
        match value.and_then(Value::as_i64) {
            Some(0) => Self::Before,
            Some(2) => Self::AuthorNoteTop,
            Some(3) => Self::AuthorNoteBottom,
            Some(4) => Self::AtDepth,
            Some(5) => Self::ExampleTop,
            Some(6) => Self::ExampleBottom,
            Some(7) => Self::Outlet,
            _ if card_position == Some("before_char") => Self::Before,
            _ => Self::After,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectiveLogic {
    AndAny,
    NotAll,
    NotAny,
    AndAll,
}

impl SelectiveLogic {
    fn from_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_i64) {
            Some(1) => Self::NotAll,
            Some(2) => Self::NotAny,
            Some(3) => Self::AndAll,
            _ => Self::AndAny,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoreEntry {
    pub source_revision: ContentHash,
    pub source_index: usize,
    pub id: String,
    pub keys: Vec<String>,
    pub secondary_keys: Vec<String>,
    pub content: String,
    pub enabled: bool,
    pub constant: bool,
    pub selective: bool,
    pub selective_logic: SelectiveLogic,
    pub insertion_order: i64,
    pub position: LorePosition,
    pub depth: usize,
    pub role: i64,
    pub outlet: String,
    pub exclude_recursion: bool,
    pub prevent_recursion: bool,
    pub delay_until_recursion: usize,
    pub scan_depth: Option<usize>,
    pub case_sensitive: Option<bool>,
    pub match_whole_words: Option<bool>,
    pub probability: f64,
    pub use_probability: bool,
    pub group: Vec<String>,
    pub group_override: bool,
    pub group_weight: u64,
    pub sticky: usize,
    pub cooldown: usize,
    pub delay: usize,
    pub ignore_budget: bool,
    pub triggers: Vec<String>,
}

impl LoreEntry {
    pub fn key(&self) -> String {
        format!("{}.{}", self.source_revision, self.id)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoreSettings {
    pub scan_depth: usize,
    pub recursive: bool,
    pub max_recursion_steps: usize,
    pub budget_tokens: usize,
    pub case_sensitive: bool,
    pub match_whole_words: bool,
    pub use_group_scoring: bool,
    pub generation_type: String,
    pub rng_seed: u64,
    pub message_count: usize,
    pub prior_activations: BTreeMap<String, usize>,
}

impl Default for LoreSettings {
    fn default() -> Self {
        Self {
            scan_depth: DEFAULT_SCAN_DEPTH,
            recursive: true,
            max_recursion_steps: DEFAULT_MAX_RECURSION_STEPS,
            budget_tokens: 512,
            case_sensitive: false,
            match_whole_words: false,
            use_group_scoring: false,
            generation_type: "normal".to_owned(),
            rng_seed: 1,
            message_count: 0,
            prior_activations: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoreDecisionOutcome {
    Disabled,
    TriggerFiltered,
    DelaySuppressed,
    CooldownSuppressed,
    RecursionSuppressed,
    NoPrimaryMatch,
    SecondaryRejected,
    GroupRejected,
    ProbabilityRejected,
    BudgetRejected,
    Activated,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LoreDecision {
    pub entry_key: String,
    pub recursion_step: usize,
    pub primary_matches: Vec<String>,
    pub secondary_matches: Vec<String>,
    pub score: usize,
    pub group: Option<String>,
    pub group_draw: Option<u64>,
    pub probability_draw: Option<f64>,
    pub tokens: usize,
    pub outcome: LoreDecisionOutcome,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ActivatedLore {
    pub entry_key: String,
    pub source_revision: ContentHash,
    pub content: String,
    pub insertion_order: i64,
    pub position: LorePosition,
    pub depth: usize,
    pub role: i64,
    pub outlet: String,
    pub tokens: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LoreResult {
    pub activated: Vec<ActivatedLore>,
    pub decisions: Vec<LoreDecision>,
    pub used_tokens: usize,
    pub budget_tokens: usize,
    pub overflowed: bool,
}

pub struct LoreEngine {
    tokenizer: TokenizerId,
    regex: EcmaRegexWorker,
}

impl LoreEngine {
    pub fn new(tokenizer: TokenizerId) -> Result<Self, LoreError> {
        Ok(Self {
            tokenizer,
            regex: EcmaRegexWorker::current(Duration::from_millis(250))?,
        })
    }

    pub fn with_worker(tokenizer: TokenizerId, regex: EcmaRegexWorker) -> Self {
        Self { tokenizer, regex }
    }

    pub fn evaluate(
        &self,
        entries: &[LoreEntry],
        messages_newest_first: &[String],
        settings: &LoreSettings,
    ) -> Result<LoreResult, LoreError> {
        self.evaluate_with(
            entries,
            messages_newest_first,
            settings,
            |entry| Ok(entry.content.clone()),
            |pattern, flags, text| {
                self.regex
                    .is_match(pattern, flags, text)
                    .map_err(LoreError::Regex)
            },
        )
    }

    pub fn evaluate_transformed<F>(
        &self,
        entries: &[LoreEntry],
        messages_newest_first: &[String],
        settings: &LoreSettings,
        transform: F,
    ) -> Result<LoreResult, LoreError>
    where
        F: FnMut(&LoreEntry) -> Result<String, LoreError>,
    {
        self.evaluate_with(
            entries,
            messages_newest_first,
            settings,
            transform,
            |pattern, flags, text| {
                self.regex
                    .is_match(pattern, flags, text)
                    .map_err(LoreError::Regex)
            },
        )
    }

    pub fn evaluate_in_process(
        &self,
        entries: &[LoreEntry],
        messages_newest_first: &[String],
        settings: &LoreSettings,
    ) -> Result<LoreResult, LoreError> {
        self.evaluate_with(
            entries,
            messages_newest_first,
            settings,
            |entry| Ok(entry.content.clone()),
            |pattern, flags, text| match crate::run_worker(crate::RegexRequest {
                pattern: pattern.to_owned(),
                flags: flags.to_owned(),
                text: text.to_owned(),
            }) {
                crate::RegexResponse::Match { matched } => Ok(matched),
                crate::RegexResponse::Error { message } => Err(LoreError::RegexPattern(message)),
            },
        )
    }

    fn evaluate_with<F, R>(
        &self,
        entries: &[LoreEntry],
        messages_newest_first: &[String],
        settings: &LoreSettings,
        mut transform: F,
        mut regex_match: R,
    ) -> Result<LoreResult, LoreError>
    where
        F: FnMut(&LoreEntry) -> Result<String, LoreError>,
        R: FnMut(&str, &str, &str) -> Result<bool, LoreError>,
    {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|left, right| {
            right
                .insertion_order
                .cmp(&left.insertion_order)
                .then(left.source_index.cmp(&right.source_index))
                .then(left.id.cmp(&right.id))
        });
        let mut result = LoreResult {
            budget_tokens: settings.budget_tokens.max(1),
            ..LoreResult::default()
        };
        let mut activated = BTreeSet::new();
        let mut recursion_text = Vec::new();
        let mut rng = LoreRng(settings.rng_seed);

        for step in 0..settings.max_recursion_steps.max(1) {
            let mut candidates = Vec::new();
            for entry in &sorted {
                let entry_key = entry.key();
                if activated.contains(&entry_key) {
                    continue;
                }
                let mut decision = LoreDecision {
                    entry_key: entry_key.clone(),
                    recursion_step: step,
                    primary_matches: Vec::new(),
                    secondary_matches: Vec::new(),
                    score: 0,
                    group: entry.group.first().cloned(),
                    group_draw: None,
                    probability_draw: None,
                    tokens: 0,
                    outcome: LoreDecisionOutcome::NoPrimaryMatch,
                };
                if !entry.enabled {
                    decision.outcome = LoreDecisionOutcome::Disabled;
                    result.decisions.push(decision);
                    continue;
                }
                if !entry.triggers.is_empty()
                    && !entry
                        .triggers
                        .iter()
                        .any(|trigger| trigger == &settings.generation_type)
                {
                    decision.outcome = LoreDecisionOutcome::TriggerFiltered;
                    result.decisions.push(decision);
                    continue;
                }
                if settings.message_count < entry.delay {
                    decision.outcome = LoreDecisionOutcome::DelaySuppressed;
                    result.decisions.push(decision);
                    continue;
                }
                let prior = settings.prior_activations.get(&entry_key).copied();
                let sticky = prior.is_some_and(|at| settings.message_count < at + entry.sticky);
                let cooldown = prior.is_some_and(|at| settings.message_count < at + entry.cooldown);
                if cooldown && !sticky {
                    decision.outcome = LoreDecisionOutcome::CooldownSuppressed;
                    result.decisions.push(decision);
                    continue;
                }
                if step == 0 && entry.delay_until_recursion > 0 && !sticky {
                    decision.outcome = LoreDecisionOutcome::RecursionSuppressed;
                    result.decisions.push(decision);
                    continue;
                }
                if step > 0
                    && (entry.exclude_recursion || step < entry.delay_until_recursion)
                    && !sticky
                {
                    decision.outcome = LoreDecisionOutcome::RecursionSuppressed;
                    result.decisions.push(decision);
                    continue;
                }

                let scan_depth = entry.scan_depth.unwrap_or(settings.scan_depth);
                let mut scan = messages_newest_first
                    .iter()
                    .take(scan_depth)
                    .cloned()
                    .collect::<Vec<_>>();
                if step > 0 {
                    scan.extend(recursion_text.iter().cloned());
                }
                let scan = scan.join("\n\u{1}");
                if entry.constant || sticky {
                    decision.score = usize::MAX;
                    candidates.push((entry, decision));
                    continue;
                }
                for key in &entry.keys {
                    if matches_key(
                        &scan,
                        key,
                        entry.case_sensitive.unwrap_or(settings.case_sensitive),
                        entry
                            .match_whole_words
                            .unwrap_or(settings.match_whole_words),
                        &mut regex_match,
                    )? {
                        decision.primary_matches.push(key.clone());
                    }
                }
                decision.score = decision.primary_matches.len();
                if decision.primary_matches.is_empty() {
                    result.decisions.push(decision);
                    continue;
                }
                for key in &entry.secondary_keys {
                    if matches_key(
                        &scan,
                        key,
                        entry.case_sensitive.unwrap_or(settings.case_sensitive),
                        entry
                            .match_whole_words
                            .unwrap_or(settings.match_whole_words),
                        &mut regex_match,
                    )? {
                        decision.secondary_matches.push(key.clone());
                    }
                }
                if entry.selective && !secondary_matches(entry, decision.secondary_matches.len()) {
                    decision.outcome = LoreDecisionOutcome::SecondaryRejected;
                    result.decisions.push(decision);
                    continue;
                }
                decision.score += decision.secondary_matches.len();
                candidates.push((entry, decision));
            }

            choose_groups(&mut candidates, &mut result.decisions, settings, &mut rng);
            if candidates.is_empty() {
                break;
            }

            let mut activated_this_step = Vec::new();
            for (entry, mut decision) in candidates {
                if entry.use_probability && entry.probability < 100.0 {
                    let draw = rng.unit() * 100.0;
                    decision.probability_draw = Some(draw);
                    if draw > entry.probability.clamp(0.0, 100.0) {
                        decision.outcome = LoreDecisionOutcome::ProbabilityRejected;
                        result.decisions.push(decision);
                        continue;
                    }
                }
                let content = transform(entry)?;
                let tokens = self.tokenizer.count(&content);
                decision.tokens = tokens;
                if !entry.ignore_budget
                    && result.used_tokens.saturating_add(tokens) >= result.budget_tokens
                {
                    decision.outcome = LoreDecisionOutcome::BudgetRejected;
                    result.overflowed = true;
                    result.decisions.push(decision);
                    continue;
                }
                result.used_tokens += tokens;
                activated.insert(entry.key());
                if !entry.prevent_recursion {
                    activated_this_step.push(content.clone());
                }
                result.activated.push(ActivatedLore {
                    entry_key: entry.key(),
                    source_revision: entry.source_revision.clone(),
                    content,
                    insertion_order: entry.insertion_order,
                    position: entry.position,
                    depth: entry.depth,
                    role: entry.role,
                    outlet: entry.outlet.clone(),
                    tokens,
                });
                decision.outcome = LoreDecisionOutcome::Activated;
                result.decisions.push(decision);
            }
            if !settings.recursive || activated_this_step.is_empty() || result.overflowed {
                break;
            }
            recursion_text.extend(activated_this_step);
        }
        Ok(result)
    }
}

pub fn parse_lore_entries(
    revision: &ContentHash,
    semantic: &Value,
    source_index: usize,
) -> Result<Vec<LoreEntry>, LoreError> {
    let entries = semantic
        .get("entries")
        .or_else(|| {
            semantic
                .get("data")
                .and_then(|data| data.get("character_book"))
                .and_then(|book| book.get("entries"))
        })
        .ok_or(LoreError::MissingEntries)?;
    let values: Vec<(String, &Value)> = if let Some(entries) = entries.as_array() {
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let id = entry
                    .get("id")
                    .map(value_id)
                    .unwrap_or_else(|| index.to_string());
                (id, entry)
            })
            .collect()
    } else if let Some(entries) = entries.as_object() {
        entries
            .iter()
            .map(|(id, entry)| (id.clone(), entry))
            .collect()
    } else {
        return Err(LoreError::InvalidEntries);
    };
    if values.len() > crate::limits::MAX_LORE_ENTRIES_PER_SOURCE {
        return Err(LoreError::TooManyEntries {
            count: values.len(),
            limit: crate::limits::MAX_LORE_ENTRIES_PER_SOURCE,
        });
    }
    values
        .into_iter()
        .map(|(id, value)| parse_entry(revision, source_index, id, value))
        .collect()
}

fn parse_entry(
    revision: &ContentHash,
    source_index: usize,
    id: String,
    value: &Value,
) -> Result<LoreEntry, LoreError> {
    let object = value
        .as_object()
        .ok_or(LoreError::InvalidEntry(id.clone()))?;
    let plugins = object.get("plugins").and_then(Value::as_object);
    let ext = |snake: &str, camel: &str| {
        plugins
            .and_then(|value| value.get(snake))
            .or_else(|| object.get(camel))
    };
    Ok(LoreEntry {
        source_revision: revision.clone(),
        source_index,
        id,
        keys: string_array(object.get("keys").or_else(|| object.get("key"))),
        secondary_keys: string_array(
            object
                .get("secondary_keys")
                .or_else(|| object.get("keysecondary")),
        ),
        content: string(object.get("content")),
        enabled: object
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| !bool_value(object.get("disable"), false)),
        constant: bool_value(object.get("constant"), false),
        selective: bool_value(object.get("selective"), false),
        selective_logic: SelectiveLogic::from_value(ext("selective_logic", "selectiveLogic")),
        insertion_order: object
            .get("insertion_order")
            .or_else(|| object.get("order"))
            .and_then(Value::as_i64)
            .unwrap_or(100),
        position: LorePosition::from_value(
            ext("position", "position"),
            object.get("position").and_then(Value::as_str),
        ),
        depth: usize_value(ext("depth", "depth"), DEFAULT_ENTRY_DEPTH),
        role: ext("role", "role").and_then(Value::as_i64).unwrap_or(0),
        outlet: string(ext("outlet_name", "outletName")),
        exclude_recursion: bool_value(ext("exclude_recursion", "excludeRecursion"), false),
        prevent_recursion: bool_value(ext("prevent_recursion", "preventRecursion"), false),
        delay_until_recursion: usize_value(ext("delay_until_recursion", "delayUntilRecursion"), 0),
        scan_depth: optional_usize(ext("scan_depth", "scanDepth")),
        case_sensitive: ext("case_sensitive", "caseSensitive").and_then(Value::as_bool),
        match_whole_words: ext("match_whole_words", "matchWholeWords").and_then(Value::as_bool),
        probability: ext("probability", "probability")
            .and_then(Value::as_f64)
            .unwrap_or(100.0),
        use_probability: bool_value(ext("use_probability", "useProbability"), true),
        group: string(ext("group", "group"))
            .split(',')
            .map(str::trim)
            .filter(|group| !group.is_empty())
            .map(str::to_owned)
            .collect(),
        group_override: bool_value(ext("group_override", "groupOverride"), false),
        group_weight: ext("group_weight", "groupWeight")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_GROUP_WEIGHT),
        sticky: usize_value(ext("sticky", "sticky"), 0),
        cooldown: usize_value(ext("cooldown", "cooldown"), 0),
        delay: usize_value(ext("delay", "delay"), 0),
        ignore_budget: bool_value(ext("ignore_budget", "ignoreBudget"), false),
        triggers: string_array(ext("triggers", "triggers")),
    })
}

fn secondary_matches(entry: &LoreEntry, matches: usize) -> bool {
    let total = entry.secondary_keys.len();
    if total == 0 {
        return true;
    }
    match entry.selective_logic {
        SelectiveLogic::AndAny => matches > 0,
        SelectiveLogic::NotAll => matches < total,
        SelectiveLogic::NotAny => matches == 0,
        SelectiveLogic::AndAll => matches == total,
    }
}

fn choose_groups(
    candidates: &mut Vec<(&LoreEntry, LoreDecision)>,
    rejected: &mut Vec<LoreDecision>,
    settings: &LoreSettings,
    rng: &mut LoreRng,
) {
    let mut groups = BTreeMap::<String, Vec<usize>>::new();
    for (index, (entry, _)) in candidates.iter().enumerate() {
        for group in &entry.group {
            groups.entry(group.clone()).or_default().push(index);
        }
    }
    let mut losers = BTreeSet::new();
    for (group, members) in groups {
        let available = members
            .into_iter()
            .filter(|index| !losers.contains(index))
            .collect::<Vec<_>>();
        if available.len() <= 1 {
            continue;
        }
        let winner = available
            .iter()
            .copied()
            .filter(|index| candidates[*index].0.group_override)
            .max_by_key(|index| candidates[*index].0.insertion_order)
            .or_else(|| {
                if settings.use_group_scoring {
                    let best = available
                        .iter()
                        .map(|index| candidates[*index].1.score)
                        .max()
                        .unwrap_or(0);
                    let scored = available
                        .iter()
                        .copied()
                        .filter(|index| candidates[*index].1.score == best)
                        .collect::<Vec<_>>();
                    weighted_choice(&scored, candidates, rng)
                } else {
                    weighted_choice(&available, candidates, rng)
                }
            });
        if let Some(winner) = winner {
            for index in available {
                candidates[index].1.group = Some(group.clone());
                if index != winner {
                    losers.insert(index);
                }
            }
        }
    }
    for index in losers.into_iter().rev() {
        let (_, mut decision) = candidates.remove(index);
        decision.outcome = LoreDecisionOutcome::GroupRejected;
        rejected.push(decision);
    }
}

fn weighted_choice(
    members: &[usize],
    candidates: &[(&LoreEntry, LoreDecision)],
    rng: &mut LoreRng,
) -> Option<usize> {
    let total = members
        .iter()
        .map(|index| candidates[*index].0.group_weight)
        .sum::<u64>();
    if total == 0 {
        return members.first().copied();
    }
    let draw = rng.next() % total;
    let mut cursor = 0;
    for index in members {
        cursor += candidates[*index].0.group_weight;
        if draw < cursor {
            return Some(*index);
        }
    }
    members.last().copied()
}

fn matches_key<F>(
    haystack: &str,
    key: &str,
    case_sensitive: bool,
    whole_words: bool,
    regex_match: &mut F,
) -> Result<bool, LoreError>
where
    F: FnMut(&str, &str, &str) -> Result<bool, LoreError>,
{
    if let Some((pattern, flags)) = parse_regex_literal(key) {
        return regex_match(pattern, flags, haystack);
    }
    let (haystack, key) = if case_sensitive {
        (haystack.to_owned(), key.to_owned())
    } else {
        (haystack.to_lowercase(), key.to_lowercase())
    };
    if !whole_words || key.chars().any(char::is_whitespace) {
        return Ok(haystack.contains(&key));
    }
    Ok(haystack.match_indices(&key).any(|(start, matched)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + matched.len()..].chars().next();
        before.is_none_or(|value| !value.is_alphanumeric())
            && after.is_none_or(|value| !value.is_alphanumeric())
    }))
}

fn parse_regex_literal(value: &str) -> Option<(&str, &str)> {
    let value = value.strip_prefix('/')?;
    let mut escaped = false;
    for (index, character) in value.char_indices().rev() {
        if character == '/' && !escaped {
            return Some((&value[..index], &value[index + 1..]));
        }
        escaped = character == '\\' && !escaped;
    }
    None
}

fn string(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn bool_value(value: Option<&Value>, default: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(default)
}

fn usize_value(value: Option<&Value>, default: usize) -> usize {
    optional_usize(value).unwrap_or(default)
}

fn optional_usize(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn value_id(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

struct LoreRng(u64);

impl LoreRng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f64 {
        self.next() as f64 / u64::MAX as f64
    }
}

#[derive(Debug, Error)]
pub enum LoreError {
    #[error("lore artifact is missing entries")]
    MissingEntries,
    #[error("lore entries must be an array or object")]
    InvalidEntries,
    #[error("lore entry '{0}' must be an object")]
    InvalidEntry(String),
    #[error("ECMAScript lore regex failed: {0}")]
    Regex(#[from] EcmaRegexError),
    #[error("ECMAScript lore regex failed: {0}")]
    RegexPattern(String),
    #[error("lore macro evaluation failed: {0}")]
    Macro(#[from] MacroError),
    #[error("lorebook source exceeds {limit} entry limit ({count} entries)")]
    TooManyEntries { count: usize, limit: usize },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn revision() -> ContentHash {
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .parse()
            .unwrap()
    }

    #[test]
    fn parses_character_book_and_recursively_activates_entries() {
        let entries = parse_lore_entries(
            &revision(),
            &json!({"entries": [
                {"id": 1, "keys": ["library"], "content": "A sealed archive", "enabled": true, "insertion_order": 100},
                {"id": 2, "keys": ["archive"], "content": "The archivist is Mira", "enabled": true, "insertion_order": 90}
            ]}),
            0,
        )
        .unwrap();
        let engine = LoreEngine::with_worker(
            TokenizerId::Cl100kBase,
            EcmaRegexWorker::new("unused", Duration::from_secs(1)),
        );
        let result = engine
            .evaluate_in_process(
                &entries,
                &["Open the library".to_owned()],
                &LoreSettings::default(),
            )
            .unwrap();
        assert_eq!(result.activated.len(), 2);
        assert_eq!(result.activated[0].entry_key, format!("{}.1", revision()));
        assert_eq!(result.activated[1].entry_key, format!("{}.2", revision()));
    }

    #[test]
    fn selective_regex_groups_probability_and_budget_are_traced() {
        let entries = parse_lore_entries(
            &revision(),
            &json!({"entries": {
                "regex": {"key": ["/lib(?:rary)?/i"], "keysecondary": ["door"], "selective": true, "selectiveLogic": 3, "content": "first", "constant": false, "order": 100, "group": "place", "groupWeight": 1},
                "constant": {"key": [], "content": "second", "constant": true, "order": 90, "group": "place", "groupWeight": 0},
                "large": {"key": [], "content": "a very long lore entry that exceeds the tiny budget", "constant": true, "order": 80}
            }}),
            0,
        )
        .unwrap();
        let settings = LoreSettings {
            budget_tokens: 8,
            rng_seed: 7,
            ..LoreSettings::default()
        };
        let engine = LoreEngine::with_worker(
            TokenizerId::Cl100kBase,
            EcmaRegexWorker::new("unused", Duration::from_secs(1)),
        );
        let result = engine
            .evaluate_in_process(&entries, &["LIBRARY door".to_owned()], &settings)
            .unwrap();
        assert_eq!(result.activated.len(), 1);
        assert!(
            result
                .decisions
                .iter()
                .any(|decision| decision.outcome == LoreDecisionOutcome::GroupRejected)
        );
        assert!(
            result
                .decisions
                .iter()
                .any(|decision| decision.outcome == LoreDecisionOutcome::BudgetRejected)
        );
    }

    #[test]
    fn oversized_lorebook_is_rejected() {
        let limit = crate::limits::MAX_LORE_ENTRIES_PER_SOURCE;
        let entries: Vec<Value> = (0..limit + 1)
            .map(|i| {
                json!({"id": i, "keys": ["k"], "content": "c", "enabled": true, "insertion_order": 100})
            })
            .collect();
        let error = parse_lore_entries(&revision(), &json!({"entries": entries}), 0).unwrap_err();
        assert!(
            error.to_string().contains("entry limit"),
            "expected TooManyEntries, got: {error}"
        );
    }
}

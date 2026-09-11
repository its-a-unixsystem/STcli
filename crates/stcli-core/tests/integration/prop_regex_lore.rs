//! Property tests for regex worker isolation and lore selection invariants.

use std::sync::LazyLock;

use proptest::prelude::*;
use serde_json::Value;
use stcli_core::{
    ContentHash, LoreEngine, LoreEntry, LorePosition, LoreSettings, SelectiveLogic, TokenizerId,
};

fn text(len: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..=len)
        .prop_map(|chars| chars.into_iter().collect::<String>())
}

fn flags() -> impl Strategy<Value = String> {
    const FLAG_CHARS: &[char] = &['g', 'i', 'm', 's', 'u', 'y'];
    prop::collection::vec(prop::sample::select(FLAG_CHARS), 0..=6)
        .prop_map(|chars| chars.into_iter().collect::<String>())
}

fn leaf_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<u8>().prop_map(|n| Value::Number(n.into())),
        any::<f64>().prop_map(|n| {
            Value::Number(
                serde_json::Number::from_f64(n).unwrap_or_else(|| serde_json::Number::from(0)),
            )
        }),
        prop::string::string_regex("[a-zA-Z0-9 ]{0,32}")
            .unwrap()
            .prop_map(Value::String),
    ]
}

fn script_json() -> impl Strategy<Value = Value> {
    (
        prop::string::string_regex("[a-zA-Z0-9 ]{1,32}").unwrap(),
        prop::collection::vec(
            prop::sample::select(&[1u64, 2u64, 3u64, 5u64, 6u64][..]),
            0..=3,
        ),
        any::<bool>(),
    )
        .prop_map(|(find, placements, disabled)| {
            serde_json::json!({
                "findRegex": find,
                "placement": placements,
                "replaceString": "",
                "disabled": disabled,
            })
        })
}

fn composite_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop::collection::vec(leaf_value(), 0..=4).prop_map(Value::Array),
        prop::collection::vec(
            (
                prop::string::string_regex("[a-zA-Z]{1,8}").unwrap(),
                leaf_value(),
            ),
            0..=4,
        )
        .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
    ]
}

fn json_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => leaf_value(),
        2 => script_json(),
        1 => composite_value(),
    ]
}

fn content_hash() -> impl Strategy<Value = ContentHash> {
    any::<[u8; 32]>().prop_map(ContentHash::new)
}

prop_compose! {
    fn lore_entry()
        (
            source_revision in content_hash(),
            id in prop::string::string_regex("[a-zA-Z0-9]{1,16}").unwrap(),
            keys in prop::collection::vec(prop::string::string_regex("[a-zA-Z0-9]{1,12}").unwrap(), 0..=3),
            secondary_keys in prop::collection::vec(prop::string::string_regex("[a-zA-Z0-9]{1,12}").unwrap(), 0..=3),
            content in text(128),
            enabled in any::<bool>(),
            constant in any::<bool>(),
            selective in any::<bool>(),
            selective_logic in prop::sample::select(&[
                SelectiveLogic::AndAny,
                SelectiveLogic::NotAll,
                SelectiveLogic::NotAny,
                SelectiveLogic::AndAll,
            ][..]),
            insertion_order in any::<i64>(),
            position in prop::sample::select(&[
                LorePosition::Before,
                LorePosition::After,
                LorePosition::AuthorNoteTop,
                LorePosition::AuthorNoteBottom,
                LorePosition::AtDepth,
                LorePosition::ExampleTop,
                LorePosition::ExampleBottom,
                LorePosition::Outlet,
            ][..]),
            depth in any::<u8>().prop_map(|v| (v % 11) as usize),
            role in any::<i64>(),
            outlet in prop::sample::select(&["", "lore", "note"][..]).prop_map(|s| s.to_string()),
            exclude_recursion in any::<bool>(),
            prevent_recursion in any::<bool>(),
            delay_until_recursion in any::<u8>().prop_map(|v| (v % 11) as usize),
            scan_depth in prop::option::of(any::<u8>().prop_map(|v| (v % 11) as usize)),
            case_sensitive in prop::option::of(any::<bool>()),
            match_whole_words in prop::option::of(any::<bool>()),
            probability in any::<u8>().prop_map(|v| v as f64),
            use_probability in any::<bool>(),
            group in prop::collection::vec(prop::string::string_regex("[a-z]{1,8}").unwrap(), 0..=2),
            group_override in any::<bool>(),
            group_weight in any::<u16>().prop_map(|v| (v as u64) % 1001),
            sticky in any::<u8>().prop_map(|v| (v % 11) as usize),
            cooldown in any::<u8>().prop_map(|v| (v % 11) as usize),
            delay in any::<u8>().prop_map(|v| (v % 11) as usize),
            triggers in prop::collection::vec(
                prop::sample::select(&["normal"][..]).prop_map(|s| s.to_string()),
                0..=1,
            ),
        )
    -> LoreEntry {
        LoreEntry {
            source_revision,
            source_index: 0,
            id,
            keys,
            secondary_keys,
            content,
            enabled,
            constant,
            selective,
            selective_logic,
            insertion_order,
            position,
            depth,
            role,
            outlet,
            exclude_recursion,
            prevent_recursion,
            delay_until_recursion,
            scan_depth,
            case_sensitive,
            match_whole_words,
            probability,
            use_probability,
            group,
            group_override,
            group_weight,
            sticky,
            cooldown,
            delay,
            ignore_budget: false,
            triggers,
        }
    }
}

fn entries() -> impl Strategy<Value = Vec<LoreEntry>> {
    prop::collection::vec(lore_entry(), 0..=20).prop_map(|mut v| {
        for (i, e) in v.iter_mut().enumerate() {
            e.source_index = i;
        }
        v
    })
}

fn settings() -> impl Strategy<Value = LoreSettings> {
    any::<u16>().prop_map(|v| LoreSettings {
        budget_tokens: (v as usize) % 1024,
        rng_seed: 1,
        ..Default::default()
    })
}

fn messages() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(text(128), 0..=5)
}

static ENGINE: LazyLock<LoreEngine> =
    LazyLock::new(|| LoreEngine::new(TokenizerId::O200kBase).unwrap());

proptest! {
    #[test]
    fn arbitrary_regex_worker_input_never_panics(
        (pattern, text, flags, value) in (text(256), text(256), flags(), json_value())
    ) {
        let response = stcli_core::run_worker(stcli_core::RegexRequest { pattern, flags, text });
        match response {
            stcli_core::RegexResponse::Match { .. } => {}
            stcli_core::RegexResponse::Error { .. } => {}
        }

        let script = stcli_core::RegexScript::from_value(&value);
        if let Some(script) = script {
            prop_assert!(serde_json::to_value(script).is_ok(), "script did not serialize");
        }
    }

    #[test]
    fn arbitrary_lore_entries_respect_budget_and_stable(
        (entries, settings, messages) in (entries(), settings(), messages())
    ) {
        let engine = &*ENGINE;
        let result1 = engine
            .evaluate_in_process(&entries, &messages, &settings)
            .expect("lore evaluation should succeed");
        let result2 = engine
            .evaluate_in_process(&entries, &messages, &settings)
            .expect("lore evaluation should succeed");

        prop_assert!(result1.used_tokens <= settings.budget_tokens);
        prop_assert!(result1.activated.len() <= entries.len());
        prop_assert_eq!(result1.activated, result2.activated);
        prop_assert_eq!(result1.used_tokens, result2.used_tokens);
    }
}

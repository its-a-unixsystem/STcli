use std::collections::BTreeMap;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use serde_json::json;
use stcli_core::{
    ChatRole, ContentHash, EntityId, LoreEngine, LoreSettings, MacroContext, MacroEngine,
    PromptPreset, PromptSegment, RenderedPromptContent, SessionConfiguration, StateTransaction,
    Store, TokenizerId, apply_prompt_preset, decode_artifact, lore::parse_lore_entries,
    prune_segments,
};
use tempfile::tempdir;

fn tokenizer() -> TokenizerId {
    TokenizerId::O200kBase
}

fn example_character() -> &'static [u8] {
    include_bytes!("../../../examples/character.json")
}

fn example_lorebook() -> &'static [u8] {
    include_bytes!("../../../examples/lorebook.json")
}

fn example_preset() -> &'static [u8] {
    include_bytes!("../../../examples/preset.json")
}

fn scaled_lorebook(entry_count: usize) -> String {
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        entries.push(json!({
            "id": i,
            "keys": [format!("keyword_{i}"), format!("trigger_{i}")],
            "secondary_keys": [],
            "comment": format!("Entry {i}"),
            "content": format!("Lore entry {i}: The artifact of power number {i} was forged in the ancient citadel."),
            "constant": false,
            "selective": false,
            "order": 100 - (i % 50) as i64,
            "position": 1,
            "disable": false,
            "extensions": {
                "position": 1,
                "depth": 4,
                "exclude_recursion": false,
                "prevent_recursion": false,
                "delay_until_recursion": 0,
                "probability": 100,
                "use_probability": true,
                "selective_logic": 0,
                "group": "",
                "group_override": false,
                "group_weight": 100,
            }
        }));
    }
    serde_json::to_string(&json!({
        "name": "Scaled Lorebook",
        "description": "Benchmark lorebook",
        "scan_depth": 2,
        "token_budget": 2048,
        "recursive_scanning": true,
        "extensions": {},
        "entries": entries,
    }))
    .unwrap()
}

fn chat_history(turn_count: usize) -> Vec<String> {
    (0..turn_count)
        .map(|i| {
            if i % 2 == 0 {
                format!("Tell me about keyword_{} and the ancient citadel of the realm.", i / 2)
            } else {
                format!(
                    "The ancient citadel stands atop the obsidian cliffs. Its brass spires reach \
                     toward the crimson sky, and within its halls the artifact of power number {} \
                     hums with residual aether energy. The archivists have cataloged its properties \
                     in volume {} of the Grand Codex.",
                    i / 2,
                    i
                )
            }
        })
        .collect()
}

fn bench_artifact_decode(c: &mut Criterion) {
    let character = example_character();
    let lorebook = example_lorebook();
    let preset = example_preset();

    c.bench_function("decode_character_card", |b| {
        b.iter(|| decode_artifact(black_box(character)).unwrap())
    });
    c.bench_function("decode_lorebook", |b| {
        b.iter(|| decode_artifact(black_box(lorebook)).unwrap())
    });
    c.bench_function("decode_preset", |b| {
        b.iter(|| decode_artifact(black_box(preset)).unwrap())
    });
}

fn bench_lore_evaluation(c: &mut Criterion) {
    let lorebook_json: serde_json::Value = serde_json::from_slice(example_lorebook()).unwrap();
    let dummy_hash = ContentHash::new([0u8; 32]);
    let entries = parse_lore_entries(&dummy_hash, &lorebook_json, 0).unwrap();
    let messages = chat_history(10);
    let messages_rev: Vec<String> = messages.into_iter().rev().collect();
    let settings = LoreSettings::default();
    let engine = LoreEngine::new(tokenizer()).unwrap();

    c.bench_function("lore_eval_4_entries_10_messages", |b| {
        b.iter(|| {
            engine
                .evaluate_in_process(
                    black_box(&entries),
                    black_box(&messages_rev),
                    black_box(&settings),
                )
                .unwrap()
        })
    });

    let scaled = scaled_lorebook(200);
    let scaled_json: serde_json::Value = serde_json::from_str(&scaled).unwrap();
    let scaled_entries = parse_lore_entries(&dummy_hash, &scaled_json, 0).unwrap();
    let long_history = chat_history(40);
    let long_rev: Vec<String> = long_history.into_iter().rev().collect();
    let scaled_settings = LoreSettings {
        budget_tokens: 2048,
        ..Default::default()
    };

    c.bench_function("lore_eval_200_entries_40_messages", |b| {
        b.iter(|| {
            engine
                .evaluate_in_process(
                    black_box(&scaled_entries),
                    black_box(&long_rev),
                    black_box(&scaled_settings),
                )
                .unwrap()
        })
    });
}

fn bench_macro_rendering(c: &mut Criterion) {
    let mut context = MacroContext::default();
    context.insert("char", "Elspeth");
    context.insert("user", "Thomas");
    context.insert("mesExamples", "<START>\nThomas: Hello\nElspeth: Welcome.");

    let simple_input = "{{char}} greets {{user}} in the {{scenario}}.";
    let complex_input = "{{char}} says: {{setvar::mood::curious}}The mood is \
                         {{getvar::mood}}. {{if::{{getvar::mood}}}}Active{{/if}} \
                         {{char}} and {{user}} continue.";

    c.bench_function("macro_simple", |b| {
        b.iter(|| {
            let mut engine = MacroEngine::new(42);
            let mut state = StateTransaction::empty(EntityId::new());
            engine
                .render(black_box(simple_input), black_box(&context), &mut state)
                .unwrap()
        })
    });

    c.bench_function("macro_complex_with_state", |b| {
        b.iter(|| {
            let mut engine = MacroEngine::new(42);
            let mut state = StateTransaction::empty(EntityId::new());
            engine
                .render(black_box(complex_input), black_box(&context), &mut state)
                .unwrap()
        })
    });
}

fn bench_prompt_assembly(c: &mut Criterion) {
    let tok = tokenizer();
    let preset_json: serde_json::Value = serde_json::from_slice(example_preset()).unwrap();
    let preset =
        PromptPreset::parse(&preset_json, stcli_core::CHAT_COMPLETION_CHARACTER_ID).unwrap();

    let build_segments = |turn_count: usize| -> Vec<PromptSegment> {
        let mut segments = vec![
            PromptSegment::new(
                tok,
                "character:description",
                "charDescription",
                ChatRole::System,
                "Elspeth is the Master Archivist of the Grand Archive of Oakhaven.".to_owned(),
            ),
            PromptSegment::new(
                tok,
                "character:personality",
                "charPersonality",
                ChatRole::System,
                "Intellectual, observant, meticulous.".to_owned(),
            ),
            PromptSegment::new(
                tok,
                "character:scenario",
                "scenario",
                ChatRole::System,
                "The Grand Archive of Oakhaven.".to_owned(),
            ),
        ];
        for i in 0..turn_count {
            let role = if i % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            };
            let content = if i % 2 == 0 {
                format!("Turn {i}: Tell me about the ancient records.")
            } else {
                format!(
                    "Turn {i}: *Elspeth adjusts her spectacles and pulls a heavy tome from the \
                     shelf.* \"The records you seek are in sub-level three. Follow me.\""
                )
            };
            let mut seg =
                PromptSegment::new(tok, format!("turn:{i}:user"), "chatHistory", role, content);
            seg.truncation_priority = 100 + i as u32;
            segments.push(seg);
        }
        segments.push(PromptSegment::new(
            tok,
            "user-input",
            "userInput",
            ChatRole::User,
            "What can you tell me about the Aether Engines?".to_owned(),
        ));
        segments
    };

    c.bench_function("prompt_assemble_10_turns", |b| {
        b.iter(|| {
            let segments = build_segments(10);
            apply_prompt_preset(tok, Some(&preset), segments, |_, content| {
                Ok(RenderedPromptContent::plain(content.to_owned()))
            })
            .unwrap()
        })
    });

    c.bench_function("prompt_assemble_100_turns", |b| {
        b.iter(|| {
            let segments = build_segments(100);
            apply_prompt_preset(tok, Some(&preset), segments, |_, content| {
                Ok(RenderedPromptContent::plain(content.to_owned()))
            })
            .unwrap()
        })
    });

    c.bench_function("prompt_prune_100_turns", |b| {
        b.iter_batched(
            || {
                let segments = build_segments(100);
                apply_prompt_preset(tok, Some(&preset), segments, |_, content| {
                    Ok(RenderedPromptContent::plain(content.to_owned()))
                })
                .unwrap()
            },
            |mut segments| {
                prune_segments(black_box(&mut segments), 4096, 512).unwrap();
                segments
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_dry_run(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let mut store = Store::open(dir.path().join("bench.sqlite3")).unwrap();
    let character = store.import_artifact(example_character()).unwrap();
    let lorebook = store.import_artifact(example_lorebook()).unwrap();
    let preset = store.import_artifact(example_preset()).unwrap();
    let config = configuration_for_bench(
        character.revision_hash.clone(),
        Some(lorebook.revision_hash.clone()),
        Some(preset.revision_hash.clone()),
    );
    let created = store.create_session(config, 0).unwrap();
    let session_id = created.session.session_id;
    let branch_id = created.branch.branch_id;

    c.bench_function("dry_run_empty_session", |b| {
        b.iter(|| {
            store
                .dry_run_message(
                    black_box(session_id),
                    black_box(branch_id),
                    black_box("Tell me about the archive."),
                )
                .unwrap()
        })
    });
}

fn configuration_for_bench(
    character_revision: stcli_core::ContentHash,
    lorebook_revision: Option<stcli_core::ContentHash>,
    preset_revision: Option<stcli_core::ContentHash>,
) -> SessionConfiguration {
    SessionConfiguration {
        compatibility_profile: "sillytavern-1.18-core".to_owned(),
        character_revision,
        persona_name: "User".to_owned(),
        lorebook_revisions: lorebook_revision.into_iter().collect(),
        prompt_preset_revision: preset_revision,
        provider: stcli_core::ProviderSettings {
            id: "bench-provider".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 1,
            ca_certificate_pem: None,
            model: "fixture-model".to_owned(),
            stream: false,
        },
        tokenizer: "tiktoken:o200k_base".to_owned(),
        generation_settings: json!({}),
        plugins: vec![],
        script_grants: vec![],
    }
}

criterion_group!(
    benches,
    bench_artifact_decode,
    bench_lore_evaluation,
    bench_macro_rendering,
    bench_prompt_assembly,
    bench_dry_run,
);
criterion_main!(benches);

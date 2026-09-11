use serde_json::json;
use stcli_core::{
    ContextFormatting, EntityId, FormatMode, InstructTemplate, OpenAiProvider, ProviderError,
    SessionError, Store, provider_request,
};
use stcli_testkit::{MockProvider, configuration, fixtures};
use tempfile::tempdir;

fn context_formatting() -> ContextFormatting {
    serde_json::from_value(json!({
        "story_string": "{{#if anchorBefore}}{{anchorBefore}}\n{{/if}}{{#if system}}{{system}}\n{{/if}}{{#if wiBefore}}{{wiBefore}}\n{{/if}}{{#if description}}{{description}}\n{{/if}}{{#if personality}}{{personality}}\n{{/if}}{{#if scenario}}{{scenario}}\n{{/if}}{{#if wiAfter}}{{wiAfter}}\n{{/if}}{{#if persona}}{{persona}}\n{{/if}}{{#if anchorAfter}}{{anchorAfter}}\n{{/if}}{{trim}}",
        "example_separator": "",
        "chat_start": "",
        "use_stop_strings": false,
        "names_as_stop_strings": true,
        "turn_separator": ""
    }))
    .unwrap()
}

fn chatml_template() -> InstructTemplate {
    serde_json::from_value(json!({
        "input_sequence": "<|im_start|>user",
        "output_sequence": "<|im_start|>assistant",
        "last_output_sequence": "",
        "system_sequence": "<|im_start|>system",
        "stop_sequence": "<|im_end|>",
        "wrap": true,
        "macro": true,
        "names_behavior": "force",
        "first_output_sequence": "",
        "skip_examples": false,
        "output_suffix": "<|im_end|>\n",
        "input_suffix": "<|im_end|>\n",
        "system_suffix": "<|im_end|>\n",
        "system_same_as_user": false,
        "last_system_sequence": "",
        "first_input_sequence": "",
        "last_input_sequence": "",
        "sequences_as_stop_strings": true,
        "story_string_prefix": "<|im_start|>system",
        "story_string_suffix": "<|im_end|>\n"
    }))
    .unwrap()
}

fn alpaca_template() -> InstructTemplate {
    serde_json::from_value(json!({
        "input_sequence": "### Instruction:",
        "output_sequence": "### Response:",
        "system_sequence": "### Input:",
        "stop_sequence": "",
        "wrap": true,
        "macro": true,
        "names_behavior": "force",
        "skip_examples": false,
        "output_suffix": "\n\n",
        "input_suffix": "\n\n",
        "system_suffix": "\n\n",
        "system_same_as_user": false,
        "sequences_as_stop_strings": true,
        "story_string_prefix": "",
        "story_string_suffix": "\n\n"
    }))
    .unwrap()
}

fn llama_3_template() -> InstructTemplate {
    serde_json::from_value(json!({
        "input_sequence": "<|start_header_id|>user<|end_header_id|>\n\n",
        "output_sequence": "<|start_header_id|>assistant<|end_header_id|>\n\n",
        "system_sequence": "<|start_header_id|>system<|end_header_id|>\n\n",
        "stop_sequence": "<|eot_id|>",
        "wrap": false,
        "macro": true,
        "names_behavior": "force",
        "skip_examples": false,
        "output_suffix": "<|eot_id|>",
        "input_suffix": "<|eot_id|>",
        "system_suffix": "<|eot_id|>",
        "system_same_as_user": false,
        "sequences_as_stop_strings": true,
        "story_string_prefix": "<|start_header_id|>system<|end_header_id|>\n\n",
        "story_string_suffix": "<|eot_id|>"
    }))
    .unwrap()
}

fn dry_run_with_settings(
    template: InstructTemplate,
    generation_settings: serde_json::Value,
) -> stcli_core::DryRunResult {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(template);
    config.provider.context_formatting = Some(context_formatting());
    config.generation_settings = generation_settings;
    let created = store.create_session(config, 0).unwrap();

    store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap()
}

fn dry_run_with_template(template: InstructTemplate) -> stcli_core::DryRunResult {
    dry_run_with_settings(template, json!({}))
}

async fn create_failed_turn(
    store: &mut Store,
    session_id: EntityId,
    branch_id: EntityId,
    content: &str,
) -> stcli_core::TurnProjection {
    store
        .send_message(session_id, branch_id, content.to_owned(), |_| {})
        .await
        .unwrap_err();
    store.turns_for_branch(branch_id).unwrap().pop().unwrap()
}

fn complete_with_candidate(store: &mut Store, turn: &stcli_core::TurnProjection, content: &str) {
    let attempt = store
        .attempts_for_turn(turn.turn_id)
        .unwrap()
        .pop()
        .unwrap();
    store
        .record_event(
            Some(turn.session_id),
            "attempt.completed",
            &json!({
                "attempt_id": attempt.attempt_id,
                "turn_id": turn.turn_id,
                "candidate_id": EntityId::new(),
                "origin": "generated",
                "content": content,
                "provider_request_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "provider_receipt": {},
            }),
        )
        .unwrap();
    store.rebuild_session_projections().unwrap();
}

// Parity fixtures: SillyTavern 1.18.0 commit 51ad27fb86d39a3daca3adaa970375c9670c12df,
// default/content/presets/instruct and context presets with the matching names.
#[test]
fn dry_run_renders_chatml_text_completion_prompt() {
    let dry_run = dry_run_with_template(chatml_template());

    assert_eq!(dry_run.prompt_plan.format_mode, FormatMode::TextCompletion);
    assert_eq!(
        dry_run.prompt_plan.text_prompt.as_deref(),
        Some(
            "<|im_start|>system\nWrite Alice's next reply in a fictional chat between Alice and User.\nA librarian.\nCurious\nAn old library\nUser\n<|im_end|>\n<|im_start|>assistant\nWelcome.<|im_end|>\n<|im_start|>user\nHello<|im_end|>\n\n<|im_start|>assistant\n"
        )
    );
    assert_eq!(
        dry_run.prompt_plan.stop_sequences,
        [
            "<|im_end|>",
            "\n<|im_start|>user",
            "\n<|im_start|>assistant",
            "\n<|im_start|>system",
            "User:",
            "Alice:",
        ]
    );
}

#[test]
fn dry_run_renders_alpaca_text_completion_prompt() {
    let dry_run = dry_run_with_template(alpaca_template());

    assert_eq!(
        dry_run.prompt_plan.text_prompt.as_deref(),
        Some(
            "Write Alice's next reply in a fictional chat between Alice and User.\nA librarian.\nCurious\nAn old library\nUser\n\n\n### Response:\nWelcome.\n\n### Instruction:\nHello\n\n\n### Response:\n"
        )
    );
}

#[test]
fn dry_run_renders_llama_3_text_completion_prompt() {
    let dry_run = dry_run_with_template(llama_3_template());

    assert_eq!(
        dry_run.prompt_plan.text_prompt.as_deref(),
        Some(
            "<|start_header_id|>system<|end_header_id|>\n\nWrite Alice's next reply in a fictional chat between Alice and User.\nA librarian.\nCurious\nAn old library\nUser\n<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\nWelcome.<|eot_id|><|start_header_id|>user<|end_header_id|>\n\nHello<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n"
        )
    );
}

#[test]
fn dry_run_appends_assistant_prefill_after_the_final_cue() {
    let dry_run =
        dry_run_with_settings(chatml_template(), json!({"assistant_prefill": "Prefilled"}));

    assert!(
        dry_run
            .prompt_plan
            .text_prompt
            .as_deref()
            .unwrap()
            .ends_with("<|im_start|>assistant\nPrefilled")
    );
}

#[test]
fn flat_projection_preserves_transformed_story_segment_attribution() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(
            json!({
                "spec": "chara_card_v2",
                "spec_version": "2.0",
                "data": {
                    "name": "Alice",
                    "description": "A {{char}} librarian.",
                    "personality": "Curious",
                    "scenario": "An old library",
                    "first_mes": "Welcome.",
                    "mes_example": "",
                    "alternate_greetings": [],
                    "extensions": {}
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let lorebook = store
        .import_artifact(
            json!({
                "entries": {
                    "archive": {
                        "key": [],
                        "content": "The archive belongs to {{char}}.",
                        "constant": true,
                        "order": 100
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.lorebook_revisions.push(lorebook.revision_hash);
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(chatml_template());
    config.provider.context_formatting = Some(context_formatting());
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();

    let description = dry_run
        .prompt_plan
        .segments
        .iter()
        .find(|segment| segment.source == "character-description")
        .unwrap();
    assert_eq!(description.raw_content, "A {{char}} librarian.");
    assert_eq!(description.content, "A Alice librarian.");
    assert_eq!(description.source_field.as_deref(), Some("description"));
    assert!(!description.macro_evaluations.is_empty());
    let lore = dry_run
        .prompt_plan
        .segments
        .iter()
        .find(|segment| segment.source == "world-info-after")
        .unwrap();
    assert!(lore.content.contains("The archive belongs to Alice."));
    assert!(
        dry_run
            .prompt_plan
            .text_prompt
            .as_deref()
            .unwrap()
            .contains("The archive belongs to Alice.")
    );
}

#[test]
fn session_rejects_incomplete_text_completion_settings() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.provider.format_mode = FormatMode::TextCompletion;

    assert!(matches!(
        store.create_session(config, 0),
        Err(SessionError::Provider(
            ProviderError::MissingCompletionsPath
        ))
    ));
}

#[test]
fn separators_overrides_names_and_role_macros_follow_instruct_boundaries() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(
            json!({
                "spec": "chara_card_v2",
                "spec_version": "2.0",
                "data": {
                    "name": "Alice",
                    "description": "",
                    "personality": "",
                    "scenario": "",
                    "first_mes": "Welcome.",
                    "mes_example": "<START>\nUser: Demo\nAlice: Sample",
                    "alternate_greetings": [],
                    "extensions": {}
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(InstructTemplate {
        input_sequence: "<{{name}}>".to_owned(),
        output_sequence: "<assistant>".to_owned(),
        first_input_sequence: "<first-user>".to_owned(),
        last_input_sequence: "<last-{{name}}>".to_owned(),
        first_output_sequence: "<first-{{name}}>".to_owned(),
        last_output_sequence: "<last-assistant>".to_owned(),
        system_sequence: "<{{name}}>".to_owned(),
        input_suffix: "|".to_owned(),
        output_suffix: "|".to_owned(),
        names_behavior: stcli_core::NamesBehavior::Always,
        r#macro: true,
        sequences_as_stop_strings: true,
        ..InstructTemplate::default()
    });
    config.provider.context_formatting = Some(ContextFormatting {
        story_string: "{{system}}".to_owned(),
        example_separator: "EXAMPLES".to_owned(),
        chat_start: "CHAT".to_owned(),
        ..ContextFormatting::default()
    });
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    let prompt = dry_run.prompt_plan.text_prompt.as_deref().unwrap();

    assert!(prompt.contains("EXAMPLES\n<User>User: Demo|<assistant>Alice: Sample|CHAT\n"));
    assert!(prompt.contains("<first-Alice>Alice: Welcome.|<last-User>User: Hello|"));
    assert!(prompt.ends_with("<last-assistant>Alice:"));
    assert!(
        dry_run
            .prompt_plan
            .stop_sequences
            .contains(&"<User>".to_owned())
    );
    assert!(
        dry_run
            .prompt_plan
            .stop_sequences
            .contains(&"<System>".to_owned())
    );
}

#[tokio::test]
async fn completion_transport_uses_flat_payload_stops_and_text_response() {
    let mock = MockProvider::spawn(["Completion candidate"]).await.unwrap();
    let mut settings = mock.provider_settings();
    settings.format_mode = FormatMode::TextCompletion;
    settings.completions_path = Some("/v1/completions".to_owned());
    settings.instruct_template = Some(chatml_template());
    settings.context_formatting = Some(context_formatting());
    let prompt_plan = dry_run_with_template(chatml_template()).prompt_plan;
    let request =
        provider_request(&settings, &prompt_plan, &json!({"stop": "configured-stop"})).unwrap();
    let provider = OpenAiProvider::new(settings).unwrap();

    let result = provider.generate_request(&request, |_| {}).await.unwrap();

    assert_eq!(result.text, "Completion candidate");
    mock.shutdown().await;
}

#[tokio::test]
async fn rendered_budget_prunes_both_sides_of_the_oldest_turn() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(chatml_template());
    config.provider.context_formatting = Some(context_formatting());
    let created = store.create_session(config.clone(), 0).unwrap();

    let oldest = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "old-user ".repeat(80).trim(),
    )
    .await;
    complete_with_candidate(&mut store, &oldest, "old-assistant ".repeat(80).trim());
    let newer = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "new-user",
    )
    .await;
    complete_with_candidate(&mut store, &newer, "new-assistant");

    let unpruned = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "current",
        )
        .unwrap();
    let content_tokens = unpruned
        .prompt_plan
        .segments
        .iter()
        .map(|segment| segment.token_count)
        .sum::<usize>();
    assert!(unpruned.prompt_plan.total_tokens > content_tokens);

    config.generation_settings = json!({
        "max_context": content_tokens + 512,
        "max_tokens": 512
    });
    store
        .update_session_configuration(created.session.session_id, config)
        .unwrap();
    let pruned = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "current",
        )
        .unwrap();

    let oldest_prefix = format!("turn:{}:", oldest.turn_id);
    let newer_prefix = format!("turn:{}:", newer.turn_id);
    assert!(
        pruned
            .prompt_plan
            .segments
            .iter()
            .filter(|segment| segment.source.starts_with(&oldest_prefix))
            .all(|segment| segment.pruned)
    );
    assert!(
        pruned
            .prompt_plan
            .segments
            .iter()
            .filter(|segment| segment.source.starts_with(&newer_prefix))
            .all(|segment| !segment.pruned)
    );
    assert_eq!(
        pruned.prompt_plan.pruning.kept_tokens,
        pruned
            .prompt_plan
            .tokenizer
            .count(pruned.prompt_plan.text_prompt.as_deref().unwrap())
    );
}

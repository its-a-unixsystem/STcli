use std::time::Duration;

use stcli_core::{
    EngineCommand, EngineResult, EntityId, StcliEngine, Store, StscriptCommand, StscriptError,
    StscriptLimits, StscriptProgram, StscriptResult, VariableScope, parse_stscript,
};
use tempfile::tempdir;

#[test]
fn parser_handles_quotes_pipes_and_nested_closures() {
    let program = parse_stscript(
        r#"/echo title="a | b" hello world | /if left=gold right=10 rule=gt else={: /echo poor :} {: /setvar key=gold value=9 | /echo paid :}"#,
    )
    .unwrap();

    assert_eq!(program.commands.len(), 2);
    assert_eq!(program.commands[0].name, "echo");
    assert_eq!(program.commands[0].named["title"], "a | b");
    assert_eq!(program.commands[0].unnamed, "hello world");
    let StscriptCommand {
        closure: Some(closure),
        else_closure: Some(otherwise),
        ..
    } = &program.commands[1]
    else {
        panic!("expected both conditional closures");
    };
    assert_eq!(closure.commands.len(), 2);
    assert_eq!(otherwise.commands[0].unnamed, "poor");
}

#[test]
fn evaluator_executes_conditionals_math_scopes_and_pipes_atomically() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let session_id = EntityId::new();
    let execution_id = EntityId::new();
    let mut store = Store::open(database).unwrap();
    let script = r#"
        /setglobalvar key=campaign moon |
        /setvar key=gold 12 |
        /if left={{getvar::gold}} right=10 rule=gt {: /setvar key=gold value={{eval::{{getvar::gold}}-10}} | /echo paid :} |
        /let key=receipt value={{pipe}} |
        /echo {{getvar::gold}}/{{getglobalvar::campaign}}/{{getlocal::receipt}}
    "#;

    let result = store
        .execute_stscript(session_id, execution_id, script, StscriptLimits::default())
        .unwrap();

    assert_eq!(
        result,
        StscriptResult::Completed {
            output: "2/moon/paid".to_owned()
        }
    );
    let state = store.state_transaction(session_id).unwrap();
    assert_eq!(
        state.get(VariableScope::Local, "gold").unwrap().raw_value,
        "2"
    );
    assert_eq!(
        state
            .get(VariableScope::Global, "campaign")
            .unwrap()
            .raw_value,
        "moon"
    );
    assert!(state.get(VariableScope::Local, "receipt").is_none());
    let events = store.trace_events(Some(session_id)).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "stscript.executed")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "state.committed")
    );
}

#[tokio::test]
async fn engine_executes_stscript_and_returns_recorded_result() {
    let directory = tempdir().unwrap();
    let engine = StcliEngine::new(directory.path().join("stcli.sqlite3"));
    let result = engine
        .execute(
            EngineCommand::ExecuteStscript {
                session_id: EntityId::new(),
                execution_id: EntityId::new(),
                source: "/echo through-engine".to_owned(),
                limits: StscriptLimits::default(),
            },
            |_| {},
        )
        .await
        .unwrap();

    assert!(matches!(
        result,
        EngineResult::Stscript(StscriptResult::Completed { output })
            if output == "through-engine"
    ));
}

#[test]
fn false_branch_executes_else_and_abort_stops_following_commands() {
    let program = StscriptProgram::parse(
        "/if left=1 right=2 rule=eq else={: /echo denied | /abort :} {: /echo allowed :} | /echo unreachable",
    )
    .unwrap();
    let result = program.evaluate_replay(StscriptLimits::default()).unwrap();

    assert_eq!(result.output, "denied");
    let standalone =
        StscriptProgram::parse("/if left=1 right=1 rule=eq {: /echo yes :} | /else {: /echo no :}")
            .unwrap()
            .evaluate_replay(StscriptLimits::default())
            .unwrap();
    assert_eq!(standalone.output, "yes");
}
#[test]
fn parser_rejects_closures_beyond_execution_depth_limit() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let mut source = "/echo deepest".to_owned();
    for _ in 0..3 {
        source = format!("/if left=1 right=1 rule=eq {{: {source} :}}");
    }

    let error = store
        .execute_stscript(
            EntityId::new(),
            EntityId::new(),
            &source,
            StscriptLimits {
                max_steps: 100,
                max_depth: 2,
                timeout: Duration::from_secs(1),
            },
        )
        .unwrap_err();

    assert!(matches!(error, StscriptError::DepthLimit { limit: 2 }));
}

#[test]
fn while_loop_stops_at_instruction_budget_without_committing_state() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let session_id = EntityId::new();
    let error = store
        .execute_stscript(
            session_id,
            EntityId::new(),
            "/setvar key=count value=0 | /while left=1 right=1 rule=eq {: :}",
            StscriptLimits {
                max_steps: 8,
                max_depth: 4,
                timeout: Duration::from_secs(1),
            },
        )
        .unwrap_err();

    assert!(matches!(error, StscriptError::StepLimit { limit: 8 }));
    assert!(
        store
            .state_transaction(session_id)
            .unwrap()
            .get(VariableScope::Local, "count")
            .is_none()
    );
    assert!(
        store
            .trace_events(Some(session_id))
            .unwrap()
            .iter()
            .any(|event| event.event_type == "stscript.failed")
    );
}

#[test]
fn delay_is_recorded_without_sleeping_during_replay() {
    let program = StscriptProgram::parse("/delay 1000 | /echo ready").unwrap();
    let result = program.evaluate_replay(StscriptLimits::default()).unwrap();
    assert_eq!(result.output, "ready");
    assert_eq!(result.delays, vec![Duration::from_millis(1000)]);
}

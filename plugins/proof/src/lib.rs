wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

struct ProofPlugin;

impl Guest for ProofPlugin {
    fn run(input: String) -> Result<String, String> {
        if input.contains(r#""mode":"failure""#) {
            return Err("requested proof failure".to_owned());
        }
        if input.contains(r#""mode":"spin""#) {
            loop {
                std::hint::black_box(&input);
            }
        }
        if input.contains(r#""mode":"huge-output""#) {
            return Ok(format!(
                r#"{{"effects":[{{"effect":"observe","name":"proof","value":"{}"}}]}}"#,
                "x".repeat(300_000)
            ));
        }
        if input.contains(r#""mode":"wrong-state""#) {
            return Ok(r#"{"effects":[{"effect":"state-write","key":{"scope":"local","name":"org.other.invoked"},"value":true}]}"#.to_owned());
        }
        if input.contains(r#""mode":"abort""#) {
            return Ok(r#"{"effects":[{"effect":"abort","code":"proof-abort","message":"requested proof abort"}]}"#.to_owned());
        }
        if input.contains(r#""command":"proof-set""#) {
            return Ok(r#"{"effects":[{"effect":"state-write","key":{"scope":"local","name":"org.stcli.proof.command-value"},"value":"set by command"}]}"#.to_owned());
        }
        Ok(r#"{"effects":[{"effect":"register-macro","name":"proof-greeting","value":"Hello from Wasm"},{"effect":"register-command","name":"proof-set","description":"Set the proof command state value"},{"effect":"prompt","contribution":{"slot":"after-character-definitions","name":"proof-note","role":"system","content":"The proof Plugin is active.","depth":null,"order":10,"outlet":null}},{"effect":"state-write","key":{"scope":"local","name":"org.stcli.proof.invoked"},"value":true}]}"#.to_owned())
    }
}

export!(ProofPlugin);

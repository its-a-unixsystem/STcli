function prePrompt(input) {
  const settings = input.settings || {};
  const start = settings.start || 0;
  const turn = (stcli.state.get("turns") || start) + 1;
  stcli.state.set("turns", turn);
  stcli.log("info", "turn " + turn);
  stcli.prompt.inject("after-character-definitions", "[Turn " + turn + "]");
}

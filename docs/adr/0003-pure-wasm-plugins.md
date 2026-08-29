# Limit MVP Plugins to pure Wasm effects

MVP Plugins use the WebAssembly Component Model and return declarative effects through closed capabilities. We rejected native dynamic libraries, arbitrary JavaScript, and plugin network/model/filesystem/secret access because they expand the trust boundary, make replay dependent on live side effects, and delay the compatibility engine.

## Consequences

Plugins may observe supported lifecycle events, register macros and commands, contribute prompt segments to closed slots, read permitted Session data, write their own attempt-local namespace, or abort before a provider request. Sessions pin exact component digests. Live-effect capabilities and external artifact codecs require later versioned interfaces and separate threat models.

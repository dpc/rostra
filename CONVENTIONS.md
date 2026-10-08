# Engineering conventions

## Conventions

> When writing code, follow these conventions


- Do NOT add comments explaining what is each line/expression doing.
- Try to keep the code uniform, and follow the style of the existing code.
- Always use standalone Rust modules, avoid inline `mod`s
- Use structured logging, don't do string interpolation in logging statements.

## Web UI

See the [UI design guide](crates/rostra-web-ui/design-ui.md) for the current visual
language, shared patterns, and practical review checks. It complements the
governing UI specs linked from the guide.

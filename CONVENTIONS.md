# Engineering conventions

## Conventions

> When writing code, follow this conventions


- Do NOT add comments explaining what is each line/expression doing.
- Try to keep the code uniform, and follow the style of the existing code.
- Always use standalone Rust modules, avoid inline `mod`s
- Use structured logging, don't do string interpolation in logging statements.

## Project structure

- `crates/` - all the project modules/crates
  - `crates/rostra-core` core domain types used all across the project
  - `crates/rostra-client-db` database tracking all the events
  - `crates/rostra-web-ui` the default web-based UI
  

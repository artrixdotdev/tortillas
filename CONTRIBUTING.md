# Contributing Guidelines

## Code of Conduct
Follow the [Arch Linux Code of Conduct](https://terms.archlinux.org/docs/code-of-conduct/).

## Commit Messages
Follow [FreeCodeCamp's commit message style guide](https://www.freecodecamp.org/news/how-to-write-better-git-commit-messages/).

**All** commit messages and pull requests must follow these guidelines.

## Code Style
- Use **3 spaces** for indentation
- Follow our [editorconfig](.editorconfig) or [rustfmt](rustfmt.toml) settings
- Write **clear, readable code**
- Add **comments and documentation** when needed

## Workflow
All changes (new features, bug fixes, refactors, or removals) should follow this workflow:

1. **Open an Issue or Discussion**  
   - Describe the problem, feature, or refactor you want to work on.  
   - This ensures visibility and avoids duplicate work.  

2. **Create a Branch**  
   - Branch from `main`.  
   - Use a descriptive branch name (e.g. `fix/connection-timeout`, `feat/add-peer-discovery`).  

3. **Draft a Pull Request**  
   - Open a draft PR early, even if the work is not finished.  
   - This allows us to give feedback as you work.  

4. **Request Review**  
   - Mark the PR as "Ready for Review" once the work is complete.  
   - Address review comments promptly and keep [commits clean](#commit-messages).  

5. **Merge**  
   - Once approved and all checks pass, the PR will be merged into `main`.  
   - See the [Pull Requests](#pull-requests) section for details on requirements before merging.  

> **Note:** Small, focused PRs are easier to review and merge quickly.  

## Pull Requests
- Target the `main` branch (unless told otherwise)
- Keep changes **small and focused**
- Make changes **atomic** (one topic per PR)
- Add **tests** for complex features
- **All** PRs must pass the following before merging:
  - Code linting
  - Formatting checks
  - Tests
- Use [**clear, detailed logging**](#Logging)

## Logging
We use [tracing](https://docs.rs/tracing/latest/tracing/) for logging.

Examples of good vs bad logging:

Good logging:
```rust
info!("Starting torrent session");
debug!("Initializing network listeners");
```

Bad logging:
```rust
println!("Starting torrent session");
println!("Initializing network listeners");
```
> Why? Always use tracing macros instead of println!

Good logging:
```rust
let my_var = 42;
info!(my_var, "We have a variable");
```

Bad logging:
```rust
let my_var = 42;
info!("My variable is {}", my_var);
```
> Why? Tracing actually stores logs in a json-like format. Formatting variables into the string makes harder to search for them later on.

### Logging with tracing
Tracing has a few different log levels that can be used to log different types of messages. Here’s how we use them:

#### `trace!`
This log level should be for:
- Information that a maintainer or developer might find useful for debugging

#### `debug!`
This log level should be for:
- Information that the user might find useful for debugging

#### `info!`
This log level should be for:
- Information or notifications that are important for the user
- Large scale successful events (ex. Started torrent session, Successfully connected to all trackers)

#### `warn!`
This log level should be for:
- A recoverable error that the user might want to know about
- A suspicious event that the user might want to know about

#### `error!`
This log level should be for:
- A critical error that the user should know about

---

✅ Now the **Workflow** section explicitly links to the **Pull Requests** section for merge requirements.  

Would you like me to also add a **"Quick Reference Checklist"** at the bottom (like a contributor TL;DR) so new contributors can see the whole process at a glance?

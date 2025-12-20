# Design Steering Guidelines

## 1. Data Structure First

Always start with a data structure to model the problem.

- For versionable constructs, include version fields and consider migration strategies
- For platform-specific behavior, define traits/interfaces that abstract OS differences
- Example pattern for platform abstraction:

```rust
pub trait PlatformHandler {
    fn get_config_path(&self) -> PathBuf;
    fn get_cache_dir(&self) -> PathBuf;
}

#[cfg(target_os = "windows")]
pub struct WindowsHandler;

#[cfg(target_os = "macos")]
pub struct MacOSHandler;

#[cfg(target_os = "linux")]
pub struct LinuxHandler;
```

## 2. Pyramid Architecture

Build code in layers like a pyramid:

1. **Base Layer (Primitives)**: Small, focused functions with a single clear purpose
2. **Middle Layer (Composition)**: Combine primitives to form higher-level operations
3. **Top Layer (User Interface)**: CLI commands, GUI handlers, or API endpoints

Each layer should only depend on the layer below it.

## 3. Performance with Pragmatism

- Don't over-optimize prematurely
- For repeated patterns that benefit from caching, use a wrapper with private implementation:

```rust
pub struct CachedLoader<T> {
    inner: Box<dyn LoaderBackend<T>>,
    cache: HashMap<String, T>,
}

trait LoaderBackend<T> {
    fn load(&self, key: &str) -> Result<T>;
}
```

This pattern allows swapping backends without changing the public API.

## 4. Testability & SOLID Principles

- Use dependency injection to allow mock objects in tests
- Prefer traits over concrete types in function signatures
- Keep side effects at the edges of the system

```rust
pub fn process_data<R: DataReader, W: DataWriter>(
    reader: &R,
    writer: &W,
) -> Result<()> {
    // Implementation can be tested with mock reader/writer
}
```

## 5. Function Length Guidelines

If a function exceeds ~75 lines:

1. **Stop and consult** the user before proceeding
2. Suggest options:
   - Extract helper functions
   - Split into multiple stages/phases
   - Use a builder or state machine pattern
   - Create a dedicated struct with methods

Provide concrete suggestions based on the function's purpose.

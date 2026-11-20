// clippy::double_must_use: the async-trait macro expansion emits a bare
// `#[must_use]` on every desugared boxed-future method, and clippy (1.99+)
// flags it because `Pin<Box<dyn Future>>` is itself a must_use type. The
// attribute is macro-generated, not author-written, so a targeted crate-level
// allow with this note is the minimal documented exception (28 sites across
// the crate, zero of them hand-written); revisit if async-trait stops
// emitting the attribute upstream.
#![allow(clippy::double_must_use)]

pub use async_trait;

pub mod events;
pub mod keybinding;
pub mod model;
pub mod sync;
pub mod traits;

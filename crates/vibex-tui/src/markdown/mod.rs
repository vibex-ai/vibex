//! Markdown parsing and terminal rendering, self-contained in this crate.
//!
//! The document model (`model`) and the parser (`parser`) live here and drive
//! `pulldown-cmark` directly, with `html` projecting inline HTML to safe text
//! and `resource` resolving links and images. `render` turns the parsed
//! document into terminal lines; nothing in the module tree touches GPUI.

mod html;
mod limits;
mod model;
mod parser;
mod render;
mod resource;

// `model` stays public because `render::plain_inlines` exposes `InlineNode` in
// its signature; the rest of the parse stack is crate-internal.
pub use model::*;
pub(crate) use parser::parse_markdown;
pub use render::*;
pub(crate) use resource::ResolvedResource;

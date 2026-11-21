//! The languages Vibex ships and the product copy keyed by its English source.
//!
//! This crate is platform-neutral: the desktop and both phone clients share one
//! classification and one translation table, so a sentence added to the English
//! table reaches every surface without a second copy.

pub mod locale;
pub mod locale_copy;

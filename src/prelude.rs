//! Traits [`Future`], [`Stream`], [`AsyncRead`], [`AsyncWrite`], [`AsyncBufRead`],
//! [`AsyncSeek`], and their extensions.
//!
//! # Examples
//!
//! ```
//! use smol::prelude::*;
//! ```

// Define our own prelude module instead of re-exporting from futures-lite,
// because rustdoc issue: https://github.com/smol-rs/smol/issues/354

#[doc(no_inline)]
pub use crate::{
    future::{Future, FutureExt as _},
    stream::{Stream, StreamExt as _},
};

#[doc(no_inline)]
pub use crate::{
    io::{AsyncBufRead, AsyncBufReadExt as _},
    io::{AsyncRead, AsyncReadExt as _},
    io::{AsyncSeek, AsyncSeekExt as _},
    io::{AsyncWrite, AsyncWriteExt as _},
};

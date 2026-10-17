//! Message catalogs (ADR-0010).
//!
//! Every spoken, brailled or displayed string of gorca is a message id (`MsgId`, generated from
//! `catalog/en/*.msg`) resolved in the UI language ([`language`]). The catalogs are embedded
//! `key = template` files with named placeholders and plural forms ([`template`]); extensions ship
//! their own namespaced catalogs and refer to them by message reference ([`ext`]).
//!
//! Pure and synchronous: no I/O, no runtime, no transport.

mod catalog;
pub mod ext;
mod lang;
mod template;

pub use catalog::{
    Catalog, CatalogError, MsgId, Report, catalog, check, parse_catalog, t, t_in, tf, tf_in,
};
pub use lang::{Lang, ScopedLang, language, scoped, set_language};
pub use template::{Arg, Template, TemplateError};

/// A message in the current language: `msg!(RoleButton)` or `msg!(ItemsCount, n = 3)`.
#[macro_export]
macro_rules! msg {
    ($id:ident) => {
        $crate::t($crate::MsgId::$id)
    };
    ($id:ident, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::tf(
            $crate::MsgId::$id,
            &[$((stringify!($name), $crate::Arg::from($val))),+],
        )
    };
}

/// A message in an explicit language.
#[macro_export]
macro_rules! msg_in {
    ($lang:expr, $id:ident) => {
        $crate::t_in($lang, $crate::MsgId::$id)
    };
    ($lang:expr, $id:ident, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::tf_in(
            $lang,
            $crate::MsgId::$id,
            &[$((stringify!($name), $crate::Arg::from($val))),+],
        )
    };
}

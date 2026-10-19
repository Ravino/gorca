// SPDX-License-Identifier: GPL-3.0-or-later
//! `coded_enum!`: a fieldless enum with explicit, append-only wire codes.
//!
//! Every registry type of the semantic model (groups, roles, provenance, confidence classes,
//! tri-states, source classes) is generated from one table, so the code, the name, the
//! decoder and the `ALL` list cannot drift apart. The first variant is the `Default`.
//!
//! No `Ord`: a code order means "stronger" in one registry and "weaker" in another, so ordered
//! types provide explicit helpers instead (`ConfClass::weaker`, `BindQuality::at_least`, ...).

macro_rules! coded_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident : $repr:ty {
            $( $(#[$vmeta:meta])* $var:ident = $code:literal => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[repr($repr)]
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $var = $code ),+
        }

        impl $name {
            /// Every registered variant in code order.
            pub const ALL: &'static [$name] = &[ $( $name::$var ),+ ];

            /// The wire code.
            #[inline]
            pub const fn code(self) -> $repr {
                self as $repr
            }

            /// Decode a wire code; `None` for an unregistered code.
            #[inline]
            pub const fn from_code(code: $repr) -> Option<Self> {
                match code {
                    $( $code => Some($name::$var), )+
                    _ => None,
                }
            }

            /// The registry name (stable; used by the oracle fixtures and the WIT mapping).
            pub const fn name(self) -> &'static str {
                match self {
                    $( $name::$var => $text ),+
                }
            }

            /// Decode a registry name.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $( $text => Some($name::$var), )+
                    _ => None,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::ALL[0]
            }
        }
    };
}

pub(crate) use coded_enum;

//! Built-in providers shipped with huffi.
//!
//! These are the concrete [`Provider`](crate::engine::provider::Provider)
//! implementations bundled with huffi. They are re-exported from
//! [`crate::engine::provider`] so they can be registered on a
//! [`ProviderCollection`](crate::engine::provider::ProviderCollection) by name.

pub mod calculator;
pub mod desktop;
pub mod meta;
pub mod test_provider;

pub use calculator::CalculatorProvider;
pub use desktop::DesktopEntryProvider;
pub use meta::MetaProvider;
pub use test_provider::TestProvider;

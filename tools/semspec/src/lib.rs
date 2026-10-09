//! Semantic specifications. Parsing, sealing and review state are pure logic;
//! project I/O and execution are separate modules. No product-specific dependency.
pub mod helpers;
pub mod ledger;
pub mod model;
pub mod parse;
pub mod project;
pub mod runner;
pub mod seal;

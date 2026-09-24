//! The simulator: synthetic learners typing sessions composed by the real
//! scheduler and applied through the real analysis, so that the tunables
//! can be set on evidence and an estimator's bias is caught before a user
//! meets it.

pub mod learner;
pub mod measure;
pub mod report;
pub mod run;
pub mod schedule;

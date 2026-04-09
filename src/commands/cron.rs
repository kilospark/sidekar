//! Cron subsystem — schedule recurring sidekar tool calls.
//!
//! Runs as an in-process tokio task (like monitor). Jobs are persisted in the
//! broker SQLite database and restored on startup.

use crate::broker;
use crate::*;

mod commands;
mod runtime;
mod schedule;

use runtime::{CronAction, CronJob, cron_cell, epoch_now, normalize_cron_target};
#[cfg(test)]
use runtime::{job_belongs_to_agent, normalize_loaded_target};
#[cfg(test)]
use schedule::interval_to_cron;
#[cfg(test)]
use schedule::{CronSchedule, parse_field};

pub(crate) use commands::*;
#[allow(unused_imports)]
pub(crate) use runtime::{
    CronContext, CronState, start_cron_loop, start_default_cron_loop, update_cron_context,
};
#[allow(unused_imports)]
pub(crate) use schedule::interval_to_secs;

#[cfg(test)]
mod tests;

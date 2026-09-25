use clap::Parser;

use super::super::{Cli, operation::RuntimeNeed};

fn operation_for(args: &[&str]) -> super::super::operation::CommandOperation {
    Cli::parse_from(args).command.operation()
}

mod audit;
mod runtime;

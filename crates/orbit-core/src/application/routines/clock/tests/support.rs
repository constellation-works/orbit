use std::sync::Mutex;

use orbit_common::OrbitError;

use super::super::manager::{ClockCommandRunner, ManagerCommand, ManagerCommandOutput};

pub(super) struct MockRunner {
    results: Mutex<Vec<Result<bool, OrbitError>>>,
    outputs: Mutex<Vec<Result<Option<String>, OrbitError>>>,
    probes: Mutex<Vec<Result<ManagerCommandOutput, OrbitError>>>,
    commands: Mutex<Vec<String>>,
}

impl MockRunner {
    pub(super) fn new(results: Vec<Result<bool, OrbitError>>) -> Self {
        Self {
            results: Mutex::new(results.into_iter().rev().collect()),
            outputs: Mutex::new(Vec::new()),
            probes: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("test command log lock").clone()
    }
}

impl ClockCommandRunner for MockRunner {
    fn run(&self, command: &ManagerCommand) -> Result<bool, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.results
            .lock()
            .expect("test result queue lock")
            .pop()
            .expect("test configured a result for every manager command")
    }

    fn stdout(&self, command: &ManagerCommand) -> Result<Option<String>, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        self.outputs
            .lock()
            .expect("test output queue lock")
            .pop()
            .expect("test configured output for every manager query")
    }

    fn probe(&self, command: &ManagerCommand) -> Result<ManagerCommandOutput, OrbitError> {
        self.commands
            .lock()
            .expect("test command log lock")
            .push(command.display());
        if let Some(output) = self.probes.lock().expect("test probe queue lock").pop() {
            return output;
        }
        let success = self
            .results
            .lock()
            .expect("test result queue lock")
            .pop()
            .expect("test configured a result for every manager command")?;
        Ok(ManagerCommandOutput {
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

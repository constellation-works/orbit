//! `orbit browser-kit verify`: the loopback screenshot smoke test.
//!
//! The test sources the kit's `env.sh` in its own shell, exactly as a
//! worker's check does, serves a page on loopback, loads it in headless
//! Chromium and screenshots it. A pass is recorded for the installed
//! Playwright version and browser revision, and only then does the CLI
//! runner hand workers the kit. A failure clears any earlier record.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all};
use orbit_core::bootstrap::browser_kit::{
    BROWSERS_PATH_ENV, ENV_SCRIPT_ENV, KitInspection, KitLayout, KitVerification, MODULE_ENV,
};
use serde::Serialize;

use super::host::{require_host_operator, require_tool, run_probe};

/// What a pass does not prove.
pub const HOST_ONLY_NOTE: &str = "A host pass does not prove the sandboxed worker path. The \
     decisive check is a sandboxed worker run that sources \"$ORBIT_BROWSER_KIT_ENV\", runs a \
     browser check and attaches its screenshot.";

const SMOKE_DEADLINE: Duration = Duration::from_secs(180);
const SCREENSHOT_ENV: &str = "ORBIT_BROWSER_KIT_SCREENSHOT";
const SMOKE_SCRIPT: &str = r#"
import http from "node:http";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
const server = http.createServer((_, res) => res.end("<h1>kit ok</h1>"));
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const text = await page.textContent("h1");
  await page.screenshot({ path: process.env.ORBIT_BROWSER_KIT_SCREENSHOT });
  console.log(JSON.stringify({ text, version: browser.version() }));
} finally {
  await browser.close();
  server.close();
}
"#;
/// The tail of a failed run's output kept in the report.
const OUTPUT_TAIL_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub passed: bool,
    /// The version Chromium reported.
    pub browser_version: Option<String>,
    /// The page the smoke test rendered.
    pub screenshot: Option<PathBuf>,
    /// Which part of the kit a failure points at.
    pub diagnosis: Option<String>,
    /// The end of a failed run's output.
    pub output: Option<String>,
    pub note: &'static str,
}

pub fn verify(layout: &KitLayout) -> Result<VerifyReport, OrbitError> {
    require_host_operator()?;
    // Revoke the previous pass before any fallible work: preparation errors,
    // probe spawn failures and timeouts must all leave the kit unverified.
    match std::fs::remove_file(layout.verification_file()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "remove {}: {error}",
                layout.verification_file().display()
            )));
        }
    }
    let kit = KitInspection::inspect(layout);
    if !kit.present {
        return Err(OrbitError::InvalidInput(format!(
            "no browser kit at {}; run `orbit browser-kit install`",
            layout.root().display()
        )));
    }
    let (Some(version), Some(revision)) = (&kit.playwright_version, &kit.chromium_revision) else {
        return Err(OrbitError::InvalidInput(
            "the kit has no Playwright module; run `orbit browser-kit install`".to_string(),
        ));
    };
    if !kit.browser_installed || !kit.env_script_present {
        return Err(OrbitError::InvalidInput(format!(
            "the kit is incomplete ({}); run `orbit browser-kit install`",
            if kit.browser_installed {
                "env.sh is missing".to_string()
            } else {
                format!("chromium_headless_shell-{revision} is not installed")
            }
        )));
    }
    let node = require_tool("node")?;
    let verify_dir = layout.root().join("verify");
    create_private_dir_all(&verify_dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", verify_dir.display())))?;
    let screenshot = verify_dir.join("smoke.png");

    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            r#". "$1" && exec "$2" --input-type=module -e "$3""#,
            "orbit-browser-kit",
        ])
        .arg(layout.env_script())
        .arg(&node)
        .arg(SMOKE_SCRIPT)
        // `env.sh` alone must supply the kit, as it does for a worker.
        .env_remove(BROWSERS_PATH_ENV)
        .env_remove(MODULE_ENV)
        .env_remove(ENV_SCRIPT_ENV)
        .env(SCREENSHOT_ENV, &screenshot);
    let output = run_probe(&mut command, SMOKE_DEADLINE)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok());
    let browser_version = result
        .as_ref()
        .and_then(|value| value.get("version")?.as_str().map(str::to_string));
    let passed = output.status.success()
        && result
            .as_ref()
            .and_then(|value| value.get("text")?.as_str())
            == Some("kit ok")
        && screenshot.is_file();

    if passed {
        let record = KitVerification {
            schema_version: 1,
            playwright_version: version.clone(),
            chromium_revision: revision.clone(),
            browser_version: browser_version.clone().unwrap_or_default(),
            verified_at: chrono::Utc::now().to_rfc3339(),
        };
        let text = serde_json::to_string_pretty(&record)
            .map_err(|error| OrbitError::Execution(format!("encode verification: {error}")))?;
        atomic_write_text(&layout.verification_file(), &format!("{text}\n")).map_err(|error| {
            OrbitError::Io(format!(
                "write {}: {error}",
                layout.verification_file().display()
            ))
        })?;
        return Ok(VerifyReport {
            passed,
            browser_version,
            screenshot: Some(screenshot),
            diagnosis: None,
            output: None,
            note: HOST_ONLY_NOTE,
        });
    }

    let combined = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    Ok(VerifyReport {
        passed,
        browser_version,
        screenshot: None,
        diagnosis: Some(diagnose(&combined)),
        output: Some(tail(&combined)),
        note: HOST_ONLY_NOTE,
    })
}

/// Which part of the kit a failure points at. It is usually the kit, not
/// the page under test.
fn diagnose(output: &str) -> String {
    if output.contains("error while loading shared libraries") || output.contains(".so") {
        "a shared library is missing: run `orbit browser-kit status` for the unresolved \
         libraries, then `orbit browser-kit install`"
            .to_string()
    } else if output.contains("FontConfig") || output.contains("Fontconfig") {
        "Chromium crashed in Skia/FontConfig: the host has no fonts; rerun `orbit browser-kit \
         install` to unpack fonts into the kit"
            .to_string()
    } else if output.contains("Executable doesn't exist") {
        "the Playwright module and the installed browser revision are out of step: rerun \
         `orbit browser-kit install`"
            .to_string()
    } else {
        "the smoke test failed; read its output, fix the kit, then rerun `orbit browser-kit \
         verify`"
            .to_string()
    }
}

fn tail(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= OUTPUT_TAIL_BYTES {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - OUTPUT_TAIL_BYTES;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_string()
}

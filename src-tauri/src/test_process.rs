use std::{
    fs::{self, File},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

const CHILD_TEST: &str = "RISUNEST_ISOLATED_TEST";

// Returns false only inside the child selected for this exact test.
pub(crate) fn run_isolated(qualified_name: &str) -> bool {
    // module_path! includes the crate name, while libtest's filters do not.
    let name = qualified_name.split_once("::").expect("qualified test name").1;
    if std::env::var(CHILD_TEST).as_deref() == Ok(name) {
        return false;
    }
    run_child(name, Duration::from_secs(60)).unwrap_or_else(|error| panic!("{error}"));
    true
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn wait(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Err(error) => return Err(format!("cannot wait for isolated test: {error}")),
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return Err(format!("isolated test timed out after {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_child(name: &str, timeout: Duration) -> Result<(), String> {
    let logs = tempfile::tempdir().map_err(|error| error.to_string())?;
    let stdout = logs.path().join("stdout");
    let stderr = logs.path().join("stderr");
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = OwnedChild(Command::new(executable)
        .args(["--exact", name, "--test-threads=1", "--format=pretty", "--color=never"])
        .env(CHILD_TEST, name)
        .stdin(Stdio::null())
        // Files avoid a full pipe blocking a child before the parent can reap it.
        .stdout(Stdio::from(File::create(&stdout).map_err(|error| error.to_string())?))
        .stderr(Stdio::from(File::create(&stderr).map_err(|error| error.to_string())?))
        .spawn().map_err(|error| format!("cannot start isolated test {name}: {error}"))?);
    let outcome = wait(&mut child.0, timeout);
    drop(child);
    let output = fs::read_to_string(stdout).map_err(|error| error.to_string())?;
    let errors = fs::read_to_string(stderr).map_err(|error| error.to_string())?;
    let diagnostic = || format!("test: {name}\nstdout:\n{output}\nstderr:\n{errors}");
    let status = outcome.map_err(|error| format!("{error}\n{}", diagnostic()))?;
    if !status.success() {
        return Err(format!("isolated test exited with {status}\n{}", diagnostic()));
    }
    if !output.lines().any(|line| line == format!("test {name} ... ok"))
        || !output.lines().any(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"))
    {
        return Err(format!("isolated test did not pass exactly one selected test\n{}", diagnostic()));
    }
    Ok(())
}

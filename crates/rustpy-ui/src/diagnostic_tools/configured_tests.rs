//! Explicitly admitted real debugger/sampler acceptance; run this selector alone.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

struct Stop<'a>(&'a AtomicBool);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn selected(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("required {name}")));
    assert!(
        path.is_absolute() && path.is_file(),
        "{name}: absolute installed executable required"
    );
    std::fs::canonicalize(path).unwrap()
}

fn identity(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn observe_children(program: &Path, seen: &mut BTreeMap<u32, u64>) {
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(children) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for pid in children
            .split_whitespace()
            .filter_map(|pid| pid.parse::<u32>().ok())
        {
            if std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .as_deref()
                == Some(program)
            {
                if let Some(start) = identity(pid) {
                    seen.insert(pid, start);
                }
            }
        }
    }
}

fn actual_capture(plan: Plan) -> Artifact {
    let done = AtomicBool::new(false);
    let program = plan.program.clone();
    let (result, seen) = std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let mut seen = BTreeMap::new();
            while !done.load(Ordering::Acquire) {
                observe_children(&program, &mut seen);
                std::thread::sleep(Duration::from_millis(1));
            }
            observe_children(&program, &mut seen);
            seen
        });
        let stop = Stop(&done);
        let result = capture(plan, &ProgressReporter::new());
        drop(stop);
        (result, observer.join().unwrap())
    });
    let artifact = result.unwrap_or_else(|error| panic!("real diagnostic provider failed (installation/kernel attach permissions are required): {error:?}"));
    assert!(
        !seen.is_empty(),
        "must observe the actual selected diagnostic child, not a fabricated artifact"
    );
    for (pid, start) in seen {
        assert_ne!(
            identity(pid),
            Some(start),
            "diagnostic child {pid} must physically retire before capture returns"
        );
    }
    artifact
}

// Keep CPU work in the target process while samply records actual samples.
#[inline(never)]
fn diagnostic_sample_work(mut value: u64) -> u64 {
    for _ in 0..100_000 {
        value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
    value
}

#[test]
#[ignore = "requires RUSTPY_DIAGNOSTICS_ADMITTED=1, explicit GDB/SAMPLY/ROOT and OS permissions to attach/sample this process; run alone"]
fn configured_real_self_thread_capture_and_cpu_profile_retire_providers() {
    assert_eq!(
        std::env::var("RUSTPY_DIAGNOSTICS_ADMITTED").as_deref(),
        Ok("1")
    );
    let gdb = selected("RUSTPY_DIAGNOSTICS_GDB");
    let samply = selected("RUSTPY_DIAGNOSTICS_SAMPLY");
    let root = PathBuf::from(
        std::env::var_os("RUSTPY_DIAGNOSTICS_ROOT")
            .expect("explicit disposable output root required"),
    );
    assert!(root.is_absolute() && root.is_dir());
    let output = root.join("actual-self-diagnostics");
    std::fs::create_dir(&output).expect("fresh owned output directory required");
    let settings = rustpy_settings::diagnostics::Settings {
        debugger_program: gdb.to_str().unwrap().into(),
        profiler_program: samply.to_str().unwrap().into(),
        profile_seconds: 3,
        ..Default::default()
    };
    let threads = actual_capture(
        plan(
            Kind::Threads,
            &settings,
            std::process::id(),
            output.join("threads.txt"),
        )
        .unwrap(),
    );
    let text = threads.text.expect("real debugger stack text");
    assert_eq!(std::fs::read_to_string(&threads.path).unwrap(), text);
    assert!(
        text.contains("Thread ") && text.contains("#0"),
        "GDB must publish actual thread frames: {text}"
    );

    let done = AtomicBool::new(false);
    let profile = std::thread::scope(|scope| {
        let work = scope.spawn(|| {
            let mut value = 1;
            while !done.load(Ordering::Acquire) {
                value = diagnostic_sample_work(value);
            }
            std::hint::black_box(value)
        });
        // Stop the workload even when the genuine provider fails or is denied.
        let stop = Stop(&done);
        let artifact = actual_capture(
            plan(
                Kind::Profile,
                &settings,
                std::process::id(),
                output.join("cpu-profile.json.gz"),
            )
            .unwrap(),
        );
        drop(stop);
        work.join().unwrap();
        artifact
    });
    assert!(profile.text.is_none());
    validate_profile(&profile.path)
        .expect("actual Firefox profile accepted by production viewer validation");
    let file = std::fs::File::open(&profile.path).unwrap();
    let profile: serde_json::Value =
        serde_json::from_reader(flate2::read::GzDecoder::new(file)).unwrap();
    let pid = std::process::id().to_string();
    assert!(
        profile["threads"].as_array().unwrap().iter().any(|thread| {
            let same_process = thread["pid"].as_u64() == Some(u64::from(std::process::id()))
                || thread["pid"].as_str() == Some(pid.as_str());
            same_process
                && thread["samples"]["length"]
                    .as_u64()
                    .is_some_and(|count| count > 0)
        }),
        "real profile must contain CPU samples attributed to this test process"
    );
}

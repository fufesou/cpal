use super::*;
use crate::traits::{DeviceTrait, HostTrait};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

const WARMUP_CYCLES: usize = 4;
const MEASURED_CYCLES: usize = 16;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const SETTLE_TIME: Duration = Duration::from_millis(100);
const CHILD_DEADLINE: Duration = Duration::from_secs(10);
const CLOSED_STDERR_CHILD: &str = "CPAL_TEST_CLOSED_STDERR_CHILD";

#[test]
#[ignore = "Requires a native F32 output device; run alone with a 60-second process deadline"]
fn native_drop_survives_closed_stderr() {
    if std::env::var_os(CLOSED_STDERR_CHILD).is_some() {
        return closed_stderr_child();
    }
    let mut child = ProcessCommand::new(std::env::current_exe().unwrap())
        .args([
            "native_drop_survives_closed_stderr",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CLOSED_STDERR_CHILD, "1")
        .creation_flags(Threading::CREATE_NO_WINDOW.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    let deadline = Instant::now() + CHILD_DEADLINE;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Closed-stderr child exceeded its deadline");
        }
        thread::sleep(POLL_INTERVAL);
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    println!("{stdout}");
    assert!(output.status.success(), "Child failed: {}", output.status);
    assert!(stdout.contains("test result: ok. 1 passed;"), "{stdout}");
}

fn closed_stderr_child() {
    std::io::stdin().read_exact(&mut [0]).unwrap();
    std::panic::set_hook(Box::new(|info| println!("{info}")));
    let error = std::io::stderr().write_all(b"probe").unwrap_err();
    println!("stderr_error={error:?}");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe,
        "The child must have a broken pipe, not a missing console"
    );
    for _ in 0..WARMUP_CYCLES {
        native_cycle(false);
    }
    let before = handle_count();
    native_cycle(true);
    let after = handle_count();
    assert_eq!(after, before, "Closed-stderr destruction leaked handles");
}

#[test]
#[ignore = "Requires a native F32 output device; run alone with a 60-second process deadline"]
fn native_callback_can_drop_stream_before_worker_exit() {
    let host = crate::host::wasapi::Host::new().unwrap();
    let device = host.default_output_device().unwrap();
    let supported = device.default_output_config().unwrap();
    assert_eq!(supported.sample_format(), SampleFormat::F32);
    let (stream_tx, stream_rx) = channel::<Stream>();
    let (result_tx, result_rx) = channel();
    let output = device
        .build_output_stream(
            &supported.into(),
            move |data: &mut [f32], _| {
                data.fill(0.0);
                let output = stream_rx.recv_timeout(CALLBACK_TIMEOUT).unwrap();
                let event = Foundation::HANDLE(output.pending_scheduled_event.as_raw_handle() as _);
                drop(output);
                // The worker still needs its command event after this callback returns.
                result_tx
                    .send(unsafe { Threading::SetEvent(event) })
                    .unwrap();
            },
            |error| eprintln!("Native callback error: {error}"),
            None,
        )
        .unwrap();
    output.play().unwrap();
    stream_tx.send(output).unwrap();
    result_rx
        .recv_timeout(CALLBACK_TIMEOUT)
        .expect("Stream destruction blocked inside its own callback")
        .expect("Stream destruction closed the live worker's command event");
    assert_eq!(
        result_rx.recv_timeout(CALLBACK_TIMEOUT),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        "Worker did not exit after callback-thread destruction"
    );
}

fn handle_count() -> u32 {
    let mut count = 0;
    unsafe { Threading::GetProcessHandleCount(Threading::GetCurrentProcess(), &mut count) }
        .unwrap();
    count
}

fn native_cycle(worker_exits_first: bool) {
    let host = crate::default_host();
    let device = host.default_output_device().unwrap();
    let supported = device.default_output_config().unwrap();
    assert_eq!(supported.sample_format(), SampleFormat::F32);
    let (tx, rx) = channel();
    let output = device
        .build_output_stream(
            &supported.into(),
            move |data: &mut [f32], _| {
                data.fill(0.0);
                tx.send(()).unwrap();
                // Test-only fault injection exits the real WASAPI worker before Drop.
                assert!(!worker_exits_first, "Injected native worker exit");
            },
            |error| eprintln!("Native callback error: {error}"),
            None,
        )
        .unwrap();
    output.play().unwrap();
    rx.recv_timeout(CALLBACK_TIMEOUT).unwrap();
    if worker_exits_first {
        let deadline = Instant::now() + CALLBACK_TIMEOUT;
        while output.play().is_ok() {
            assert!(Instant::now() < deadline, "Worker receiver did not close");
            thread::sleep(POLL_INTERVAL);
        }
    }
    drop(output);
    thread::sleep(SETTLE_TIME);
}

fn measured_cycles(worker_exits_first: bool) -> Vec<u32> {
    let mut counts = vec![handle_count()];
    for _ in 0..MEASURED_CYCLES {
        native_cycle(worker_exits_first);
        counts.push(handle_count());
    }
    counts
}

#[test]
#[ignore = "Requires a native F32 output device; run alone with a 60-second process deadline"]
fn native_worker_exit_does_not_leak_process_handles() {
    for _ in 0..WARMUP_CYCLES {
        native_cycle(false);
    }
    let normal_before = measured_cycles(false);
    let exited_first = measured_cycles(true);
    let normal_after = measured_cycles(false);
    eprintln!("normal_before={normal_before:?} worker_exits_first={exited_first:?} normal_after={normal_after:?}");
    assert_eq!(normal_before.first(), normal_before.last());
    assert_eq!(normal_after.first(), normal_after.last());
    assert_eq!(
        exited_first.first(),
        exited_first.last(),
        "Worker-exit-before-drop leaked native process handles"
    );
}

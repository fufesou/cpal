use super::*;
use crate::traits::{DeviceTrait, HostTrait};
use std::{
    ffi::CString,
    time::{Duration, Instant},
};

const WORKER_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug)]
enum WorkerExit {
    Termination,
    Finished,
    Panicked,
}

fn stream(exit: WorkerExit) -> (Stream, CString) {
    let name = CString::new(format!(
        "Local\\cpal-lifecycle-{}-{exit:?}",
        std::process::id()
    ))
    .unwrap();
    let event = unsafe {
        Threading::CreateEventA(None, false, false, windows::core::PCSTR(name.as_ptr() as _))
    }
    .unwrap();
    let (tx, rx) = channel();
    let wait_for_exit = !matches!(exit, WorkerExit::Termination);
    let worker = thread::spawn(move || match exit {
        WorkerExit::Termination => assert!(matches!(rx.recv().unwrap(), Command::Terminate)),
        WorkerExit::Finished => drop(rx),
        WorkerExit::Panicked => {
            drop(rx);
            panic!("Injected worker failure");
        }
    });
    if wait_for_exit {
        let deadline = Instant::now() + WORKER_DEADLINE;
        while !worker.is_finished() {
            assert!(Instant::now() < deadline, "Worker failed to exit");
            thread::yield_now();
        }
    }
    let owned_event = unsafe { OwnedHandle::from_raw_handle(event.0 as _) };
    (
        Stream {
            thread: Some(worker),
            commands: tx,
            pending_scheduled_event: owned_event,
        },
        name,
    )
}

fn assert_event_closed(exit: WorkerExit) {
    let (stream, name) = stream(exit);
    drop(stream);
    // Another test can reuse a closed handle value; check the event's identity instead.
    match unsafe {
        Threading::OpenEventA(
            Threading::EVENT_MODIFY_STATE,
            false,
            windows::core::PCSTR(name.as_ptr() as _),
        )
    } {
        Ok(event) => {
            let _event = unsafe { OwnedHandle::from_raw_handle(event.0 as _) };
            panic!("Stream destruction leaked its command event");
        }
        Err(error) => assert_eq!(
            error.code(),
            windows::core::HRESULT::from_win32(Foundation::ERROR_FILE_NOT_FOUND.0),
        ),
    }
}

#[test]
fn normal_shutdown_closes_command_event() {
    assert_event_closed(WorkerExit::Termination);
}

#[test]
fn completed_worker_still_closes_command_event() {
    assert_event_closed(WorkerExit::Finished);
}

#[test]
fn panicked_worker_does_not_panic_in_drop_or_leak_event() {
    assert_event_closed(WorkerExit::Panicked);
}

const WARMUP_CYCLES: usize = 4;
const MEASURED_CYCLES: usize = 16;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const SETTLE_TIME: Duration = Duration::from_millis(100);

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

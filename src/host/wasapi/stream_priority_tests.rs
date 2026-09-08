use super::boost_current_thread_priority;
use windows::Win32::System::Threading;

const THREAD_PRIORITY_ERROR_RETURN: i32 = i32::MAX;

#[test]
fn boost_changes_the_calling_thread_priority() {
    assert_boost_from(Threading::THREAD_PRIORITY_NORMAL);
}

#[test]
fn boost_works_from_below_normal_priority() {
    assert_boost_from(Threading::THREAD_PRIORITY_BELOW_NORMAL);
}

#[test]
fn boost_works_from_above_normal_priority() {
    assert_boost_from(Threading::THREAD_PRIORITY_ABOVE_NORMAL);
}

#[test]
fn boost_preserves_time_critical_priority() {
    assert_boost_from(Threading::THREAD_PRIORITY_TIME_CRITICAL);
}

fn assert_boost_from(initial: Threading::THREAD_PRIORITY) {
    let parent = unsafe { Threading::GetCurrentThread() };
    let parent_priority = unsafe { Threading::GetThreadPriority(parent) };
    assert_ne!(parent_priority, THREAD_PRIORITY_ERROR_RETURN);
    std::thread::spawn(move || unsafe {
        let thread = Threading::GetCurrentThread();
        let original = Threading::GetThreadPriority(thread);
        assert_ne!(original, THREAD_PRIORITY_ERROR_RETURN);
        Threading::SetThreadPriority(thread, initial)
            .expect("could not establish initial priority");

        let result = boost_current_thread_priority();
        let observed = Threading::GetThreadPriority(thread);
        let repeated = boost_current_thread_priority();
        let observed_again = Threading::GetThreadPriority(thread);
        let restored = Threading::SetThreadPriority(thread, Threading::THREAD_PRIORITY(original));

        restored.expect("could not restore the original priority");
        result.expect("priority boost failed");
        repeated.expect("repeated priority boost failed");
        assert_eq!(
            observed,
            Threading::THREAD_PRIORITY_TIME_CRITICAL.0,
            "the production priority helper did not boost its calling thread"
        );
        assert_eq!(observed_again, observed);
        assert_eq!(Threading::GetThreadPriority(thread), original);
    })
    .join()
    .expect("priority regression thread failed");
    assert_eq!(
        unsafe { Threading::GetThreadPriority(parent) },
        parent_priority
    );
}

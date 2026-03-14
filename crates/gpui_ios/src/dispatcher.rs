use dispatch2::{DispatchQueue, DispatchQueueGlobalPriority, DispatchTime, GlobalQueueIdentifier};
use gpui::{
    GLOBAL_THREAD_TIMINGS, PlatformDispatcher, Priority, RunnableMeta, RunnableVariant,
    THREAD_TIMINGS, TaskTiming, ThreadTaskTimings,
};
use objc::{class, msg_send, runtime::BOOL, sel, sel_impl};

use async_task::Runnable;
use std::{
    ffi::c_void,
    ptr::NonNull,
    time::{Duration, Instant},
};

pub(crate) struct IosDispatcher;

impl IosDispatcher {
    pub fn new() -> Self {
        Self
    }
}

impl PlatformDispatcher for IosDispatcher {
    fn get_all_timings(&self) -> Vec<ThreadTaskTimings> {
        let global_timings = GLOBAL_THREAD_TIMINGS.lock();
        ThreadTaskTimings::convert(&global_timings)
    }

    fn get_current_thread_timings(&self) -> ThreadTaskTimings {
        THREAD_TIMINGS.with(|timings| {
            let timings = timings.lock();
            let thread_name = timings.thread_name.clone();
            let total_pushed = timings.total_pushed;
            let timings = &timings.timings;
            let mut vec = Vec::with_capacity(timings.len());
            let (s1, s2) = timings.as_slices();
            vec.extend_from_slice(s1);
            vec.extend_from_slice(s2);
            ThreadTaskTimings {
                thread_name,
                thread_id: std::thread::current().id(),
                timings: vec,
                total_pushed,
            }
        })
    }

    fn is_main_thread(&self) -> bool {
        let is_main: BOOL = unsafe { msg_send![class!(NSThread), isMainThread] };
        is_main == objc::runtime::YES
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;

        let queue_priority = match priority {
            Priority::RealtimeAudio => {
                panic!("RealtimeAudio priority should use spawn_realtime, not dispatch")
            }
            Priority::High => DispatchQueueGlobalPriority::High,
            Priority::Medium => DispatchQueueGlobalPriority::Default,
            Priority::Low => DispatchQueueGlobalPriority::Low,
        };

        unsafe {
            DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(queue_priority))
                .exec_async_f(context, trampoline);
        }
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;
        unsafe {
            DispatchQueue::main().exec_async_f(context, trampoline);
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let context = runnable.into_raw().as_ptr() as *mut c_void;
        let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(
            DispatchQueueGlobalPriority::High,
        ));
        let when = DispatchTime::NOW.time(duration.as_nanos() as i64);
        unsafe {
            DispatchQueue::exec_after_f(when, &queue, context, trampoline);
        }
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        // iOS has no real-time audio thread API equivalent to macOS mach thread
        // policies. We just spawn a regular high-priority thread.
        std::thread::Builder::new()
            .name("gpui-realtime".into())
            .spawn(move || f())
            .expect("failed to spawn realtime thread");
    }
}

extern "C" fn trampoline(context: *mut c_void) {
    let runnable = unsafe {
        Runnable::<RunnableMeta>::from_raw(NonNull::new_unchecked(context as *mut ()))
    };

    let location = runnable.metadata().location;
    let start = Instant::now();
    let timing = TaskTiming {
        location,
        start,
        end: None,
    };

    THREAD_TIMINGS.with(|timings| {
        let mut timings = timings.lock();
        let timings = &mut timings.timings;
        if let Some(last) = timings.iter_mut().rev().next() {
            if last.location == timing.location {
                return;
            }
        }
        timings.push_back(timing);
    });

    runnable.run();
    let end = Instant::now();

    THREAD_TIMINGS.with(|timings| {
        let mut timings = timings.lock();
        let timings = &mut timings.timings;
        if let Some(last) = timings.iter_mut().rev().next() {
            last.end = Some(end);
        }
    });
}

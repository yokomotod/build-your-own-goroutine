use corosensei::{Coroutine, CoroutineResult, Yielder};
use std::cell::RefCell;
use std::collections::VecDeque;

thread_local! {
    static CURRENT_YIELDER: RefCell<Option<*const Yielder<(), ()>>> = const { RefCell::new(None) };
}

type Task = Coroutine<(), (), ()>;

pub struct Runtime {
    tasks: RefCell<VecDeque<Task>>,
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            tasks: RefCell::new(VecDeque::new()),
        }
    }

    pub fn go<F>(&self, f: F)
    where
        F: FnOnce() + 'static,
    {
        let task = Coroutine::new(move |yielder, ()| {
            CURRENT_YIELDER.with(|cy| {
                *cy.borrow_mut() = Some(yielder as *const _);
            });
            f();
        });
        self.tasks.borrow_mut().push_back(task);
    }

    pub fn run(&self) {
        loop {
            let task = self.tasks.borrow_mut().pop_front();
            match task {
                Some(mut task) => {
                    match task.resume(()) {
                        CoroutineResult::Yield(()) => {
                            self.tasks.borrow_mut().push_back(task);
                        }
                        CoroutineResult::Return(()) => {
                            // Task completed
                        }
                    }
                }
                None => break,
            }
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

pub fn gosched() {
    CURRENT_YIELDER.with(|cy| {
        let yielder_ptr = cy.borrow().expect("gosched called outside of runtime");
        unsafe {
            (*yielder_ptr).suspend(());
        }
    });
}

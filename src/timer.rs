use std::collections::BTreeMap;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

static SET: Lazy<Mutex<BTreeMap<(Instant, usize), Waker>>> = Lazy::new(|| Mutex::default());

fn poll() {
    let ready = {
        let mut set = SET.lock().unwrap();
        let pending = set.split_off(&(Instant::now(), 0));
        mem::replace(&mut *set, pending)
    };

    for (_, waker) in ready {
        waker.wake();
    }
}

pub struct Timer {
    when: Instant,
    inserted: bool,
}

impl Timer {
    pub fn after(dur: Duration) -> Timer {
        Timer {
            when: Instant::now() + dur,
            inserted: false,
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if self.inserted {
            let id = self as *mut Timer as usize;
            SET.lock().unwrap().remove(&(self.when, id));
        }
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let id = &mut *self as *mut Timer as usize;

        if Instant::now() >= self.when {
            SET.lock().unwrap().remove(&(self.when, id));
            Poll::Ready(())
        } else {
            if !self.inserted {
                self.inserted = true;
                let waker = cx.waker().clone();
                SET.lock().unwrap().insert((self.when, id), waker);
                todo!("notify timer");
            }
            Poll::Pending
        }
    }
}

// mentat addition: marking must not recurse once per `Gc` edge.
//
// Upstream rust-gc v0.5.1 marked recursively, so collecting while a long chain
// of `Gc`s was alive overflowed the stack. These run on a small thread so an
// unoptimized build would have overflowed at a few thousand links.

use gc::{force_collect, Finalize, Gc, GcCell, Trace};

#[derive(Trace, Finalize)]
enum List {
    Nil,
    Cons(u64, Gc<List>),
}

fn on_small_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("overflowed the stack or panicked");
}

fn chain(n: u64) -> Gc<List> {
    let mut l = Gc::new(List::Nil);
    for i in 0..n {
        l = Gc::new(List::Cons(i, l));
    }
    l
}

fn len(mut l: &Gc<List>) -> u64 {
    let mut n = 0;
    while let List::Cons(_, next) = &**l {
        n += 1;
        l = next;
    }
    n
}

#[test]
fn collect_with_a_long_live_chain() {
    on_small_stack(|| {
        let l = chain(1_000_000);
        force_collect();
        assert_eq!(len(&l), 1_000_000);
    });
}

#[test]
fn collect_a_long_dead_chain() {
    on_small_stack(|| {
        drop(chain(1_000_000));
        force_collect();
    });
}

#[test]
fn long_chain_through_gccells() {
    #[derive(Trace, Finalize)]
    struct Node {
        next: GcCell<Option<Gc<Node>>>,
    }
    on_small_stack(|| {
        let head = Gc::new(Node { next: GcCell::new(None) });
        let mut tail = head.clone();
        for _ in 0..500_000 {
            let n = Gc::new(Node { next: GcCell::new(None) });
            *tail.next.borrow_mut() = Some(n.clone());
            tail = n;
        }
        force_collect();
        let mut count = 0;
        let mut cur = Some(head.clone());
        while let Some(n) = cur {
            count += 1;
            cur = n.next.borrow().clone();
        }
        assert_eq!(count, 500_001);
    });
}

#[test]
fn cycles_are_still_collected() {
    #[derive(Trace, Finalize)]
    struct Node {
        other: GcCell<Option<Gc<Node>>>,
    }
    // A finalizer counts how many cycle members were freed.
    thread_local!(static FREED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
    #[derive(Trace)]
    struct Counted {
        other: GcCell<Option<Gc<Counted>>>,
    }
    impl Finalize for Counted {
        fn finalize(&self) {
            FREED.with(|f| f.set(f.get() + 1));
        }
    }
    let _ = Node { other: GcCell::new(None) };
    on_small_stack(|| {
        {
            let a = Gc::new(Counted { other: GcCell::new(None) });
            let b = Gc::new(Counted { other: GcCell::new(Some(a.clone())) });
            *a.other.borrow_mut() = Some(b);
        }
        force_collect();
        assert_eq!(FREED.with(|f| f.get()), 2);
    });
}
